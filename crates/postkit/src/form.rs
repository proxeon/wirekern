/// `application/x-www-form-urlencoded` encoding — one implementation for
/// the whole crate. `oauth.rs` (RFC 6749 token exchange) and the Threads
/// connector (Graph form posts) used to carry byte-identical private
/// copies; duplicates like that drift — a fix to the `' '` → `+` vs `%20`
/// subtlety or the unreserved set applied to one silently misses the
/// other. Form encoding is a shared primitive, not an OAuth hop, so it
/// lives here rather than under `oauth`.
///
/// Unreserved set per RFC 3986 §2.3: `ALPHA / DIGIT / "-" / "_" / "." /
/// "~"`; space becomes `+` (form rule, not `%20`); everything else is
/// `%XX` uppercase hex.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push('&');
        }
        s.push_str(&form_encode(k));
        s.push('=');
        s.push_str(&form_encode(v));
    }
    s
}

pub(crate) fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encodes_pairs_and_specials() {
        assert_eq!(form(&[]), "");
        assert_eq!(form(&[("a", "1"), ("b", "2")]), "a=1&b=2");
        // space is + (the form rule), not %20
        assert_eq!(form(&[("text", "hello world")]), "text=hello+world");
        // unreserved pass through untouched
        assert_eq!(form_encode("aZ09-_.~"), "aZ09-_.~");
        // reserved and non-ASCII escape as uppercase hex
        assert_eq!(form_encode("a&=b/c"), "a%26%3Db%2Fc");
        assert_eq!(form_encode("é"), "%C3%A9");
        // the shape oauth and Graph both rely on
        assert_eq!(
            form(&[
                ("grant_type", "authorization_code"),
                ("redirect_uri", "https://localhost/callback")
            ]),
            "grant_type=authorization_code&redirect_uri=https%3A%2F%2Flocalhost%2Fcallback"
        );
    }
}
