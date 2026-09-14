use super::graph::{graph_after, graph_page_url};
use super::*;
use crate::error::Error;
use crate::facets::{
    WhatsAppAccount, WhatsAppAssets, WhatsAppFlows, WhatsAppSender, WhatsAppTemplates,
};
use crate::publisher::Publisher;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Site};
use crate::whatsapp::{
    DeliveryStatusKind, RecipientType, WebhookParseOptions, WhatsAppMediaUpload, WhatsAppMessage,
    WhatsAppPageQuery, WhatsAppSendRequest, WhatsAppTemplateQuery,
};
use hmac::{Hmac, Mac};
use httpmock::prelude::*;
use serde_json::json;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn app() -> AppConfig {
    AppConfig {
        site: Site::new(SITE),
        oauth: None,
        extra: json!({
            "phone_number_id": "123456789",
            "waba_id": "102290129340398",
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
            preview_url: false,
        },
        idempotency_key: "reply-1".into(),
        recipient_type: RecipientType::Individual,
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
async fn image_send_uses_media_id_and_optional_caption() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": "60123456789",
        "type": "image",
        "image": { "id": "media-1", "caption": "hi" },
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200)
            .json_body(json!({ "messages": [{ "id": "wamid.img" }] }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::Image {
            to: "60123456789".into(),
            media: crate::whatsapp::MediaRef {
                id: Some("media-1".into()),
                link: None,
            },
            caption: Some("hi".into()),
            reply_to_message_id: None,
        },
        idempotency_key: "img-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let out = connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("wamid.img"));
    assert_eq!(send.hits(), 1);
}

#[test]
fn document_audio_video_sticker_payloads_are_typed() {
    let doc = send_payload(&WhatsAppMessage::Document {
        to: "60123456789".into(),
        media: crate::whatsapp::MediaRef {
            id: None,
            link: Some("https://example.com/a.pdf".into()),
        },
        caption: Some("doc".into()),
        filename: Some("a.pdf".into()),
        reply_to_message_id: None,
    });
    assert_eq!(doc["type"], "document");
    assert_eq!(doc["document"]["link"], "https://example.com/a.pdf");
    assert_eq!(doc["document"]["filename"], "a.pdf");
    let audio = send_payload(&WhatsAppMessage::Audio {
        to: "60123456789".into(),
        media: crate::whatsapp::MediaRef {
            id: Some("a1".into()),
            link: None,
        },
        reply_to_message_id: Some("wamid.in".into()),
    });
    assert_eq!(audio["type"], "audio");
    assert_eq!(audio["context"]["message_id"], "wamid.in");
    assert_eq!(
        send_payload(&WhatsAppMessage::Video {
            to: "60123456789".into(),
            media: crate::whatsapp::MediaRef {
                id: Some("v1".into()),
                link: None,
            },
            caption: None,
            reply_to_message_id: None,
        })["type"],
        "video"
    );
    assert_eq!(
        send_payload(&WhatsAppMessage::Sticker {
            to: "60123456789".into(),
            media: crate::whatsapp::MediaRef {
                id: Some("s1".into()),
                link: None,
            },
            reply_to_message_id: None,
        })["type"],
        "sticker"
    );
}

#[test]
fn interactive_payloads_match_cloud_api() {
    use crate::whatsapp::{ListRow, ListSection, ReplyButton};
    let buttons = send_payload(&WhatsAppMessage::Buttons {
        to: "60123456789".into(),
        body: "Pick".into(),
        buttons: vec![ReplyButton {
            id: "yes".into(),
            title: "Yes".into(),
        }],
        header: None,
        footer: None,
        reply_to_message_id: None,
    });
    assert_eq!(buttons["interactive"]["type"], "button");
    assert_eq!(
        buttons["interactive"]["action"]["buttons"][0]["reply"]["id"],
        "yes"
    );
    let list = send_payload(&WhatsAppMessage::List {
        to: "60123456789".into(),
        body: "Menu".into(),
        button: "Open".into(),
        sections: vec![ListSection {
            title: Some("A".into()),
            rows: vec![ListRow {
                id: "r1".into(),
                title: "One".into(),
                description: None,
            }],
        }],
        header: None,
        footer: None,
        reply_to_message_id: None,
    });
    assert_eq!(list["interactive"]["type"], "list");
    let cta = send_payload(&WhatsAppMessage::CtaUrl {
        to: "60123456789".into(),
        body: "See".into(),
        display_text: "Open".into(),
        url: "https://example.com".into(),
        header: None,
        footer: None,
        reply_to_message_id: None,
    });
    assert_eq!(cta["interactive"]["type"], "cta_url");
    assert_eq!(
        cta["interactive"]["action"]["parameters"]["url"],
        "https://example.com"
    );
    assert_eq!(
        send_payload(&WhatsAppMessage::LocationRequest {
            to: "60123456789".into(),
            body: "Share pin".into(),
            reply_to_message_id: None,
        })["interactive"]["type"],
        "location_request_message"
    );
    assert_eq!(
        send_payload(&WhatsAppMessage::VoiceCall {
            to: "60123456789".into(),
            body: "Call us".into(),
            display_text: Some("Call".into()),
            ttl_minutes: Some(60),
            payload: None,
            reply_to_message_id: None,
        })["interactive"]["type"],
        "voice_call"
    );
}

#[test]
fn location_contacts_address_and_reaction_payloads_match_cloud_api() {
    use crate::whatsapp::OutboundContact;
    let loc = send_payload(&WhatsAppMessage::Location {
        to: "60123456789".into(),
        latitude: "3.139".into(),
        longitude: "101.687".into(),
        name: Some("KLCC".into()),
        address: Some("Kuala Lumpur".into()),
        reply_to_message_id: Some("wamid.in".into()),
    });
    assert_eq!(loc["type"], "location");
    assert_eq!(loc["location"]["latitude"], "3.139");
    assert_eq!(loc["location"]["name"], "KLCC");
    assert_eq!(loc["context"]["message_id"], "wamid.in");
    let contacts = send_payload(&WhatsAppMessage::Contacts {
        to: "60123456789".into(),
        contacts: vec![OutboundContact {
            formatted_name: "Ada".into(),
            phones: vec!["6011".into()],
        }],
        reply_to_message_id: None,
    });
    assert_eq!(contacts["type"], "contacts");
    assert_eq!(contacts["contacts"][0]["name"]["formatted_name"], "Ada");
    assert_eq!(contacts["contacts"][0]["phones"][0]["phone"], "6011");
    let addr = send_payload(&WhatsAppMessage::AddressRequest {
        to: "60123456789".into(),
        body: "Share address".into(),
        country: "my".into(),
        reply_to_message_id: None,
    });
    assert_eq!(addr["interactive"]["type"], "address_message");
    assert_eq!(addr["interactive"]["action"]["parameters"]["country"], "MY");
    let reaction = send_payload(&WhatsAppMessage::Reaction {
        to: "60123456789".into(),
        message_id: "wamid.in".into(),
        emoji: "thumbs".into(),
    });
    assert_eq!(reaction["type"], "reaction");
    assert_eq!(reaction["reaction"]["message_id"], "wamid.in");
    assert_eq!(reaction["reaction"]["emoji"], "thumbs");
}

#[test]
fn mark_read_and_typing_are_status_acks_not_customer_sends() {
    let read = send_payload(&WhatsAppMessage::MarkRead {
        message_id: "wamid.in".into(),
    });
    assert_eq!(read["status"], "read");
    assert_eq!(read["message_id"], "wamid.in");
    assert!(read.get("to").is_none());
    assert!(read.get("recipient_type").is_none());
    let typing = send_payload(&WhatsAppMessage::Typing {
        message_id: "wamid.in".into(),
    });
    assert_eq!(typing["status"], "read");
    assert_eq!(typing["typing_indicator"]["type"], "text");
    let group = send_payload_for(
        &WhatsAppMessage::Text {
            to: "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD".into(),
            text: "hello group".into(),
            preview_url: false,
        },
        RecipientType::Group,
    );
    assert_eq!(group["recipient_type"], "group");
    assert_eq!(
        group["to"],
        "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD"
    );
}

#[tokio::test]
async fn mark_read_records_success_ack_not_a_wamid() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "status": "read",
        "message_id": "wamid.in",
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200).json_body(json!({ "success": true }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::MarkRead {
            message_id: "wamid.in".into(),
        },
        idempotency_key: "read-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let out = connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("wamid.in"));
    assert_eq!(send.hits(), 1);
}

#[tokio::test]
async fn group_text_sets_recipient_type_group() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "group",
        "to": "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD",
        "type": "text",
        "text": { "body": "hello group", "preview_url": false },
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200)
            .json_body(json!({ "messages": [{ "id": "wamid.g" }] }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::Text {
            to: "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD".into(),
            text: "hello group".into(),
            preview_url: false,
        },
        idempotency_key: "group-1".into(),
        recipient_type: RecipientType::Group,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let out = connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("wamid.g"));
    assert_eq!(send.hits(), 1);
}

#[tokio::test]
async fn session_text_has_no_context_object() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": "60123456789",
        "type": "text",
        "text": { "body": "Hello", "preview_url": false },
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200)
            .json_body(json!({ "messages": [{ "id": "wamid.text" }] }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::Text {
            to: "60123456789".into(),
            text: "Hello".into(),
            preview_url: false,
        },
        idempotency_key: "text-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let outcome = connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(outcome.id.as_deref(), Some("wamid.text"));
    assert_eq!(send.hits(), 1);
}

#[tokio::test]
async fn media_upload_get_and_delete_use_phone_scoped_graph_paths() {
    let server = MockServer::start();
    let upload = server.mock(|when, then| {
        when.method(POST).path("/v26.0/123456789/media");
        then.status(200).json_body(json!({ "id": "media-99" }));
    });
    let meta = server.mock(|when, then| {
        when.method(GET).path("/v26.0/media-99");
        then.status(200).json_body(json!({
            "id": "media-99",
            "mime_type": "image/jpeg",
            "url": format!("{}/file.bin", server.base_url()),
            "file_size": 3
        }));
    });
    let file = server.mock(|when, then| {
        when.method(GET).path("/file.bin");
        then.status(200).body("abc");
    });
    let del = server.mock(|when, then| {
        when.method(DELETE).path("/v26.0/media-99");
        then.status(200).json_body(json!({ "success": true }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let up = connector
        .upload_media(
            &app(),
            &creds(),
            &WhatsAppMediaUpload {
                bytes: vec![1, 2, 3],
                mime_type: "image/jpeg".into(),
                filename: "a.jpg".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(up.id, "media-99");
    let got = connector
        .media_metadata(&app(), &creds(), "media-99", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(got.mime_type.as_deref(), Some("image/jpeg"));
    let bytes = connector
        .download_media(&app(), &creds(), "media-99", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(bytes, b"abc");
    connector
        .delete_media(&app(), &creds(), "media-99", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(upload.hits(), 1);
    assert!(meta.hits() >= 1);
    assert_eq!(file.hits(), 1);
    assert_eq!(del.hits(), 1);
}

#[tokio::test]
async fn formatted_recipient_is_normalized_on_the_wire() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": "+60123456789",
        "type": "text",
        "text": { "body": "Hello", "preview_url": false },
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200)
            .json_body(json!({ "messages": [{ "id": "wamid.fmt" }] }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::Text {
            to: "+60 12-345 6789".into(),
            text: "Hello".into(),
            preview_url: false,
        },
        idempotency_key: "fmt-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(send.hits(), 1);
}

#[tokio::test]
async fn preview_url_opt_in_reaches_the_wire() {
    let server = MockServer::start();
    let payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": "60123456789",
        "type": "text",
        "text": { "body": "https://example.com", "preview_url": true },
    });
    let send = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/123456789/messages")
            .json_body(payload);
        then.status(200)
            .json_body(json!({ "messages": [{ "id": "wamid.prev" }] }));
    });
    let request = WhatsAppSendRequest {
        message: WhatsAppMessage::Text {
            to: "60123456789".into(),
            text: "https://example.com".into(),
            preview_url: true,
        },
        idempotency_key: "prev-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
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
            named_body_parameters: vec![],
            header: None,
            buttons: vec![],
            limited_time_offer: None,
        },
        idempotency_key: "template-1".into(),
        recipient_type: RecipientType::Individual,
    };
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let outcome = connector
        .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(outcome.id.as_deref(), Some("wamid.template"));
    assert_eq!(send.hits(), 1);
}

#[test]
fn template_header_named_body_buttons_and_lto_match_cloud_api() {
    use crate::whatsapp::{LimitedTimeOffer, NamedBodyParameter, TemplateButton, TemplateHeader};
    let payload = send_payload(&WhatsAppMessage::Template {
        to: "60123456789".into(),
        name: "fall_sale".into(),
        language: "en_US".into(),
        body_parameters: vec![],
        named_body_parameters: vec![NamedBodyParameter {
            parameter_name: "first_name".into(),
            text: "Ada".into(),
        }],
        header: Some(TemplateHeader::Image {
            media: crate::whatsapp::MediaRef {
                id: Some("media-1".into()),
                link: None,
            },
        }),
        buttons: vec![
            TemplateButton::CopyCode {
                index: 0,
                coupon_code: "SAVE10".into(),
            },
            TemplateButton::Url {
                index: 1,
                text: "promo".into(),
            },
            TemplateButton::PhoneNumber { index: 2 },
        ],
        limited_time_offer: Some(LimitedTimeOffer {
            expiration_time_ms: 1_700_000_000_000,
        }),
    });
    let components = &payload["template"]["components"];
    assert_eq!(components[0]["type"], "header");
    assert_eq!(components[0]["parameters"][0]["type"], "image");
    assert_eq!(
        components[1]["parameters"][0]["parameter_name"],
        "first_name"
    );
    assert_eq!(components[2]["type"], "limited_time_offer");
    assert_eq!(components[3]["sub_type"], "copy_code");
    assert_eq!(components[3]["parameters"][0]["coupon_code"], "SAVE10");
    assert_eq!(components[4]["sub_type"], "url");
    assert_eq!(components[5]["sub_type"], "phone_number");
}

fn sample_draft() -> crate::whatsapp::WhatsAppTemplateDraft {
    use crate::whatsapp::{TemplateCreateButton, TemplateCreateComponent, WhatsAppTemplateDraft};
    WhatsAppTemplateDraft {
        name: "order_update".into(),
        language: "en_US".into(),
        category: "utility".into(),
        parameter_format: crate::whatsapp::ParameterFormat::Positional,
        components: vec![
            TemplateCreateComponent::Header {
                format: "TEXT".into(),
                text: Some("Update".into()),
                example_handle: None,
            },
            TemplateCreateComponent::Body {
                text: "Hi {{1}}, your order is ready.".into(),
                example: vec!["Ada".into()],
                named_example: vec![],
            },
            TemplateCreateComponent::Footer {
                text: "Thanks".into(),
            },
            TemplateCreateComponent::Buttons {
                buttons: vec![TemplateCreateButton::QuickReply { text: "OK".into() }],
            },
        ],
    }
}

#[tokio::test]
async fn template_list_get_create_edit_delete_use_waba_paths() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/102290129340398/message_templates");
        then.status(200).json_body(json!({
            "data": [{
                "id": "920070352646140",
                "name": "order_update",
                "language": "en_US",
                "status": "APPROVED",
                "category": "UTILITY",
                "quality_score": { "score": "GREEN" }
            }]
        }));
    });
    let get = server.mock(|when, then| {
        when.method(GET).path("/v26.0/920070352646140");
        then.status(200).json_body(json!({
            "id": "920070352646140",
            "name": "order_update",
            "status": "APPROVED",
            "quality_score": { "score": "YELLOW" }
        }));
    });
    let create = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/102290129340398/message_templates");
        then.status(200).json_body(json!({
            "id": "111",
            "status": "PENDING",
            "category": "UTILITY"
        }));
    });
    let edit = server.mock(|when, then| {
        when.method(POST).path("/v26.0/111");
        then.status(200).json_body(json!({
            "id": "111",
            "status": "PENDING",
            "category": "UTILITY"
        }));
    });
    let del = server.mock(|when, then| {
        when.method(DELETE)
            .path("/v26.0/102290129340398/message_templates");
        then.status(200).json_body(json!({ "success": true }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let listed = connector
        .list_templates(
            &app(),
            &creds(),
            &crate::whatsapp::WhatsAppTemplateQuery::default(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(listed.templates[0].quality.as_deref(), Some("GREEN"));
    let got = connector
        .get_template(&app(), &creds(), "920070352646140", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(got.quality.as_deref(), Some("YELLOW"));
    let created = connector
        .create_template(&app(), &creds(), &sample_draft(), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(created.status.as_deref(), Some("PENDING"));
    connector
        .edit_template(
            &app(),
            &creds(),
            "111",
            &sample_draft(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    connector
        .delete_template(&app(), &creds(), "order_update", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(list.hits(), 1);
    assert_eq!(get.hits(), 1);
    assert_eq!(create.hits(), 1);
    assert_eq!(edit.hits(), 1);
    assert_eq!(del.hits(), 1);
}

#[test]
fn page_cursor_is_opaque_encoded_and_extracted_without_next_url() {
    let url = graph_page_url(
        "https://graph.example/flows?fields=id".into(),
        &WhatsAppPageQuery {
            limit: Some(25),
            after: Some("cursor+/=&".into()),
        },
    );
    assert_eq!(
        url,
        "https://graph.example/flows?fields=id&limit=25&after=cursor%2B%2F%3D%26"
    );
    assert_eq!(
        graph_after(&json!({
            "paging": {
                "cursors": { "after": "next-page" },
                "next": "https://graph.example/secretly-unrelated"
            }
        })),
        Some("next-page".into())
    );
}

#[tokio::test]
async fn template_list_round_trips_meta_after_cursor() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/102290129340398/message_templates")
            .query_param("after", "old-page");
        then.status(200).json_body(json!({
            "data": [{ "id": "1", "name": "one" }],
            "paging": { "cursors": { "after": "next-page" } }
        }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let page = connector
        .list_templates(
            &app(),
            &creds(),
            &WhatsAppTemplateQuery {
                name: None,
                status: None,
                limit: Some(25),
                after: Some("old-page".into()),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(page.templates[0].id, "1");
    assert_eq!(page.after.as_deref(), Some("next-page"));
    assert_eq!(list.hits(), 1);
}

#[test]
fn catalog_product_order_and_flow_payloads_match_cloud_api() {
    use crate::whatsapp::ProductSection;
    let catalog = send_payload(&WhatsAppMessage::Catalog {
        to: "60123456789".into(),
        body: "See our catalog".into(),
        thumbnail_product_retailer_id: Some("sku-1".into()),
        footer: Some("Shop".into()),
        reply_to_message_id: None,
    });
    assert_eq!(catalog["interactive"]["type"], "catalog_message");
    assert_eq!(
        catalog["interactive"]["action"]["parameters"]["thumbnail_product_retailer_id"],
        "sku-1"
    );
    let product = send_payload(&WhatsAppMessage::Product {
        to: "60123456789".into(),
        catalog_id: "cat-1".into(),
        product_retailer_id: "sku-1".into(),
        body: Some("Nice".into()),
        footer: None,
        reply_to_message_id: None,
    });
    assert_eq!(product["interactive"]["type"], "product");
    let list = send_payload(&WhatsAppMessage::ProductList {
        to: "60123456789".into(),
        catalog_id: "cat-1".into(),
        header: "Items".into(),
        body: "Pick".into(),
        sections: vec![ProductSection {
            title: Some("A".into()),
            product_retailer_ids: vec!["sku-1".into()],
        }],
        footer: None,
        reply_to_message_id: None,
    });
    assert_eq!(list["interactive"]["type"], "product_list");
    let order = send_payload(&WhatsAppMessage::OrderStatus {
        to: "60123456789".into(),
        body: "Update".into(),
        reference_id: "ord-1".into(),
        status: "processing".into(),
        description: None,
        reply_to_message_id: None,
    });
    assert_eq!(order["interactive"]["type"], "order_status");
    assert_eq!(
        order["interactive"]["action"]["parameters"]["order"]["status"],
        "processing"
    );
    let flow = send_payload(&WhatsAppMessage::Flow {
        to: "60123456789".into(),
        body: "Book".into(),
        flow_cta: "Open".into(),
        flow_id: Some("123".into()),
        flow_name: None,
        header: None,
        footer: None,
        flow_token: None,
        screen: Some("WELCOME".into()),
        reply_to_message_id: None,
    });
    assert_eq!(flow["interactive"]["type"], "flow");
    assert_eq!(
        flow["interactive"]["action"]["parameters"]["flow_id"],
        "123"
    );
    assert_eq!(
        flow["interactive"]["action"]["parameters"]["flow_message_version"],
        "3"
    );
}

#[tokio::test]
async fn flows_list_create_and_publish_use_waba_flow_paths() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET).path("/v26.0/102290129340398/flows");
        then.status(200).json_body(json!({
            "data": [{ "id": "123", "name": "booking", "status": "DRAFT", "categories": ["OTHER"] }]
        }));
    });
    let create = server.mock(|when, then| {
        when.method(POST).path("/v26.0/102290129340398/flows");
        then.status(200)
            .json_body(json!({ "id": "123", "status": "DRAFT" }));
    });
    let publish = server.mock(|when, then| {
        when.method(POST).path("/v26.0/123/publish");
        then.status(200).json_body(json!({ "success": true }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let listed = connector
        .list_flows(
            &app(),
            &creds(),
            &crate::whatsapp::WhatsAppPageQuery::default(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(listed.flows[0].status.as_deref(), Some("DRAFT"));
    let draft = crate::whatsapp::WhatsAppFlowDraft {
        name: "booking".into(),
        categories: vec!["OTHER".into()],
        flow_json: r#"{"version":"7.0","screens":[]}"#.into(),
    };
    connector
        .create_flow(&app(), &creds(), &draft, Deadline::from_secs(30))
        .await
        .unwrap();
    let published = connector
        .publish_flow(&app(), &creds(), "123", Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(published.status.as_deref(), Some("PUBLISHED"));
    assert_eq!(list.hits(), 1);
    assert_eq!(create.hits(), 1);
    assert_eq!(publish.hits(), 1);
}

#[tokio::test]
async fn account_list_subscribe_register_and_health_use_waba_paths() {
    let server = MockServer::start();
    let phones = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/102290129340398/phone_numbers");
        then.status(200).json_body(json!({
            "data": [{
                "id": "123456789",
                "display_phone_number": "+60 12",
                "quality_rating": "GREEN",
                "messaging_limit_tier": "TIER_1K"
            }]
        }));
    });
    let health = server.mock(|when, then| {
        when.method(GET).path("/v26.0/123456789");
        then.status(200).json_body(json!({
            "id": "123456789",
            "quality_rating": "YELLOW",
            "messaging_limit_tier": "TIER_250"
        }));
    });
    let sub = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/102290129340398/subscribed_apps");
        then.status(200).json_body(json!({ "success": true }));
    });
    let register = server.mock(|when, then| {
        when.method(POST).path("/v26.0/123456789/register");
        then.status(200).json_body(json!({ "success": true }));
    });
    let pin = server.mock(|when, then| {
        when.method(POST).path("/v26.0/123456789");
        then.status(200).json_body(json!({ "success": true }));
    });
    let waba = server.mock(|when, then| {
        when.method(GET).path("/v26.0/102290129340398");
        then.status(200)
            .json_body(json!({ "id": "102290129340398", "name": "Test" }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let listed = connector
        .list_phone_numbers(
            &app(),
            &creds(),
            &crate::whatsapp::WhatsAppPageQuery::default(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(
        listed.phone_numbers[0].quality_rating.as_deref(),
        Some("GREEN")
    );
    let health_row = connector
        .phone_health(&app(), &creds(), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(health_row.quality_rating.as_deref(), Some("YELLOW"));
    connector
        .subscribe_apps(&app(), &creds(), Deadline::from_secs(30))
        .await
        .unwrap();
    connector
        .register_phone(&app(), &creds(), "123456", Deadline::from_secs(30))
        .await
        .unwrap();
    connector
        .set_two_step_pin(&app(), &creds(), "654321", Deadline::from_secs(30))
        .await
        .unwrap();
    let wabas = connector
        .list_wabas(
            &app(),
            &creds(),
            &crate::whatsapp::WhatsAppPageQuery::default(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(wabas.wabas[0].id, "102290129340398");
    assert_eq!(phones.hits(), 1);
    assert_eq!(health.hits(), 1);
    assert_eq!(sub.hits(), 1);
    assert_eq!(register.hits(), 1);
    assert_eq!(pin.hits(), 1);
    assert_eq!(waba.hits(), 1);
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
            to: "not-a-number".into(),
            reply_to_message_id: "wamid.inbound".into(),
            text: "ok".into(),
            preview_url: false,
        },
        idempotency_key: "bad-1".into(),
        recipient_type: RecipientType::Individual,
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
fn signed_webhook_extracts_inbound_messages_and_delivery_statuses() {
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
              "statuses":[{
                "id":"wamid.outbound",
                "status":"delivered",
                "timestamp":"1720000001",
                "recipient_id":"60123456789",
                "conversation":{"id":"billing-data-not-returned"}
              }]
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
    assert_eq!(reply.statuses.len(), 1);
    assert_eq!(reply.statuses[0].id, "wamid.outbound");
    assert_eq!(reply.statuses[0].status, DeliveryStatusKind::Delivered);
    assert_eq!(reply.statuses[0].timestamp.as_deref(), Some("1720000001"));
    // The status model intentionally does not reproduce recipient or
    // conversation data from the signed payload.
    let wire = serde_json::to_value(&reply).unwrap();
    assert!(wire["statuses"][0].get("recipient_id").is_none());
    assert!(wire["statuses"][0].get("conversation").is_none());
    assert!(reply.messages[0].media.is_none());
}

#[test]
fn signed_webhook_extracts_inbound_media_id_mime_and_caption() {
    let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[{
                "from":"60123456789",
                "id":"wamid.image",
                "type":"image",
                "image":{"id":"media-1","mime_type":"image/jpeg","caption":"photo"}
              }]
            }
          }]}]
        }"#;
    let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
    let media = reply.messages[0].media.as_ref().expect("media");
    assert_eq!(media.id, "media-1");
    assert_eq!(media.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(media.caption.as_deref(), Some("photo"));
    assert!(media.filename.is_none());
}

#[test]
fn signed_webhook_extracts_structured_inbound_fields() {
    let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[
                {"from":"1","id":"wamid.loc","type":"location",
                 "location":{"latitude":3.14,"longitude":101.6,"name":"KL"}},
                {"from":"1","id":"wamid.btn","type":"interactive",
                 "interactive":{"type":"button_reply","button_reply":{"id":"yes","title":"Yes"}}},
                {"from":"1","id":"wamid.rx","type":"reaction",
                 "reaction":{"emoji":"thumbs","message_id":"wamid.parent"}},
                {"from":"1","id":"wamid.un","type":"unsupported",
                 "errors":[{"code":131051,"title":"unsupported"}]}
              ]
            }
          }]}]
        }"#;
    let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
    assert_eq!(
        reply.messages[0].location.as_ref().unwrap().name.as_deref(),
        Some("KL")
    );
    assert_eq!(
        reply.messages[1]
            .interactive
            .as_ref()
            .unwrap()
            .id
            .as_deref(),
        Some("yes")
    );
    assert_eq!(
        reply.messages[2]
            .reaction
            .as_ref()
            .unwrap()
            .emoji
            .as_deref(),
        Some("thumbs")
    );
    assert_eq!(
        reply.messages[3]
            .unsupported
            .as_ref()
            .unwrap()
            .code
            .as_deref(),
        Some("131051")
    );
}

#[test]
fn failed_status_exposes_code_and_title_only() {
    let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{
                "id":"wamid.fail",
                "status":"failed",
                "errors":[{"code":131026,"title":"Message undeliverable","href":"https://example.invalid"}]
              }]
            }
          }]}]
        }"#;
    let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
    assert_eq!(reply.statuses[0].status, DeliveryStatusKind::Failed);
    assert_eq!(reply.statuses[0].errors[0].code.as_deref(), Some("131026"));
    assert_eq!(
        reply.statuses[0].errors[0].title.as_deref(),
        Some("Message undeliverable")
    );
    let wire = serde_json::to_value(&reply).unwrap();
    assert!(wire["statuses"][0]["errors"][0].get("href").is_none());
}

#[test]
fn status_extras_are_off_by_default_and_opt_in() {
    let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{
                "id":"wamid.outbound",
                "status":"delivered",
                "recipient_id":"60123456789",
                "conversation":{"id":"conv-1","origin":{"type":"service"}},
                "pricing":{"billable":false,"pricing_model":"PMP","category":"service"}
              }]
            }
          }]}]
        }"#;
    let hidden = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
    assert!(hidden.statuses[0].recipient_id.is_none());
    assert!(hidden.statuses[0].conversation.is_none());
    assert!(hidden.statuses[0].pricing.is_none());
    let shown = WhatsAppCloud::parse_signed_webhook_with(
        &app(),
        &signed(raw),
        raw,
        WebhookParseOptions {
            include_status_extras: true,
        },
    )
    .unwrap();
    assert_eq!(
        shown.statuses[0].recipient_id.as_deref(),
        Some("60123456789")
    );
    assert_eq!(
        shown.statuses[0]
            .conversation
            .as_ref()
            .unwrap()
            .id
            .as_deref(),
        Some("conv-1")
    );
    assert_eq!(
        shown.statuses[0]
            .pricing
            .as_ref()
            .unwrap()
            .category
            .as_deref(),
        Some("service")
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
fn signed_status_only_webhook_preserves_all_supported_states_in_order() {
    let status_only = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[
                {"id":"wamid.sent","status":"sent","timestamp":"1"},
                {"id":"wamid.delivered","status":"delivered","timestamp":2},
                {"id":"wamid.read","status":"read","timestamp":"3"},
                {"id":"wamid.failed","status":"failed"}
              ]
            }
          }]}]
        }"#;
    let reply =
        WhatsAppCloud::parse_signed_webhook(&app(), &signed(status_only), status_only).unwrap();
    assert!(reply.messages.is_empty());
    assert_eq!(
        reply
            .statuses
            .iter()
            .map(|status| (
                status.id.as_str(),
                &status.status,
                status.timestamp.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("wamid.sent", &DeliveryStatusKind::Sent, Some("1")),
            ("wamid.delivered", &DeliveryStatusKind::Delivered, Some("2")),
            ("wamid.read", &DeliveryStatusKind::Read, Some("3")),
            ("wamid.failed", &DeliveryStatusKind::Failed, None),
        ]
    );
}

#[test]
fn webhook_rejects_malformed_json_and_unmodeled_statuses_without_echoing_them() {
    let malformed = b"not-json";
    let error =
        WhatsAppCloud::parse_signed_webhook(&app(), &signed(malformed), malformed).unwrap_err();
    assert!(
        matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_json_invalid")
    );

    let unsupported = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{"id":"wamid.outbound","status":"deleted"}]
            }
          }]}]
        }"#;
    let error =
        WhatsAppCloud::parse_signed_webhook(&app(), &signed(unsupported), unsupported).unwrap_err();
    assert!(
        matches!(error, Error::InvalidQuery { ref reason, .. } if reason == "webhook_status_unsupported")
    );
    assert!(!error.to_string().contains("deleted"));
}

#[test]
fn webhook_refuses_a_statuses_object_instead_of_an_array() {
    let malformed = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":{"id":"wamid.outbound","status":"sent"}
            }
          }]}]
        }"#;
    let error =
        WhatsAppCloud::parse_signed_webhook(&app(), &signed(malformed), malformed).unwrap_err();
    assert!(
        matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_statuses_invalid")
    );
}

#[test]
fn capabilities_describe_both_verified_webhook_read_surfaces() {
    let connector = WhatsAppCloud::new().unwrap();
    assert!(connector
        .capabilities()
        .contains(&Capability::ReadWebhookMessages));
    assert!(connector
        .capabilities()
        .contains(&Capability::ReadWebhookStatuses));
}

#[test]
fn oversized_webhook_is_refused_before_signature_or_json_work() {
    let oversized = vec![b'x'; MAX_WEBHOOK_BYTES + 1];
    let error = WhatsAppCloud::parse_signed_webhook(&app(), "sha256=00", &oversized).unwrap_err();
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
            "verified_name": "Wirekern Test",
        }));
    });
    let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let who = connector.whoami(&app(), &creds()).await.unwrap();
    assert_eq!(who.id, "123456789");
    assert_eq!(who.handle.as_deref(), Some("Wirekern Test"));
    assert_eq!(lookup.hits(), 1);
}
