//! Narrow, private-message types for the X connector.
//!
//! X DMs cannot reuse [`crate::Intent`]: a recipient is private data and a
//! direct-message send needs mandatory idempotency plus a separate policy
//! decision. Keeping this request closed also prevents the initial connector
//! from accidentally becoming a group-DM, inbox, or arbitrary JSON surface.

use serde::{Deserialize, Serialize};

/// One one-to-one text DM sent by the authenticated X user.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct XDirectMessageRequest {
    /// Numeric X user ID, never a handle. Handles can be renamed and would
    /// require an additional lookup/authority decision before sending.
    pub recipient_id: String,
    pub text: String,
    /// Required because a connection failure after X accepts a DM is
    /// ambiguous. The local vault records only confirmed outcomes.
    pub idempotency_key: String,
}

impl XDirectMessageRequest {
    /// Pure validation before policy, vault, or HTTP access. X user IDs and
    /// DM event IDs are decimal strings; accepting a handle or URL here would
    /// silently widen this send surface into account discovery.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_x_id(&self.recipient_id) {
            return Err("x_dm_recipient_id_invalid".into());
        }
        if self.text.trim().is_empty() {
            return Err("x_dm_text_empty".into());
        }
        if !crate::valid_name(&self.idempotency_key) {
            return Err("x_dm_idempotency_key_invalid".into());
        }
        Ok(())
    }
}

/// X's API identifiers fit in an unsigned 64-bit integer but must remain
/// strings: JSON numbers and several programming languages lose precision.
pub fn valid_x_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 19 && id.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_message_request_is_closed_and_validated_locally() {
        let good = XDirectMessageRequest {
            recipient_id: "1234567890123456789".into(),
            text: "Hello".into(),
            idempotency_key: "dm-1".into(),
        };
        assert!(good.validate().is_ok());

        let mut bad = good.clone();
        bad.recipient_id = "@not-an-id".into();
        assert_eq!(bad.validate().unwrap_err(), "x_dm_recipient_id_invalid");
        bad.recipient_id = good.recipient_id;
        bad.text = " \n".into();
        assert_eq!(bad.validate().unwrap_err(), "x_dm_text_empty");
        bad.text = "Hello".into();
        bad.idempotency_key = "has/slash".into();
        assert_eq!(bad.validate().unwrap_err(), "x_dm_idempotency_key_invalid");
    }
}
