//! `postkit whatsapp` dispatch.
use super::cmd::{
    WhatsAppAccountCmd, WhatsAppCmd, WhatsAppConsentCmd, WhatsAppFlowsCmd, WhatsAppLedgerCmd,
    WhatsAppMediaCmd, WhatsAppTemplatesCmd, WhatsAppWebhookCmd,
};
use super::helpers::*;
use crate::app::{fail, invalid_post};
use crate::output::{emit_raw, human_line};
use postkit::{
    app_source, AccountKey, AppStore, Client, ConsentKind, ConsentRecord, Deadline, Error,
    FileAppStore, Site, WhatsAppFlowDraft, WhatsAppMessage, WhatsAppPageQuery, WhatsAppSendRequest,
    WhatsAppTemplateDraft, WhatsAppTemplateQuery,
};
use std::path::Path;

pub(crate) fn send_allowed(command: &WhatsAppCmd) -> bool {
    matches!(
        command,
        WhatsAppCmd::Reply {
            allow_send: true,
            ..
        } | WhatsAppCmd::Text {
            allow_send: true,
            ..
        } | WhatsAppCmd::Template {
            allow_send: true,
            ..
        } | WhatsAppCmd::Send {
            allow_send: true,
            ..
        } | WhatsAppCmd::SendBatch {
            allow_send: true,
            ..
        } | WhatsAppCmd::Account(WhatsAppAccountCmd::RegisterPhone { yes: true, .. })
            | WhatsAppCmd::Account(WhatsAppAccountCmd::SetTwoStepPin { yes: true, .. })
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn configure(
    phone_number_id: String,
    waba_id: Option<String>,
    business_id: Option<String>,
    app_secret: Option<String>,
    verify_token: Option<String>,
    senders: Vec<String>,
    home: &Path,
    json: bool,
) -> Result<(), i32> {
    let configured_senders = parse_whatsapp_senders(&senders, json)?;
    let cfg = whatsapp_app_config(
        phone_number_id,
        waba_id,
        business_id,
        app_secret,
        verify_token,
        configured_senders,
    )
    .map_err(|e| fail(&e, json))?;
    let apps = FileAppStore::new(home).map_err(|e| fail(&e, json))?;
    apps.put(&cfg).map_err(|e| fail(&e, json))?;
    if app_source(&Site::new("whatsapp_cloud")) == "env" {
        eprintln!(
            "note: WhatsApp environment values override only their matching fields; an unset app secret remains available from apps/whatsapp_cloud.json"
        );
    }
    let phone_number_id = cfg.extra["phone_number_id"]
        .as_str()
        .expect("validated phone id");
    if json {
        emit_raw(&serde_json::json!({
            "site": "whatsapp_cloud",
            "phone_number_id": phone_number_id,
            "sender_aliases": cfg.extra["senders"].as_array().map(|items| items.iter().filter_map(|item| item.get("alias").and_then(|value| value.as_str())).collect::<Vec<_>>()).unwrap_or_default(),
            "webhook_signing": cfg.extra["app_secret"].is_string(),
        }));
    } else {
        eprintln!(
            "configured whatsapp_cloud sender {phone_number_id}; webhook signing={}",
            cfg.extra["app_secret"].is_string()
        );
    }
    Ok(())
}

pub(crate) async fn dispatch(
    client: Client,
    cmd: WhatsAppCmd,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        WhatsAppCmd::Text {
            to,
            text,
            preview_url,
            idempotency,
            recipient_type,
            ..
        } => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Text {
                    to,
                    text,
                    preview_url,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        WhatsAppCmd::Reply {
            to,
            reply_to_message_id,
            text,
            preview_url,
            idempotency,
            recipient_type,
            ..
        } => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Reply {
                    to,
                    reply_to_message_id,
                    text,
                    preview_url,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        WhatsAppCmd::Template {
            to,
            name,
            language,
            body_parameters,
            idempotency,
            recipient_type,
            ..
        } => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Template {
                    to,
                    name,
                    language,
                    body_parameters,
                    named_body_parameters: vec![],
                    header: None,
                    buttons: vec![],
                    limited_time_offer: None,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        WhatsAppCmd::Send {
            request, sender, ..
        } => {
            let request: WhatsAppSendRequest = read_whatsapp_json(&request, json)?;
            one_whatsapp_send_from(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                sender.as_deref(),
                request,
                deadline,
                json,
            )
            .await
        }
        WhatsAppCmd::SendBatch {
            requests, sender, ..
        } => {
            let requests: Vec<WhatsAppSendRequest> = read_whatsapp_json(&requests, json)?;
            let key = AccountKey::new("whatsapp_cloud", &account);
            match client
                .send_whatsapp_many_from(&key, sender.as_deref(), requests, deadline)
                .await
            {
                Ok(outcomes) => {
                    if json {
                        emit_raw(&serde_json::json!({ "outcomes": outcomes }));
                    } else {
                        human_line(format!(
                            "whatsapp_cloud {} messages accepted by Meta; delivery statuses arrive via webhook",
                            outcomes.len()
                        ));
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
        WhatsAppCmd::Media(command) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppMediaCmd::Upload { file, mime_type } => {
                    let bytes = std::fs::read(&file).map_err(|_| {
                        fail(
                            &Error::InvalidQuery {
                                site: Site::new("whatsapp_cloud"),
                                reason: "whatsapp_media_unreadable".into(),
                            },
                            json,
                        )
                    })?;
                    let filename = file
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            fail(
                                &Error::InvalidQuery {
                                    site: Site::new("whatsapp_cloud"),
                                    reason: "whatsapp_media_filename_invalid".into(),
                                },
                                json,
                            )
                        })?
                        .to_string();
                    match client
                        .upload_whatsapp_media(
                            &key,
                            postkit::WhatsAppMediaUpload {
                                bytes,
                                mime_type,
                                filename,
                            },
                            deadline,
                        )
                        .await
                    {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "media upload");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppMediaCmd::Metadata { media_id } => match client
                    .whatsapp_media_metadata(&key, &media_id, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "media metadata read");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppMediaCmd::Download { media_id, output } => {
                    match client
                        .download_whatsapp_media(&key, &media_id, deadline)
                        .await
                    {
                        Ok(bytes) => {
                            write_whatsapp_download(&output, &bytes, json)?;
                            if json {
                                emit_raw(&serde_json::json!({
                                    "downloaded": true,
                                    "bytes": bytes.len(),
                                }));
                            } else {
                                human_line("whatsapp_cloud media download completed");
                            }
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppMediaCmd::Delete { media_id, yes } => {
                    require_whatsapp_yes(yes, "delete WhatsApp media", json)?;
                    match client
                        .delete_whatsapp_media(&key, &media_id, deadline)
                        .await
                    {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "deleted": true }),
                                json,
                                "media delete",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        WhatsAppCmd::Templates(command) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppTemplatesCmd::List {
                    name,
                    status,
                    limit,
                    after,
                } => match client
                    .list_whatsapp_templates(
                        &key,
                        WhatsAppTemplateQuery {
                            name,
                            status,
                            limit,
                            after,
                        },
                        deadline,
                    )
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "template list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppTemplatesCmd::Get { template_id } => match client
                    .get_whatsapp_template(&key, &template_id, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "template read");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppTemplatesCmd::Create { draft, yes } => {
                    require_whatsapp_yes(yes, "submit a template for Meta review", json)?;
                    let draft: WhatsAppTemplateDraft = read_whatsapp_json(&draft, json)?;
                    match client.create_whatsapp_template(&key, draft, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "template create");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppTemplatesCmd::Edit {
                    template_id,
                    draft,
                    yes,
                } => {
                    require_whatsapp_yes(yes, "edit a WhatsApp template", json)?;
                    let draft: WhatsAppTemplateDraft = read_whatsapp_json(&draft, json)?;
                    match client
                        .edit_whatsapp_template(&key, &template_id, draft, deadline)
                        .await
                    {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "template edit");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppTemplatesCmd::Delete { name, yes } => {
                    require_whatsapp_yes(yes, "delete a WhatsApp template", json)?;
                    match client.delete_whatsapp_template(&key, &name, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "deleted": true }),
                                json,
                                "template delete",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        WhatsAppCmd::Flows(command) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppFlowsCmd::List { limit, after } => match client
                    .list_whatsapp_flows(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "Flow list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppFlowsCmd::Get { flow_id } => {
                    match client.get_whatsapp_flow(&key, &flow_id, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow read");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppFlowsCmd::Create { draft, yes } => {
                    require_whatsapp_yes(yes, "create a WhatsApp Flow", json)?;
                    let draft: WhatsAppFlowDraft = read_whatsapp_json(&draft, json)?;
                    match client.create_whatsapp_flow(&key, draft, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow create");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppFlowsCmd::Publish { flow_id, yes } => {
                    require_whatsapp_yes(yes, "publish a WhatsApp Flow", json)?;
                    match client.publish_whatsapp_flow(&key, &flow_id, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow publish");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        WhatsAppCmd::Account(command) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppAccountCmd::Wabas { limit, after } => match client
                    .list_whatsapp_wabas(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "WABA list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::PhoneNumbers { limit, after } => match client
                    .list_whatsapp_phone_numbers(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "phone-number list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::PhoneHealth => {
                    match client.whatsapp_phone_health(&key, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "phone health read");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::SystemUsers { limit, after } => match client
                    .list_whatsapp_system_users(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "system-user list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::SubscribeApps { yes } => {
                    require_whatsapp_yes(yes, "subscribe the app to WhatsApp webhooks", json)?;
                    match client.subscribe_whatsapp_apps(&key, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "subscribed": true }),
                                json,
                                "app subscription",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::RegisterPhone { pin, yes } => {
                    require_whatsapp_yes(yes, "register the WhatsApp phone", json)?;
                    match client.register_whatsapp_phone(&key, &pin, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "registered": true }),
                                json,
                                "phone registration",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::SetTwoStepPin { pin, yes } => {
                    require_whatsapp_yes(yes, "set the WhatsApp two-step PIN", json)?;
                    match client.set_whatsapp_two_step_pin(&key, &pin, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "updated": true }),
                                json,
                                "two-step PIN update",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        WhatsAppCmd::Ledger(command) => match command {
            WhatsAppLedgerCmd::Get { wamid } => match client.whatsapp_ledger_get(&wamid) {
                Ok(reply) => {
                    emit_whatsapp_value(&reply, json, "ledger read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppLedgerCmd::Window { wa_id } => match client.whatsapp_window_open(&wa_id) {
                Ok(open) => {
                    emit_whatsapp_value(&serde_json::json!({ "open": open }), json, "window read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppLedgerCmd::DeadLetters => match client.list_whatsapp_replay_dead_letters() {
                Ok(reply) => {
                    emit_whatsapp_value(&reply, json, "replayable dead-letter list");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppLedgerCmd::Replay { id, yes } => {
                require_whatsapp_yes(yes, "replay and remove a signed WhatsApp callback", json)?;
                match client
                    .replay_whatsapp_dead_letter(&id, postkit::WebhookParseOptions::default())
                {
                    Ok(reply) => {
                        // Human output remains count-only; callers choosing
                        // --json are explicitly asking for parsed event data.
                        emit_whatsapp_value(&reply, json, "replayed callback");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                }
            }
            WhatsAppLedgerCmd::Purge { before_unix, yes } => {
                require_whatsapp_yes(yes, "purge local WhatsApp ledger records", json)?;
                match client.purge_whatsapp_ledger_before(before_unix) {
                    Ok(removed) => {
                        emit_whatsapp_value(
                            &serde_json::json!({ "removed": removed }),
                            json,
                            "ledger purge",
                        );
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                }
            }
        },
        WhatsAppCmd::Consent(command) => match command {
            WhatsAppConsentCmd::Get { wa_id } => match client.get_whatsapp_consent(&wa_id) {
                Ok(reply) => {
                    emit_whatsapp_value(&reply, json, "consent read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppConsentCmd::Set {
                wa_id,
                kind,
                at_unix,
                yes,
            } => {
                require_whatsapp_yes(yes, "record a WhatsApp consent decision", json)?;
                let kind = match kind.as_str() {
                    "opt_in" => ConsentKind::OptIn,
                    "opt_out" => ConsentKind::OptOut,
                    _ => {
                        return Err(fail(
                            &invalid_post("whatsapp_cloud", "whatsapp_consent_kind_invalid"),
                            json,
                        ))
                    }
                };
                let at = at_unix.unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|value| value.as_secs())
                        .unwrap_or(0)
                });
                match client.put_whatsapp_consent(ConsentRecord { wa_id, kind, at }) {
                    Ok(()) => {
                        emit_whatsapp_value(
                            &serde_json::json!({ "recorded": true }),
                            json,
                            "consent record",
                        );
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                }
            }
        },
        WhatsAppCmd::Webhook(WhatsAppWebhookCmd::Parse {
            signature,
            status_extras,
        }) => {
            let raw = read_whatsapp_webhook_stdin(json)?;
            let reply = client
                .parse_whatsapp_webhook(
                    &signature,
                    &raw,
                    postkit::WebhookParseOptions {
                        include_status_extras: status_extras,
                    },
                )
                .map_err(|error| fail(&error, json))?;
            if json {
                emit_raw(&serde_json::to_value(&reply).expect("webhook reply serializes"));
            } else {
                // Do not echo phone numbers or customer text to a terminal by
                // default. Scripts that explicitly need the PII use --json.
                human_line(whatsapp_webhook_line(&reply));
            }
            Ok(())
        }
        // `Configure` is handled before a Client is constructed because it
        // changes the app configuration that `auth --token` must validate.
        WhatsAppCmd::Configure { .. } => unreachable!("handled in run"),
    }
}
