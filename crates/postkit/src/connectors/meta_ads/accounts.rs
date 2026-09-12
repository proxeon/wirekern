//! Ad-account discovery and currency.
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::insights::AdAccount;
use crate::types::{Deadline, Site};
use serde_json::Value;

use super::graph::{read_json, value_string, MAX_PAGES};
use super::SITE;

/// First ad account visible to the token, as `act_<account_id>`. This is kept
/// only for existing auth compatibility; `ads accounts` lets new operators
/// discover IDs and select them explicitly with `insights --ad-account`.
pub(super) async fn first_ad_account(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    Ok(list_ad_accounts(http, base, token, deadline)
        .await?
        .into_iter()
        .next()
        .map(|account| account.id))
}

/// Page through every account visible to the credential. The same deadline
/// and cap as insights prevent account discovery from becoming an unbounded
/// read if Graph returns a malformed cursor cycle.
pub(super) async fn list_ad_accounts(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Vec<AdAccount>, Error> {
    let site = Site::new(SITE);
    let q = form(&[
        (
            "fields",
            "account_id,name,currency,timezone_name,account_status",
        ),
        ("limit", "100"),
        ("access_token", token),
    ]);
    let mut next = Some(format!("{base}/me/adaccounts?{q}"));
    let mut pages = 0usize;
    let mut accounts = Vec::new();
    while let Some(url) = next {
        deadline.check(&site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("ad account paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, &site).await?;
        let body = read_json(resp, &site).await?;
        if let Some(data) = body.get("data").and_then(|data| data.as_array()) {
            for account in data {
                accounts.push(ad_account_from(account)?);
            }
        }
        next = body
            .get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(|next| next.as_str())
            .map(str::to_owned);
    }
    accounts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(accounts)
}

pub(super) fn ad_account_from(value: &Value) -> Result<AdAccount, Error> {
    let raw = value_string(value.get("account_id")).ok_or_else(|| Error::Platform {
        site: Site::new(SITE),
        code: "missing_account_id".into(),
        message: "ad account returned no account_id".into(),
    })?;
    let digits = raw.strip_prefix("act_").unwrap_or(&raw);
    if digits.is_empty() || !digits.chars().all(|digit| digit.is_ascii_digit()) {
        return Err(Error::Platform {
            site: Site::new(SITE),
            code: "bad_account_id".into(),
            message: "ad account returned an invalid account_id".into(),
        });
    }
    Ok(AdAccount {
        id: format!("act_{digits}"),
        name: value_string(value.get("name")),
        currency: value_string(value.get("currency")),
        timezone: value_string(value.get("timezone_name")),
        status: value_string(value.get("account_status")),
    })
}

pub(super) async fn account_currency(
    http: &Http,
    base: &str,
    account: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    let site = Site::new(SITE);
    let q = form(&[("fields", "currency"), ("access_token", token)]);
    let url = format!("{base}/act_{account}?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    Ok(body
        .get("currency")
        .and_then(|c| c.as_str())
        .map(String::from))
}
