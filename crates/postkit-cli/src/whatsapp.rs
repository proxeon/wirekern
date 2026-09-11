//! WhatsApp Cloud CLI: typed sends, configure, signed webhook parse.

use crate::app::fail;
use crate::output::emit_ok;
use postkit::{
    AccountKey, AppConfig, Client, Deadline, Error, InboundMessages, RecipientType, Site,
    WhatsAppSendRequest,
};
use std::io::{self, Read};

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
    app_secret: Option<String>,
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
    Ok(AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        // The app secret is required only to authenticate inbound event
        // parsing. FileAppStore writes config owner-only, and Debug/apps show
        // never render `extra`.
        extra: serde_json::json!({
            "phone_number_id": phone_number_id,
            "app_secret": app_secret,
        }),
    })
}
