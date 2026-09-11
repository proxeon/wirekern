//! `postkit post` — flags compile to Client::publish / probe.

use crate::app::{fail, invalid_post, print_results};
use crate::output::emit_ok;
use postkit::connectors::instagram::MAX_CAROUSEL_IMAGES;
use postkit::{AccountKey, Body, Client, Deadline, Error, Image, Intent, Site};
use std::io::{self, Read};

pub(crate) fn stdin_conflict(
    stdin: bool,
    text: &[String],
    images: &[String],
    alt: &str,
    to: Option<&str>,
    param: &[String],
    site: Option<&str>,
) -> Option<Error> {
    if !stdin {
        return None;
    }
    let clean = text.is_empty()
        && images.is_empty()
        && alt.is_empty()
        && to.is_none()
        && param.is_empty()
        && site.is_none();
    if clean {
        return None;
    }
    Some(Error::InvalidPost {
        site: Site::new(""),
        reason: "stdin_exclusive".into(),
        limit: None,
    })
}

pub(crate) fn dry_run_conflict(
    dry_run: bool,
    idempotency: Option<&str>,
    texts: usize,
) -> Option<Error> {
    if !dry_run {
        return None;
    }
    if idempotency.is_some() {
        return Some(Error::InvalidPost {
            site: Site::new(""),
            reason: "dry_run_idempotency".into(),
            limit: None,
        });
    }
    if texts > 1 {
        return Some(Error::InvalidPost {
            site: Site::new("threads"),
            reason: "dry_run_chain".into(),
            limit: None,
        });
    }
    None
}

/// Site label for pre-parse refusals: the explicit site when given,
/// otherwise the first --to target, otherwise a blank marker. The label is
/// for the operator's eyes in the error, nothing more.
pub(crate) fn site_or_to(site: &Option<String>, to: &Option<String>) -> String {
    site.clone()
        .or_else(|| {
            to.as_ref()
                .map(|t| t.split(',').next().unwrap_or("").trim().to_string())
        })
        .unwrap_or_default()
}

/// Reject a multi-image command shape that Postkit cannot map to one honest
/// carousel. Kept pure so CLI tests prove every refusal occurs before a local
/// image read, credential lookup, or remote container create.
pub(crate) fn image_input_conflict(
    image_count: usize,
    text_count: usize,
    dry_run: bool,
    alt: &str,
    params: &[String],
) -> Option<&'static str> {
    if image_count == 0 {
        return None;
    }
    if text_count > 1 {
        return Some(if image_count > 1 {
            "carousel_caption_multiple"
        } else {
            "image_chain_unsupported"
        });
    }
    if dry_run {
        return Some("dry_run_image_unsupported");
    }
    if image_count > 1 && !alt.is_empty() {
        // A single generic alt string cannot truthfully describe multiple
        // slides. Reject it instead of silently dropping it while Instagram
        // carousel alt text is not a reviewed per-slide wire contract.
        return Some("carousel_alt_unsupported");
    }
    if params
        .iter()
        .any(|param| param.split('=').next().unwrap_or("") == "reply_to_id")
    {
        return Some(if image_count > 1 {
            "carousel_reply_unsupported"
        } else {
            "image_reply_unsupported"
        });
    }
    None
}

/// Build one typed body after the CLI has rejected combinations it cannot
/// represent faithfully. Repeating `--image` is a single carousel body, not
/// a loop that could accidentally create multiple visible posts.
pub(crate) fn build_post_body(
    images: &[String],
    text: Option<String>,
    alt: &str,
    site: &str,
) -> Result<Body, Error> {
    match images {
        [] => Ok(Body::Text {
            text: text.expect("resolve_texts guarantees one when no image exists"),
        }),
        [image] => Ok(Body::Image {
            text,
            image: resolve_image(image, site)?,
            alt: alt.to_string(),
        }),
        images if images.len() > MAX_CAROUSEL_IMAGES => Err(Error::InvalidPost {
            site: Site::new(site),
            reason: "carousel_too_many_images".into(),
            // Check the cardinality before resolving a local filename. A
            // malformed 11-image command should not touch eleven files only
            // to report a platform limit that was already knowable.
            limit: Some(MAX_CAROUSEL_IMAGES as u32),
        }),
        images => Ok(Body::Carousel {
            text,
            images: images
                .iter()
                .map(|image| resolve_image(image, site))
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

/// Resolve --image to the kernel's dual form: an https URL passes through
/// (Threads and Instagram crawl it), anything else is a local file read here
/// — the sole filesystem boundary — into bytes + bare basename. Read errors
/// never echo the operator's path.
pub(crate) fn resolve_image(image: &str, site: &str) -> Result<Image, Error> {
    // Anything scheme-shaped is a URL attempt, not a filename: `http://…`
    // must die as "must be https", never as a confusing unreadable file.
    if image.contains("://") {
        let image = Image::Url(image.to_string());
        image.validate().map_err(|r| invalid_post(site, &r))?;
        return Ok(image);
    }
    let path = std::path::Path::new(image);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid_post(site, "invalid_image_filename"))?;
    let bytes = std::fs::read(path).map_err(|_| invalid_post(site, "image_file_unreadable"))?;
    Ok(Image::Bytes { filename, bytes })
}

pub(crate) fn resolve_texts(text: Vec<String>) -> Result<Vec<String>, i32> {
    if text.is_empty() {
        eprintln!("--text is required (or --stdin)");
        return Err(2);
    }
    if text.iter().any(|t| t == "-") {
        if text.len() != 1 {
            eprintln!("--text - cannot be combined with other --text");
            return Err(2);
        }
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
        return Ok(vec![buf]);
    }
    Ok(text)
}

pub(crate) fn collect_post_sites(site: Option<&str>, to: Option<&str>) -> Result<Vec<String>, i32> {
    if let Some(to) = to {
        let sites: Vec<String> = to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if sites.is_empty() {
            eprintln!("site or --to is required");
            return Err(2);
        }
        Ok(sites)
    } else if let Some(site) = site {
        Ok(vec![site.to_string()])
    } else {
        eprintln!("site or --to is required");
        Err(2)
    }
}

pub(crate) fn chain_blocked_site(sites: &[String]) -> Option<&str> {
    sites.iter().map(String::as_str).find(|s| *s != "threads")
}

pub(crate) fn with_reply_to(params: &serde_json::Value, id: &str) -> serde_json::Value {
    let mut p = params.clone();
    match p {
        serde_json::Value::Object(ref mut m) => {
            m.insert(
                "reply_to_id".into(),
                serde_json::Value::String(id.to_string()),
            );
        }
        _ => {
            p = serde_json::json!({ "reply_to_id": id });
        }
    }
    p
}

pub(crate) async fn chain_threads(
    client: &Client,
    account: &str,
    texts: &[String],
    params: serde_json::Value,
    idempotency: Option<String>,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    let key = AccountKey::new("threads", account);
    let mut results = Vec::new();
    let mut prev: Option<String> = None;
    let mut code = 0i32;
    for (i, text) in texts.iter().enumerate() {
        let p = if let Some(id) = prev.as_deref() {
            with_reply_to(&params, id)
        } else {
            params.clone()
        };
        let intent = Intent {
            site: Site::new("threads"),
            params: p,
            body: Body::Text { text: text.clone() },
            idempotency_key: if i == 0 { idempotency.clone() } else { None },
        };
        match client.publish(&key, intent, deadline).await {
            Ok(o) => {
                prev = o.id.clone();
                if prev.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                    let e = Error::Platform {
                        site: Site::new("threads"),
                        code: "missing_id".into(),
                        message: "Graph create returned no id".into(),
                    };
                    code = e.exit_code();
                    results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                    break;
                }
                results.push(serde_json::to_value(&o).unwrap());
            }
            Err(e) => {
                code = e.exit_code();
                results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                break;
            }
        }
    }
    print_results(&results, json);
    if code != 0 {
        if !json {
            eprintln!("published {} then failed", results.len().saturating_sub(1));
        }
        return Err(code);
    }
    Ok(())
}

/// Probe twin of `one_post`. The human line states the contract in plain
/// words — nothing was published — because a bare `site id` here would
/// read exactly like a successful post.
pub(crate) async fn one_probe(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.probe(key, intent, deadline).await {
        Ok(p) => {
            emit_ok(&p, json, || {
                format!(
                    "{} {} dry-run (nothing published, expires in {}h)",
                    p.site, p.container_id, p.expires_in_hours
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}

pub(crate) async fn one_post(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.publish(key, intent, deadline).await {
        Ok(o) => {
            emit_ok(&o, json, || {
                format!(
                    "{} {}",
                    o.id.as_deref().unwrap_or("-"),
                    o.url.as_deref().unwrap_or("")
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}
