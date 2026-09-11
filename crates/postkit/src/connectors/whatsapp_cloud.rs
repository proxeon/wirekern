//! WhatsApp Cloud API connector.
//!
//! Cloud API messaging is deliberately not routed through generic social
//! publishing. A reply's context, an approved template, a specific phone
//! number and the later webhook delivery state are all part of the contract.

use crate::error::Error;
use crate::http::Http;
use crate::publisher::{AuthKind, Publisher};
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
use crate::whatsapp::{InboundMessage, InboundMessages, WhatsAppMessage, WhatsAppSendRequest};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

pub const GRAPH_HOST: &str = "graph.facebook.com";
/// Pinning makes a Meta version update a reviewed wire change rather than an
/// incidental dependency upgrade.
pub const GRAPH_VERSION: &str = "v26.0";
pub const SITE: &str = "whatsapp_cloud";
pub const WEBHOOK_SIGNATURE_PREFIX: &str = "sha256=";
pub const MAX_WEBHOOK_BYTES: usize = 1_048_576;

type HmacSha256 = Hmac<Sha256>;

pub struct WhatsAppCloud {
    http: Http,
    site: Site,
    base: String,
}

impl WhatsAppCloud {
    pub fn new() -> Result<Self, Error> {
        Self::with_base(format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"))
    }

    /// Local mock helper. The production constructor remains pinned to the
    /// versioned Graph API base above.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: base.into().trim_end_matches('/').to_string(),
        })
    }

    /// Verify and parse raw `messages` webhook bytes. This is intentionally a
    /// pure adapter for an application's HTTP endpoint: Postkit does not run
    /// a public listener, acknowledge Meta's delivery, or persist an inbox.
    pub fn parse_signed_webhook(
        app: &AppConfig,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<InboundMessages, Error> {
        if raw_body.len() > MAX_WEBHOOK_BYTES {
            return Err(webhook_error("webhook_body_too_large"));
        }
        let secret = webhook_secret(app)?;
        let supplied = decode_signature(signature)
            .ok_or_else(|| webhook_error("webhook_signature_invalid"))?;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .expect("HMAC-SHA256 accepts every key length");
        mac.update(raw_body);
        // `verify_slice` performs a constant-time comparison. Parsing an
        // unsigned payload first would turn this CLI helper into a convenient
        // source of untrusted personal-data output.
        if mac.verify_slice(&supplied).is_err() {
            return Err(webhook_error("webhook_signature_invalid"));
        }

        let body: Value =
            serde_json::from_slice(raw_body).map_err(|_| webhook_error("webhook_json_invalid"))?;
        if body.get("object").and_then(Value::as_str) != Some("whatsapp_business_account") {
            return Err(webhook_error("webhook_object_invalid"));
        }
        let expected_phone_number_id = phone_number_id(app)?;
        let entries = body
            .get("entry")
            .and_then(Value::as_array)
            .ok_or_else(|| webhook_error("webhook_entry_invalid"))?;
        let mut messages = Vec::new();
        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(Value::as_array) else {
                return Err(webhook_error("webhook_changes_invalid"));
            };
            for change in changes {
                // Meta batches statuses and other notifications beside
                // inbound messages. Ignore those without recasting a status
                // callback as a customer message.
                if change.get("field").and_then(Value::as_str) != Some("messages") {
                    continue;
                }
                let value = change
                    .get("value")
                    .and_then(Value::as_object)
                    .ok_or_else(|| webhook_error("webhook_change_invalid"))?;
                let phone_number_id = value
                    .get("metadata")
                    .and_then(|metadata| metadata.get("phone_number_id"))
                    .and_then(value_string)
                    .ok_or_else(|| webhook_error("webhook_phone_number_id_missing"))?;
                // A valid app signature proves Meta sent this callback, not
                // that it belongs to the Postkit-configured sender. Refusing
                // a mismatched number prevents accidental cross-number data
                // handling in a multi-WABA webhook endpoint.
                if phone_number_id != expected_phone_number_id {
                    return Err(webhook_error("webhook_phone_number_mismatch"));
                }
                let Some(inbound) = value.get("messages").and_then(Value::as_array) else {
                    continue; // status-only messages change
                };
                for message in inbound {
                    messages.push(inbound_message(message)?);
                }
            }
        }
        Ok(InboundMessages {
            site: Site::new(SITE),
            messages,
        })
    }
}

#[async_trait]
impl Publisher for WhatsAppCloud {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // `read.webhook_messages` means verified callback parsing, not a
        // fictional remote inbox listing. Cloud API delivers inbound events
        // to the business' configured webhook endpoint.
        &[
            Capability::SendReply,
            Capability::SendTemplate,
            Capability::ReadWebhookMessages,
        ]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::StaticToken
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        // Defend direct library calls too: private messages cannot be made by
        // supplying `post whatsapp_cloud --param to=…` to a generic surface.
        Err(Error::InvalidPost {
            site: self.site.clone(),
            reason: "use_whatsapp_command".into(),
            limit: None,
        })
    }

    async fn send_whatsapp(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        request.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone_number_id}/messages", self.base))
                    .bearer_auth(token)
                    .json(&send_payload(&request.message)),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("id"))
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_message_id".into(),
                message: "WhatsApp accepted the request without a message id".into(),
            })?;
        Ok(Outcome {
            site: self.site.clone(),
            id: Some(id),
            url: None,
            limits: None,
        })
    }

    async fn whoami(&self, app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!(
                        "{}/{phone_number_id}?fields=id,display_phone_number,verified_name",
                        self.base
                    ))
                    .bearer_auth(token),
                Deadline::from_secs(30),
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("id")
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_phone_number_id".into(),
                message: "WhatsApp phone lookup returned no id".into(),
            })?;
        Ok(WhoAmI {
            site: self.site.clone(),
            id,
            handle: body
                .get("verified_name")
                .and_then(value_string)
                .or_else(|| body.get("display_phone_number").and_then(value_string)),
        })
    }
}

/// The exact limited JSON grammar Postkit allows on the message endpoint.
/// Keeping it public makes wire tests and embedding callers inspectable
/// without permitting arbitrary unreviewed JSON components.
pub fn send_payload(message: &WhatsAppMessage) -> Value {
    match message {
        WhatsAppMessage::Reply {
            to,
            reply_to_message_id,
            text,
        } => json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to,
            "context": { "message_id": reply_to_message_id },
            "type": "text",
            // URL previews are an additional remote fetch/rendering effect;
            // v1 keeps replies literal until an operator requests a typed
            // preview policy.
            "text": { "body": text, "preview_url": false },
        }),
        WhatsAppMessage::Template {
            to,
            name,
            language,
            body_parameters,
        } => {
            let mut template = json!({
                "name": name,
                "language": { "code": language },
            });
            if !body_parameters.is_empty() {
                template["components"] = json!([{
                    "type": "body",
                    "parameters": body_parameters.iter().map(|text| json!({
                        "type": "text",
                        "text": text,
                    })).collect::<Vec<_>>(),
                }]);
            }
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": to,
                "type": "template",
                "template": template,
            })
        }
    }
}

fn phone_number_id(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("phone_number_id")
        .and_then(value_string)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_phone_number_id".into(),
        })
}

fn webhook_secret(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("app_secret")
        .and_then(Value::as_str)
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_webhook_app_secret".into(),
        })
}

fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::BotToken { token } if !token.is_empty() => Ok(token),
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "credential_kind".into(),
        }),
    }
}

fn inbound_message(value: &Value) -> Result<InboundMessage, Error> {
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| webhook_error("webhook_message_id_missing"))?;
    let from = value
        .get("from")
        .and_then(value_string)
        .filter(|from| !from.is_empty())
        .ok_or_else(|| webhook_error("webhook_message_from_missing"))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| webhook_error("webhook_message_type_missing"))?;
    Ok(InboundMessage {
        id,
        from,
        kind,
        timestamp: value.get("timestamp").and_then(value_string),
        text: value
            .get("text")
            .and_then(|text| text.get("body"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        context_message_id: value
            .get("context")
            .and_then(|context| context.get("id"))
            .and_then(value_string),
    })
}

fn value_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn webhook_error(reason: &str) -> Error {
    Error::InvalidQuery {
        site: Site::new(SITE),
        reason: reason.into(),
    }
}

fn decode_signature(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix(WEBHOOK_SIGNATURE_PREFIX)?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let high = hex_value(hex.as_bytes()[index * 2])?;
        let low = hex_value(hex.as_bytes()[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn read_json(response: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = response.status();
    let text = response
        .text()
        .await
        // A reqwest body diagnostic can carry credential-bearing request
        // details, so its content never enters a public Postkit error.
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: "WhatsApp returned invalid JSON".into(),
    })?;
    if value.get("error").is_some() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    Ok(value)
}

fn map_graph_error(status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // Never fall back to an arbitrary raw body: reverse proxies can echo
    // Authorization headers or operator input. Meta's structured message is
    // the only useful and bounded diagnostics channel.
    let message = error
        .and_then(|error| error.get("error_user_msg"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("WhatsApp request failed");
    if code == 190 || status == 401 {
        return Error::Auth {
            site,
            reason: "token_invalid".into(),
        };
    }
    if matches!(code, 4 | 17 | 32 | 613) || status == 429 {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if status >= 500 && code == 0 {
        return Error::Network {
            site,
            message: format!("http_{status}"),
        };
    }
    Error::Platform {
        site,
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[cfg(all(test, not(feature = "oauth")))]
#[test]
fn whatsapp_feature_excludes_oauth() {
    // This compiles only when `whatsapp-cloud` is enabled without `oauth`.
    // The connector must not accidentally pull a browser redirect flow.
    assert_eq!(SITE, "whatsapp_cloud");
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: None,
            extra: json!({
                "phone_number_id": "123456789",
                "app_secret": "webhook-secret",
            }),
        }
    }

    fn creds() -> AccountCreds {
        AccountCreds::BotToken {
            token: "system-user-token".into(),
        }
    }

    fn reply_request() -> WhatsAppSendRequest {
        WhatsAppSendRequest {
            message: WhatsAppMessage::Reply {
                to: "60123456789".into(),
                reply_to_message_id: "wamid.inbound".into(),
                text: "Terima kasih".into(),
            },
            idempotency_key: "reply-1".into(),
        }
    }

    #[tokio::test]
    async fn reply_uses_context_and_returns_only_accepted_wamid() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "context": { "message_id": "wamid.inbound" },
            "type": "text",
            "text": { "body": "Terima kasih", "preview_url": false },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .header("authorization", "Bearer system-user-token")
                .json_body(payload);
            then.status(200).json_body(json!({
                "messaging_product": "whatsapp",
                "messages": [{ "id": "wamid.outbound" }],
            }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &reply_request(), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.outbound"));
        assert!(outcome.url.is_none());
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn template_has_only_approved_name_language_and_body_values() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "type": "template",
            "template": {
                "name": "order_update",
                "language": { "code": "en_US" },
                "components": [{
                    "type": "body",
                    "parameters": [
                        { "type": "text", "text": "A-42" },
                        { "type": "text", "text": "tomorrow" },
                    ],
                }],
            },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.template" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Template {
                to: "60123456789".into(),
                name: "order_update".into(),
                language: "en_US".into(),
                body_parameters: vec!["A-42".into(), "tomorrow".into()],
            },
            idempotency_key: "template-1".into(),
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.template"));
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn malformed_send_refuses_before_http() {
        let server = MockServer::start();
        let send = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let bad = WhatsAppSendRequest {
            message: WhatsAppMessage::Reply {
                to: "+60123456789".into(),
                reply_to_message_id: "wamid.inbound".into(),
                text: "ok".into(),
            },
            idempotency_key: "bad-1".into(),
        };
        let connector = WhatsAppCloud::with_base(server.base_url()).unwrap();
        let error = connector
            .send_whatsapp(&app(), &creds(), &bad, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidPost { reason, .. } if reason == "recipient_must_be_whatsapp_id")
        );
        assert_eq!(send.hits(), 0);
    }

    fn signed(raw: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(b"webhook-secret").unwrap();
        mac.update(raw);
        let bytes = mac.finalize().into_bytes();
        format!(
            "sha256={}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    #[test]
    fn signed_webhook_extracts_only_matching_inbound_messages() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[{
                "from":"60123456789",
                "id":"wamid.inbound",
                "timestamp":"1720000000",
                "type":"text",
                "text":{"body":"Hello"},
                "context":{"id":"wamid.parent"}
              }],
              "statuses":[{"id":"wamid.outbound","status":"delivered"}]
            }
          }]}]
        }"#;
        let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        assert_eq!(reply.messages.len(), 1);
        assert_eq!(reply.messages[0].id, "wamid.inbound");
        assert_eq!(reply.messages[0].text.as_deref(), Some("Hello"));
        assert_eq!(
            reply.messages[0].context_message_id.as_deref(),
            Some("wamid.parent")
        );
    }

    #[test]
    fn webhook_refuses_invalid_signature_and_foreign_phone_without_echoing_body() {
        let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"other"},"messages":[]}}]}]}"#;
        let invalid = WhatsAppCloud::parse_signed_webhook(&app(), "sha256=00", raw).unwrap_err();
        assert!(
            matches!(invalid, Error::InvalidQuery { reason, .. } if reason == "webhook_signature_invalid")
        );

        let foreign = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap_err();
        assert!(
            matches!(foreign, Error::InvalidQuery { ref reason, .. } if reason == "webhook_phone_number_mismatch")
        );
        assert!(!foreign.to_string().contains("other"));
        assert!(!foreign.to_string().contains("webhook-secret"));
    }

    #[test]
    fn webhook_rejects_malformed_json_and_ignores_status_only_changes() {
        let malformed = b"not-json";
        let error =
            WhatsAppCloud::parse_signed_webhook(&app(), &signed(malformed), malformed).unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_json_invalid")
        );

        let status_only = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{"id":"wamid.outbound","status":"read"}]
            }
          }]}]
        }"#;
        let reply =
            WhatsAppCloud::parse_signed_webhook(&app(), &signed(status_only), status_only).unwrap();
        assert!(reply.messages.is_empty());
    }

    #[test]
    fn oversized_webhook_is_refused_before_signature_or_json_work() {
        let oversized = vec![b'x'; MAX_WEBHOOK_BYTES + 1];
        let error =
            WhatsAppCloud::parse_signed_webhook(&app(), "sha256=00", &oversized).unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_body_too_large")
        );
    }

    #[tokio::test]
    async fn whoami_uses_configured_phone_and_static_bearer_token() {
        let server = MockServer::start();
        let lookup = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/123456789")
                .query_param("fields", "id,display_phone_number,verified_name")
                .header("authorization", "Bearer system-user-token");
            then.status(200).json_body(json!({
                "id": "123456789",
                "display_phone_number": "6012 345 6789",
                "verified_name": "Postkit Test",
            }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let who = connector.whoami(&app(), &creds()).await.unwrap();
        assert_eq!(who.id, "123456789");
        assert_eq!(who.handle.as_deref(), Some("Postkit Test"));
        assert_eq!(lookup.hits(), 1);
    }
}
