//! `postkit post` — flags compile to Client::publish / probe.

use crate::app::{fail, invalid_post, parse_params, print_results};
use crate::output::emit_ok;
use postkit::connectors::instagram::MAX_CAROUSEL_IMAGES;
use postkit::connectors::threads::validate_text;
use postkit::{AccountKey, Body, Client, Deadline, Error, Image, Intent, PostRequest, Site};
use std::io::{self, Read};

#[allow(clippy::too_many_arguments)]
pub(crate) fn stdin_conflict(
    stdin: bool,
    text: &[String],
    images: &[String],
    alt: Option<&str>,
    to: Option<&str>,
    param: &[String],
    site: Option<&str>,
    page_id: Option<&str>,
) -> Option<Error> {
    if !stdin {
        return None;
    }
    let clean = text.is_empty()
        && images.is_empty()
        && alt.is_none()
        && to.is_none()
        && param.is_empty()
        && site.is_none()
        && page_id.is_none();
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

/// `--reply-to` is sugar for `--param reply_to_id=…`. Pure validation so
/// tests prove refusals occur before any parsing, local read, or publish.
pub(crate) fn reply_to_conflict(reply_to: Option<&str>, param: &[String]) -> Option<&'static str> {
    let id = reply_to?;
    // Threads degrades an empty reply_to_id to a root post; the operator
    // asked for a reply, so the flag must actually carry one.
    if id.is_empty() {
        return Some("reply_to_empty");
    }
    // Both spellings set one wire field; picking a winner would let a
    // command line publish a different reply than it describes.
    if param
        .iter()
        .any(|p| p.split('=').next().unwrap_or("") == "reply_to_id")
    {
        return Some("reply_to_conflict");
    }
    None
}

/// `--page-id` is sugar for `--param page_id=…`. Same dual-source and empty
/// rules as `--reply-to`.
pub(crate) fn page_id_conflict(page_id: Option<&str>, param: &[String]) -> Option<&'static str> {
    let id = page_id?;
    if id.is_empty() {
        return Some("page_id_empty");
    }
    if param
        .iter()
        .any(|p| p.split('=').next().unwrap_or("") == "page_id")
    {
        return Some("page_id_conflict");
    }
    None
}

/// A Page id is facebook_pages-only and not honest across a fan-out.
pub(crate) fn page_id_target_conflict(
    page_id: Option<&str>,
    sites: &[String],
) -> Option<&'static str> {
    page_id?;
    if sites.len() != 1 {
        return Some("page_id_fanout_unsupported");
    }
    if sites[0] != "facebook_pages" {
        return Some("page_id_site_unsupported");
    }
    None
}

/// One id cannot be honest across a fan-out: threads media ids and bluesky
/// at:// URIs are different namespaces, so the same value cloned to every
/// --to target would publish on one site and fail on the rest.
pub(crate) fn reply_to_fanout_conflict(
    reply_to: Option<&str>,
    sites: &[String],
) -> Option<&'static str> {
    if reply_to.is_none() || sites.len() <= 1 {
        return None;
    }
    Some("reply_to_fanout_unsupported")
}

/// Reject a multi-image command shape that Postkit cannot map to one honest
/// carousel. Kept pure so CLI tests prove every refusal occurs before a local
/// image read, credential lookup, or remote container create.
pub(crate) fn image_input_conflict(
    image_count: usize,
    text_count: usize,
    dry_run: bool,
    alt: Option<&str>,
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
    if image_count > 1 && alt.is_some() {
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    client: &Client,
    site: Option<String>,
    text: Vec<String>,
    to: Option<String>,
    param: Vec<String>,
    reply_to: Option<String>,
    page_id: Option<String>,
    idempotency: Option<String>,
    stdin: bool,
    dry_run: bool,
    image: Vec<String>,
    alt: Option<String>,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    if let Some(e) = dry_run_conflict(dry_run, idempotency.as_deref(), text.len()) {
        return Err(fail(&e, json));
    }
    // --reply-to is sugar for --param reply_to_id=…: refuse the
    // empty and dual-source shapes, then fold it in so every later
    // check — image guard, stdin exclusivity, fan-out, chain
    // anchor — sees exactly one spelling of the intent.
    if let Some(reason) = reply_to_conflict(reply_to.as_deref(), &param) {
        return Err(fail(&invalid_post(&site_or_to(&site, &to), reason), json));
    }
    let mut param = param;
    if let Some(id) = reply_to.as_deref() {
        param.push(format!("reply_to_id={id}"));
    }
    if let Some(reason) = page_id_conflict(page_id.as_deref(), &param) {
        return Err(fail(&invalid_post(&site_or_to(&site, &to), reason), json));
    }
    if let Some(id) = page_id.as_deref() {
        param.push(format!("page_id={id}"));
    }
    // Image exclusions fire before any parsing or I/O: each
    // combination names a wire contract postkit has not verified
    // (015 D4), and half-honoring it is the 022 failure mode.
    if let Some(err) =
        image_input_conflict(image.len(), text.len(), dry_run, alt.as_deref(), &param)
    {
        return Err(fail(&invalid_post(&site_or_to(&site, &to), err), json));
    }
    // 025: --stdin is a complete request in itself; any other
    // content-carrying flag would be silently ignored by the stdin
    // branch — refuse the combination before stdin is even read.
    if let Some(e) = stdin_conflict(
        stdin,
        &text,
        &image,
        alt.as_deref(),
        to.as_deref(),
        &param,
        site.as_deref(),
        page_id.as_deref(),
    ) {
        return Err(fail(&e, json));
    }
    if stdin {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
        let req: PostRequest = serde_json::from_str(&buf).map_err(|e| {
            fail(
                &Error::InvalidPost {
                    site: Site::new(""),
                    reason: format!("json:{e}"),
                    limit: None,
                },
                json,
            )
        })?;
        let (key, mut intent) = req.into_key_intent().map_err(|e| fail(&e, json))?;
        intent.idempotency_key = idempotency;
        if dry_run && matches!(intent.body, Body::Image { .. } | Body::Carousel { .. }) {
            return Err(fail(
                &invalid_post(key.site.as_str(), "dry_run_image_unsupported"),
                json,
            ));
        }
        // --stdin has no dry_run field of its own; the CLI flag is
        // the single switch, so both input paths stay in parity.
        if dry_run {
            return one_probe(client, &key, intent, deadline, json).await;
        }
        return one_post(client, &key, intent, deadline, json).await;
    }
    // With --image the caption is optional (zero or one --text);
    // without it the existing text rules apply unchanged.
    let texts = if !image.is_empty() {
        if text.iter().any(|t| t == "-") {
            return Err(fail(
                &invalid_post(&site_or_to(&site, &to), "image_chain_unsupported"),
                json,
            ));
        }
        text
    } else {
        resolve_texts(text)?
    };
    let params = parse_params(&param, json)?;
    let sites = collect_post_sites(site.as_deref(), to.as_deref())?;
    if let Some(reason) = reply_to_fanout_conflict(reply_to.as_deref(), &sites) {
        return Err(fail(&invalid_post(&site_or_to(&site, &to), reason), json));
    }
    if let Some(reason) = page_id_target_conflict(page_id.as_deref(), &sites) {
        return Err(fail(&invalid_post(&site_or_to(&site, &to), reason), json));
    }
    if texts.len() > 1 {
        if let Some(bad) = chain_blocked_site(&sites) {
            return Err(fail(
                &Error::InvalidPost {
                    site: Site::new(bad),
                    reason: "thread_unsupported".into(),
                    limit: None,
                },
                json,
            ));
        }
        for t in &texts {
            validate_text(t).map_err(|e| fail(&e, json))?;
        }
        return chain_threads(
            client,
            &account,
            &texts,
            params,
            idempotency,
            deadline,
            json,
        )
        .await;
    }
    // Option: Some = caption (image) or the post text; None is
    // only possible with --image and zero --text flags.
    let text = texts.into_iter().next();
    if let Some(to) = to {
        let mut results = Vec::new();
        let mut code = 0i32;
        for raw in to.split(',') {
            let s = raw.trim();
            if s.is_empty() {
                continue;
            }
            let key = AccountKey::new(s, &account);
            // Bytes are cloned per target: each connector gets its
            // own copy and a per-target failure (e.g. a URL image
            // on Bluesky) is isolated in the fan-out results.
            let body = build_post_body(&image, text.clone(), alt.as_deref().unwrap_or(""), s)
                .map_err(|e| fail(&e, json))?;
            let intent = Intent {
                site: Site::new(s),
                params: params.clone(),
                body,
                idempotency_key: idempotency.clone(),
            };
            let attempt = if dry_run {
                client
                    .probe(&key, intent, deadline)
                    .await
                    .map(|p| serde_json::to_value(&p).unwrap())
            } else {
                client
                    .publish(&key, intent, deadline)
                    .await
                    .map(|o| serde_json::to_value(&o).unwrap())
            };
            match attempt {
                Ok(v) => results.push(v),
                Err(e) => {
                    if code == 0 {
                        code = e.exit_code();
                    }
                    results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                }
            }
        }
        print_results(&results, json);
        if code == 0 {
            Ok(())
        } else {
            Err(code)
        }
    } else {
        let site = sites.into_iter().next().expect("collect_post_sites");
        let key = AccountKey::new(&site, &account);
        let body = build_post_body(&image, text, alt.as_deref().unwrap_or(""), &site)
            .map_err(|e| fail(&e, json))?;
        let intent = Intent {
            site: Site::new(&site),
            params,
            body,
            idempotency_key: idempotency,
        };
        if dry_run {
            one_probe(client, &key, intent, deadline, json).await
        } else {
            one_post(client, &key, intent, deadline, json).await
        }
    }
}
