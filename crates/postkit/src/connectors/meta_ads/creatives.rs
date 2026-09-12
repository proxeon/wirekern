//! Link, video, and typed ad creatives plus preview.
use crate::ads::{
    CreateLinkAdCreativeRequest, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
};
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{Deadline, Site};
use serde_json::Value;

use super::graph::{read_json, value_string};

pub(super) fn attach_creative_identity(
    spec: &mut Value,
    instagram_user_id: Option<&str>,
    _advantage_plus: bool,
    _whatsapp_identity: Option<&crate::ads::WhatsAppStatusIdentity>,
) {
    if let Some(id) = instagram_user_id {
        spec["instagram_user_id"] = serde_json::Value::String(id.to_string());
    }
}

pub(super) fn append_creative_extras(
    fields: &mut Vec<(&str, String)>,
    advantage_plus: bool,
    whatsapp_identity: Option<&crate::ads::WhatsAppStatusIdentity>,
) {
    if advantage_plus {
        fields.push((
            "degrees_of_freedom_spec",
            serde_json::json!({
                "creative_features_spec": {
                    "standard_enhancements": { "enroll_status": "OPT_IN" }
                }
            })
            .to_string(),
        ));
    }
    if let Some(ident) = whatsapp_identity {
        fields.push(("wamo_whatsapp_identity_spec", ident.meta_json().to_string()));
    }
}

/// Create an unpublished Page-backed image-link creative. CTA `value` is
/// built from the typed extra fields; video, carousel, and Instagram shapes
/// have their own typed contracts.
pub(super) async fn create_link_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &CreateLinkAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    let creative = &request.creative;
    let mut spec = serde_json::json!({
        "page_id": creative.page_id,
        "link_data": {
            "image_hash": creative.image_hash,
            "link": creative.destination_url,
            "message": creative.message,
            "name": creative.headline,
            "call_to_action": {
                "type": creative.call_to_action.meta_value(),
                "value": crate::ads::link_cta_value_json(creative),
            },
        },
    });
    attach_creative_identity(
        &mut spec,
        creative.instagram_user_id.as_deref(),
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", creative.name.clone()),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    append_creative_extras(
        &mut fields,
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

pub(super) async fn create_video_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::CreateVideoAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    let creative = &request.creative;
    let link_cta = crate::ads::LinkAdCreative {
        name: creative.name.clone(),
        page_id: creative.page_id.clone(),
        image_hash: creative.image_hash.clone(),
        message: creative.message.clone(),
        headline: String::new(),
        destination_url: creative.destination_url.clone(),
        call_to_action: creative.call_to_action,
        geo_link: creative.geo_link.clone(),
        application_id: creative.application_id.clone(),
        app_link: creative.app_link.clone(),
        instagram_user_id: creative.instagram_user_id.clone(),
        advantage_plus: creative.advantage_plus,
        whatsapp_identity: creative.whatsapp_identity.clone(),
    };
    let mut spec = serde_json::json!({
        "page_id": creative.page_id,
        "video_data": {
            "video_id": creative.video_id,
            "image_hash": creative.image_hash,
            "message": creative.message,
            "call_to_action": {
                "type": creative.call_to_action.meta_value(),
                "value": crate::ads::link_cta_value_json(&link_cta),
            },
        },
    });
    attach_creative_identity(
        &mut spec,
        creative.instagram_user_id.as_deref(),
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", creative.name.clone()),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    append_creative_extras(
        &mut fields,
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

pub(super) async fn create_typed_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::CreateAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    use crate::ads::AdCreativeKind;
    let (name, spec, advantage_plus, wa, product_set_id) = match &request.kind {
        AdCreativeKind::Carousel(c) => {
            let cards: Vec<_> = c
                .cards
                .iter()
                .map(|card| {
                    serde_json::json!({
                        "image_hash": card.image_hash,
                        "link": card.link,
                        "name": card.name,
                    })
                })
                .collect();
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "message": c.message,
                    "link": c.cards.first().map(|card| card.link.as_str()).unwrap_or_default(),
                    "child_attachments": cards,
                    "call_to_action": { "type": c.call_to_action.meta_value() },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
        AdCreativeKind::Catalog(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "template_data": {
                    "link": c.link,
                    "message": c.message,
                    "call_to_action": { "type": c.call_to_action.meta_value() },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                Some(c.product_set_id.clone()),
            )
        }
        AdCreativeKind::LeadForm(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "image_hash": c.image_hash,
                    "link": c.destination_url,
                    "message": c.message,
                    "name": c.headline,
                    "call_to_action": {
                        "type": c.call_to_action.meta_value(),
                        "value": {
                            "link": c.destination_url,
                            "lead_gen_form_id": c.lead_gen_form_id,
                        },
                    },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
        AdCreativeKind::AppInstall(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "image_hash": c.image_hash,
                    "link": c.object_store_url,
                    "message": c.message,
                    "call_to_action": {
                        "type": "INSTALL_MOBILE_APP",
                        "value": {
                            "application": c.application_id,
                            "link": c.object_store_url,
                        },
                    },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
    };
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", name),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    if let Some(product_set_id) = product_set_id {
        // Advantage+ catalog ads: product_set_id is a creative field, not
        // nested under template_data.
        fields.push(("product_set_id", product_set_id));
    }
    append_creative_extras(&mut fields, advantage_plus, wa.as_ref());
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

/// Ask Meta to render an already-stored creative in one reviewed placement.
/// This is a Graph read edge, not `generatepreviews`: no campaign, ad set, or
/// final ad is created, and the body is kept opaque until the CLI writes it to
/// the operator-selected preview file.
pub(super) async fn preview_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &CreativePreviewRequest,
    deadline: Deadline,
) -> Result<CreativePreview, Error> {
    // Graph accepts user tokens on this read edge as a query parameter. The
    // shared HTTP layer deliberately redacts request URLs from transport
    // errors, preventing this credential from reaching terminal output.
    let params = form(&[
        ("ad_format", request.ad_format.meta_value()),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}/previews?{params}", request.creative_id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let body = response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| value_string(item.get("body")))
        .filter(|body| !body.trim().is_empty())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_preview_body".into(),
            message: "creative preview returned no body".into(),
        })?;
    Ok(CreativePreview {
        site: site.clone(),
        creative_id: request.creative_id.clone(),
        ad_format: request.ad_format,
        body,
    })
}
