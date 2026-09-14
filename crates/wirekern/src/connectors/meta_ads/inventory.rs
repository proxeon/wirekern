//! Account-scoped inventory list and spend-shaped inspect.
use crate::ads::{
    AdsInspectReply, AdsInspectRequest, AdsInventoryItem, AdsInventoryKind, AdsInventoryReply,
    AdsTargetingReadback,
};
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{Deadline, Site};
use serde_json::Value;

use super::graph::{nonempty_value_string, read_json, MAX_PAGES};
use super::SITE;

/// Meta's documented campaign default already omits archived/deleted. The
/// example `["ACTIVE","PAUSED"]` would hide paused drafts still in
/// `IN_PROCESS` / `WITH_ISSUES`. Ad set and ad edges do not promise that
/// same default, so inventory always sends this explicit live set.
pub(super) const LIVE_EFFECTIVE_STATUS: &str = "[\"ACTIVE\",\"PAUSED\",\"IN_PROCESS\",\"WITH_ISSUES\",\"PENDING_REVIEW\",\"DISAPPROVED\",\"PREAPPROVED\",\"PENDING_BILLING_INFO\",\"CAMPAIGN_PAUSED\",\"ADSET_PAUSED\"]";
const INVENTORY_PAGE_LIMIT: &str = "25";

/// Page one account-scoped inventory edge. Follows only Meta's opaque
/// `paging.next`, caps at `MAX_PAGES`, then sorts by id so CLI/MCP order
/// does not follow cursor arrival.
pub(super) async fn list_ads_inventory(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    kind: AdsInventoryKind,
    deadline: Deadline,
) -> Result<AdsInventoryReply, Error> {
    let fields = inventory_list_fields(kind);
    let mut pairs = vec![
        ("fields", fields),
        ("limit", INVENTORY_PAGE_LIMIT),
        ("access_token", token),
    ];
    // Creatives: the adcreatives edge documents no parameters. Filter
    // DELETED locally after the GET instead of sending `effective_status`.
    if kind != AdsInventoryKind::Creative {
        pairs.push(("effective_status", LIVE_EFFECTIVE_STATUS));
    }
    let q = form(&pairs);
    let mut next = Some(format!("{base}/act_{account}/{}?{q}", kind.graph_edge()));
    let mut pages = 0usize;
    let mut items = Vec::new();
    while let Some(url) = next {
        deadline.check(site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("ads inventory paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, site).await?;
        let body = read_json(resp, site).await?;
        if let Some(data) = body.get("data").and_then(|data| data.as_array()) {
            for object in data {
                if let Some(item) = inventory_item_from(kind, object)? {
                    items.push(item);
                }
            }
        }
        next = body
            .get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(|next| next.as_str())
            .map(str::to_owned);
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(AdsInventoryReply {
        site: site.clone(),
        account_id: format!("act_{account}"),
        kind,
        items,
    })
}

pub(super) fn inventory_list_fields(kind: AdsInventoryKind) -> &'static str {
    match kind {
        AdsInventoryKind::Campaign => "id,name,configured_status,effective_status,objective",
        AdsInventoryKind::Adset => "id,name,campaign_id,configured_status,effective_status",
        AdsInventoryKind::Ad => "id,name,adset_id,campaign_id,configured_status,effective_status",
        AdsInventoryKind::Creative => "id,name,status,object_type",
    }
}

pub(super) fn inventory_item_from(
    kind: AdsInventoryKind,
    value: &Value,
) -> Result<Option<AdsInventoryItem>, Error> {
    let id = nonempty_value_string(value.get("id")).ok_or_else(|| Error::Platform {
        site: Site::new(SITE),
        code: "missing_inventory_id".into(),
        message: "ads inventory object returned no id".into(),
    })?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Platform {
            site: Site::new(SITE),
            code: "bad_inventory_id".into(),
            message: "ads inventory object returned a non-numeric id".into(),
        });
    }
    if kind == AdsInventoryKind::Creative {
        let status = nonempty_value_string(value.get("status"));
        // The creative edge has no status filter. A deleted library entry
        // is not inventory of something an operator could later activate.
        if status.as_deref() == Some("DELETED") {
            return Ok(None);
        }
        return Ok(Some(AdsInventoryItem {
            id,
            name: nonempty_value_string(value.get("name")),
            configured_status: None,
            effective_status: None,
            status,
            campaign_id: None,
            adset_id: None,
            objective: None,
            object_type: nonempty_value_string(value.get("object_type")),
        }));
    }
    Ok(Some(AdsInventoryItem {
        id,
        name: nonempty_value_string(value.get("name")),
        configured_status: nonempty_value_string(value.get("configured_status")),
        effective_status: nonempty_value_string(value.get("effective_status")),
        status: None,
        campaign_id: nonempty_value_string(value.get("campaign_id")),
        adset_id: nonempty_value_string(value.get("adset_id")),
        objective: nonempty_value_string(value.get("objective")),
        object_type: None,
    }))
}

/// Read one object's spend-shaped fields. Campaign/ad set carry budget and
/// bid; targeting and destination live on the ad set (and sometimes the ad);
/// Page and click destination live on the creative.
pub(super) async fn inspect_ads_object(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsInspectRequest,
    deadline: Deadline,
) -> Result<AdsInspectReply, Error> {
    let params = form(&[
        ("fields", inspect_fields(request.kind)),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}?{params}", request.id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    inspect_reply_from(site, request, &response)
}

pub(super) fn inspect_fields(kind: AdsInventoryKind) -> &'static str {
    match kind {
        AdsInventoryKind::Campaign => {
            "id,name,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,objective"
        }
        AdsInventoryKind::Adset => {
            "id,name,campaign_id,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,bid_amount,bid_constraints,targeting,promoted_object,destination_type"
        }
        AdsInventoryKind::Ad => {
            "id,name,adset_id,campaign_id,configured_status,effective_status,targeting,creative{id,name,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec}"
        }
        AdsInventoryKind::Creative => {
            "id,name,status,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec"
        }
    }
}

pub(super) fn inspect_reply_from(
    site: &Site,
    request: &AdsInspectRequest,
    value: &Value,
) -> Result<AdsInspectReply, Error> {
    let creative = value.get("creative");
    let story = value
        .get("object_story_spec")
        .or_else(|| creative.and_then(|creative| creative.get("object_story_spec")));
    let product_set_id = nonempty_value_string(value.get("product_set_id")).or_else(|| {
        nonempty_value_string(creative.and_then(|creative| creative.get("product_set_id")))
    });
    let destination = destination_from(value, story).or_else(|| product_set_id.clone());
    Ok(AdsInspectReply {
        site: site.clone(),
        kind: request.kind,
        id: request.id.clone(),
        name: nonempty_value_string(value.get("name")),
        configured_status: nonempty_value_string(value.get("configured_status")),
        effective_status: nonempty_value_string(value.get("effective_status")),
        status: nonempty_value_string(value.get("status")),
        daily_budget: nonempty_value_string(value.get("daily_budget")),
        lifetime_budget: nonempty_value_string(value.get("lifetime_budget")),
        bid_strategy: nonempty_value_string(value.get("bid_strategy")),
        bid_amount: nonempty_value_string(value.get("bid_amount")),
        roas_average_floor: nonempty_value_string(
            value
                .get("bid_constraints")
                .and_then(|constraints| constraints.get("roas_average_floor")),
        ),
        targeting: targeting_readback(value.get("targeting")),
        page_id: page_id_from(value, story),
        destination,
        destination_type: nonempty_value_string(value.get("destination_type")),
        call_to_action_type: nonempty_value_string(value.get("call_to_action_type"))
            .or_else(|| {
                nonempty_value_string(
                    creative.and_then(|creative| creative.get("call_to_action_type")),
                )
            })
            .or_else(|| {
                nonempty_value_string(
                    story
                        .and_then(|story| story.get("link_data"))
                        .or_else(|| story.and_then(|story| story.get("video_data")))
                        .or_else(|| story.and_then(|story| story.get("template_data")))
                        .and_then(|data| data.get("call_to_action"))
                        .and_then(|cta| cta.get("type")),
                )
            }),
        product_set_id,
        instagram_user_id: nonempty_value_string(value.get("instagram_user_id"))
            .or_else(|| {
                nonempty_value_string(story.and_then(|story| story.get("instagram_user_id")))
            })
            .or_else(|| {
                nonempty_value_string(
                    creative.and_then(|creative| creative.get("instagram_user_id")),
                )
            }),
        whatsapp_identity_id: whatsapp_identity_id(value)
            .or_else(|| whatsapp_identity_id(creative.unwrap_or(&Value::Null))),
        campaign_id: nonempty_value_string(value.get("campaign_id")),
        adset_id: nonempty_value_string(value.get("adset_id")),
        creative_id: nonempty_value_string(
            value
                .get("creative")
                .and_then(|creative| creative.get("id")),
        ),
        objective: nonempty_value_string(value.get("objective")),
    })
}

pub(super) fn whatsapp_identity_id(value: &Value) -> Option<String> {
    nonempty_value_string(
        value
            .get("wamo_whatsapp_identity_spec")
            .and_then(|spec| spec.get("wamo_whatsapp_identity_id")),
    )
}

pub(super) fn page_id_from(value: &Value, story: Option<&Value>) -> Option<String> {
    nonempty_value_string(story.and_then(|story| story.get("page_id")))
        .or_else(|| nonempty_value_string(value.get("actor_id")))
        .or_else(|| {
            nonempty_value_string(
                value
                    .get("promoted_object")
                    .and_then(|object| object.get("page_id")),
            )
        })
        .or_else(|| {
            nonempty_value_string(
                value
                    .get("creative")
                    .and_then(|creative| creative.get("actor_id")),
            )
        })
}

pub(super) fn destination_from(value: &Value, story: Option<&Value>) -> Option<String> {
    story_destination(story)
        .or_else(|| nonempty_value_string(value.get("link_url")))
        .or_else(|| nonempty_value_string(value.get("object_url")))
        .or_else(|| {
            story_destination(
                value
                    .get("creative")
                    .and_then(|creative| creative.get("object_story_spec")),
            )
            .or_else(|| {
                nonempty_value_string(
                    value
                        .get("creative")
                        .and_then(|creative| creative.get("link_url")),
                )
            })
            .or_else(|| {
                nonempty_value_string(
                    value
                        .get("creative")
                        .and_then(|creative| creative.get("object_url")),
                )
            })
        })
}

pub(super) fn story_destination(story: Option<&Value>) -> Option<String> {
    let story = story?;
    for key in ["link_data", "video_data", "template_data"] {
        let Some(data) = story.get(key) else {
            continue;
        };
        if let Some(link) = nonempty_value_string(data.get("link")) {
            return Some(link);
        }
        let cta_value = data.get("call_to_action").and_then(|cta| cta.get("value"));
        // Website CTAs store `value.link`. Page CTAs store `value.page`.
        // WhatsApp Message stores `value.app_destination`. Get Directions
        // stores `value.geo_link` (or HTTPS/fbgeo in `link`).
        for key in ["link", "geo_link", "page", "app_destination"] {
            if let Some(dest) = nonempty_value_string(cta_value.and_then(|value| value.get(key))) {
                return Some(dest);
            }
        }
    }
    None
}

pub(super) fn targeting_readback(value: Option<&Value>) -> Option<AdsTargetingReadback> {
    let targeting = value?;
    if !targeting.is_object() {
        return None;
    }
    let countries = targeting
        .get("geo_locations")
        .and_then(|geo| geo.get("countries"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| nonempty_value_string(Some(item)))
                .collect()
        })
        .unwrap_or_default();
    let readback = AdsTargetingReadback {
        countries,
        age_min: targeting
            .get("age_min")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok()),
        age_max: targeting
            .get("age_max")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok()),
        publisher_platforms: string_list(targeting.get("publisher_platforms")),
        facebook_positions: string_list(targeting.get("facebook_positions")),
        instagram_positions: string_list(targeting.get("instagram_positions")),
        whatsapp_positions: string_list(targeting.get("whatsapp_positions")),
        user_age_unknown: targeting.get("user_age_unknown").and_then(Value::as_bool),
    };
    if readback.is_empty() {
        None
    } else {
        Some(readback)
    }
}

pub(super) fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| nonempty_value_string(Some(item)))
                .collect()
        })
        .unwrap_or_default()
}
