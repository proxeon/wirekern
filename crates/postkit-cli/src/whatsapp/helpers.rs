//! WhatsApp Cloud CLI helpers: typed sends, configure, signed webhook parse.

use crate::app::fail;
use crate::output::{emit_ok, emit_raw, human_line};
use postkit::{
    AccountKey, AppConfig, Client, Deadline, Error, InboundMessages, RecipientType, Site,
    WhatsAppOutboundSender, WhatsAppSendRequest,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

pub(crate) fn parse_recipient_type(raw: &str, json: bool) -> Result<RecipientType, i32> {
    raw.parse().map_err(|reason: String| {
        fail(
            &Error::InvalidPost {
                site: Site::new("whatsapp_cloud"),
                reason,
                limit: None,
            },
            json,
        )
    })
}

pub(crate) async fn one_whatsapp_send(
    client: &Client,
    key: &AccountKey,
    request: WhatsAppSendRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.send_whatsapp(key, request, deadline).await {
        Ok(outcome) => {
            emit_ok(&outcome, json, || {
                format!(
                    "{} {} accepted by Meta; delivery status arrives via webhook",
                    outcome.site,
                    outcome.id.as_deref().unwrap_or("-")
                )
            });
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// The structured send command uses the same closed `WhatsAppSendRequest`
/// schema as the HTTP and library surfaces. It selects an already configured
/// alias rather than accepting an arbitrary Cloud API phone-number ID.
pub(crate) async fn one_whatsapp_send_from(
    client: &Client,
    key: &AccountKey,
    sender: Option<&str>,
    request: WhatsAppSendRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client
        .send_whatsapp_from(key, sender, request, deadline)
        .await
    {
        Ok(outcome) => {
            emit_ok(&outcome, json, || {
                format!(
                    "{} {} accepted by Meta; delivery status arrives via webhook",
                    outcome.site,
                    outcome.id.as_deref().unwrap_or("-")
                )
            });
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// Read a typed operator draft without turning the CLI into an arbitrary
/// Graph JSON pass-through. `-` permits pipe-friendly automation while every
/// other path is regular local input.
pub(crate) fn read_whatsapp_json<T: DeserializeOwned>(path: &Path, json: bool) -> Result<T, i32> {
    let bytes = if path == Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin().lock().read_to_end(&mut bytes).map(|_| bytes)
    } else {
        fs::read(path)
    }
    .map_err(|_| {
        fail(
            &Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_json_unreadable".into(),
            },
            json,
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        fail(
            &Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_json_invalid".into(),
            },
            json,
        )
    })
}

/// Render structured management/read output only when the caller explicitly
/// asks for JSON. The normal terminal line avoids copying contact, WABA, or
/// short-lived media URL data into shell history by default.
pub(crate) fn emit_whatsapp_value<T: Serialize>(value: &T, json: bool, action: &str) {
    if json {
        emit_raw(&serde_json::to_value(value).expect("WhatsApp reply serializes"));
    } else {
        human_line(format!(
            "whatsapp_cloud {action} completed; use --json for structured output"
        ));
    }
}

/// Management writes are not private message sends, but they can still
/// publish a Flow, submit a template, alter registration, or delete media.
/// Make the acknowledgement visible and consistent across every such CLI verb.
pub(crate) fn require_whatsapp_yes(yes: bool, _action: &str, json: bool) -> Result<(), i32> {
    if yes {
        Ok(())
    } else {
        Err(fail(
            &Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "explicit_whatsapp_management_acknowledgement_required".into(),
            },
            json,
        ))
    }
}

/// Write a download exactly once. Media can be sensitive, so refusing an
/// existing destination prevents a typo from overwriting an operator file.
pub(crate) fn write_whatsapp_download(path: &Path, bytes: &[u8], json: bool) -> Result<(), i32> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|_| {
            fail(
                &Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason: "whatsapp_media_output_unwritable".into(),
                },
                json,
            )
        })
}

/// Paused ads have their own result type instead of being rendered as social
/// posts. The status is shown prominently so an operator can verify the
/// safety invariant in scripts and terminal output alike.
pub(crate) fn whatsapp_webhook_line(reply: &InboundMessages) -> String {
    format!(
        "whatsapp_cloud verified webhook: {} inbound message(s), {} delivery status(es)",
        reply.messages.len(),
        reply.statuses.len()
    )
}

/// The parser receives exact raw request bytes, so cap stdin before HMAC or
/// JSON work. This CLI is an adapter tool, not an unbounded webhook server.
pub(crate) fn read_whatsapp_webhook_stdin(json: bool) -> Result<Vec<u8>, i32> {
    const LIMIT: usize = postkit::connectors::whatsapp_cloud::MAX_WEBHOOK_BYTES;
    let mut raw = Vec::new();
    io::stdin()
        .lock()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| {
            fail(
                &Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason: "webhook_body_unreadable".into(),
                },
                json,
            )
        })?;
    if raw.len() > LIMIT {
        return Err(fail(
            &Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "webhook_body_too_large".into(),
            },
            json,
        ));
    }
    Ok(raw)
}

pub(crate) fn whatsapp_app_config(
    phone_number_id: String,
    waba_id: Option<String>,
    business_id: Option<String>,
    app_secret: Option<String>,
    verify_token: Option<String>,
    senders: Vec<WhatsAppOutboundSender>,
) -> Result<AppConfig, Error> {
    if phone_number_id.is_empty()
        || phone_number_id.len() > 32
        || !phone_number_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::InvalidQuery {
            site: Site::new("whatsapp_cloud"),
            reason: "phone_number_id_invalid".into(),
        });
    }
    if app_secret.as_deref().is_some_and(str::is_empty) {
        return Err(Error::InvalidQuery {
            site: Site::new("whatsapp_cloud"),
            reason: "webhook_app_secret_empty".into(),
        });
    }
    if let Some(waba) = waba_id.as_deref() {
        if waba.is_empty() || waba.len() > 32 || !waba.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "waba_id_invalid".into(),
            });
        }
    }
    if let Some(business) = business_id.as_deref() {
        if business.is_empty()
            || business.len() > 32
            || !business.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "business_id_invalid".into(),
            });
        }
    }
    Ok(AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        // The app secret is required only to authenticate inbound event
        // parsing. FileAppStore writes config owner-only, and Debug/apps show
        // never render `extra`.
        extra: serde_json::json!({
            "phone_number_id": phone_number_id,
            "waba_id": waba_id,
            "business_id": business_id,
            "app_secret": app_secret,
            "verify_token": verify_token,
            "senders": senders,
        }),
    })
}

/// Parse repeatable `alias=phone_number_id` flags once, preserving a stable
/// configuration contract for CLI, webhook filtering, and outbound routing.
pub(crate) fn parse_whatsapp_senders(
    raw: &[String],
    json: bool,
) -> Result<Vec<WhatsAppOutboundSender>, i32> {
    let mut aliases = HashSet::new();
    let mut phone_ids = HashSet::new();
    let mut senders = Vec::new();
    for value in raw {
        let Some((alias, phone_number_id)) = value.split_once('=') else {
            return Err(fail(
                &Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason: "whatsapp_sender_expected_alias_eq_phone_number_id".into(),
                },
                json,
            ));
        };
        let sender = WhatsAppOutboundSender {
            alias: alias.to_string(),
            phone_number_id: phone_number_id.to_string(),
        };
        sender.validate().map_err(|reason| {
            fail(
                &Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason,
                },
                json,
            )
        })?;
        if !aliases.insert(sender.alias.clone())
            || !phone_ids.insert(sender.phone_number_id.clone())
        {
            return Err(fail(
                &Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason: "whatsapp_sender_duplicate".into(),
                },
                json,
            ));
        }
        senders.push(sender);
    }
    Ok(senders)
}
