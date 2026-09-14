//! Shared local validators used by ads request types.
pub(super) fn confirm_ids(id: &str, confirm_id: &str) -> Result<(), String> {
    require_numeric_id("ad_entity_id", id)?;
    require_numeric_id("confirm_id", confirm_id)?;
    if confirm_id != id {
        return Err("confirm_id_mismatch".into());
    }
    Ok(())
}

/// Meta accepts `daily_budget` or `lifetime_budget`, never both, at one
/// object. `allow_none` is true for campaigns (ABO: budget lives on the ad
/// set) and for ad sets (CBO: budget lives on the campaign).
pub(super) fn validate_budget_xor(
    daily: Option<u64>,
    lifetime: Option<u64>,
    allow_none: bool,
) -> Result<(), String> {
    match (daily, lifetime) {
        (None, None) if allow_none => Ok(()),
        (None, None) => Err("missing_budget".into()),
        (Some(0), _) | (_, Some(0)) => Err("budget_must_be_positive".into()),
        (Some(_), Some(_)) => Err("daily_and_lifetime_budget_mutually_exclusive".into()),
        (Some(_), None) | (None, Some(_)) => Ok(()),
    }
}

pub(super) fn require_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err("missing_name".into())
    } else {
        Ok(())
    }
}

pub(super) fn require_numeric_id(field: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        Err(format!("bad_{field}:{id}"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_account(account: Option<&str>) -> Result<(), String> {
    if let Some(account) = account {
        // Operators copy `act_<id>` from account discovery; accept that
        // canonical spelling as well as a bare numeric API ID.
        require_numeric_id(
            "ad_account",
            account.strip_prefix("act_").unwrap_or(account),
        )?;
    }
    Ok(())
}

pub(super) fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("missing_{field}"));
    }
    Ok(())
}

pub(super) fn require_https_url(field: &str, value: &str) -> Result<(), String> {
    let Some(authority_and_path) = value.strip_prefix("https://") else {
        return Err(format!("{field}_must_be_https"));
    };
    // Split at the first path/query/fragment separator. This rejects values
    // such as `https:///offer`: they have the required scheme text but no
    // authority, so Meta would only return a less actionable form error.
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{field}_must_be_https"));
    }
    Ok(())
}
