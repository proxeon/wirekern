//! Local WhatsApp operate surfaces: Meta callback challenge, a wamid ledger,
//! consent records, and a 24h window clock.
//!
//! This is not an inbox. The ledger answers "what happened to `wamid X`?"
//! Meta has no GET-by-wamid; history exists only if the operator stores
//! callbacks. Message bodies are optional on inbound rows so a status log
//! does not become a hosted conversation product.

use crate::error::Error;
use crate::whatsapp::{DeliveryStatusKind, InboundMessage, InboundMessages};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "vault-file")]
use std::fs;
#[cfg(feature = "vault-file")]
use std::path::{Path, PathBuf};

/// Meta's documented Cloud API default throughput (~80 messages/second
/// per phone number). Used as a local send-pacing ceiling, not a Meta SLA.
pub const DEFAULT_THROUGHPUT_PER_SEC: u32 = 80;
pub const CUSTOMER_WINDOW_SECS: u64 = 24 * 60 * 60;

/// GET hub.mode / hub.verify_token / hub.challenge. Success returns the
/// raw challenge string Meta must receive as the HTTP body (not JSON).
pub fn verify_webhook_challenge(
    stored_token: &str,
    mode: &str,
    supplied_token: &str,
    challenge: &str,
) -> Result<String, String> {
    if mode != "subscribe" {
        return Err("webhook_challenge_mode".into());
    }
    if stored_token.is_empty() {
        return Err("webhook_verify_token_missing".into());
    }
    if challenge.is_empty() || challenge.len() > 256 {
        return Err("webhook_challenge_invalid".into());
    }
    if !constant_eq(stored_token, supplied_token) {
        return Err("webhook_verify_token_mismatch".into());
    }
    Ok(challenge.to_string())
}

fn constant_eq(a: &str, b: &str) -> bool {
    // Hash both sides so a length mismatch cannot short-circuit the compare.
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type H = Hmac<Sha256>;
    let mut left = H::new_from_slice(b"postkit-verify-token").expect("hmac key");
    let mut right = H::new_from_slice(b"postkit-verify-token").expect("hmac key");
    left.update(a.as_bytes());
    right.update(b.as_bytes());
    left.finalize().into_bytes() == right.finalize().into_bytes()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppLedgerRecord {
    pub wamid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbound: Option<InboundMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<DeliveryStatusKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_category: Option<String>,
    pub updated_at: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LedgerApply {
    Inserted,
    Unchanged,
    Advanced,
}

pub trait WhatsAppLedger: Send + Sync {
    fn get(&self, wamid: &str) -> Result<Option<WhatsAppLedgerRecord>, Error>;
    fn put(&self, record: &WhatsAppLedgerRecord) -> Result<(), Error>;
    fn put_dead_letter(&self, reason: &str, body_sha256: &str) -> Result<(), Error>;
    fn last_inbound_at(&self, from: &str) -> Result<Option<u64>, Error>;
    fn remember_inbound_from(&self, from: &str, at: u64) -> Result<(), Error>;
}

/// Apply one signed parse into the ledger. Duplicate inbound `wamid`s are
/// ignored; status only moves forward (sent → delivered → read, or failed).
pub fn ingest_parsed(
    ledger: &dyn WhatsAppLedger,
    parsed: &InboundMessages,
) -> Result<Vec<(String, LedgerApply)>, Error> {
    let mut out = Vec::new();
    for message in &parsed.messages {
        if !valid_wamid(&message.id) {
            continue;
        }
        let existing = ledger.get(&message.id)?;
        if existing.as_ref().is_some_and(|r| r.inbound.is_some()) {
            out.push((message.id.clone(), LedgerApply::Unchanged));
            continue;
        }
        let mut record = existing.unwrap_or(WhatsAppLedgerRecord {
            wamid: message.id.clone(),
            inbound: None,
            status: None,
            status_timestamp: None,
            conversation_id: None,
            pricing_category: None,
            updated_at: now_secs(),
        });
        // Correlation only: do not persist message bodies in the ledger.
        let mut stored = message.clone();
        stored.text = None;
        record.inbound = Some(stored);
        record.updated_at = now_secs();
        ledger.put(&record)?;
        let at = message
            .timestamp
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(now_secs);
        ledger.remember_inbound_from(&message.from, at)?;
        out.push((message.id.clone(), LedgerApply::Inserted));
    }
    for status in &parsed.statuses {
        if !valid_wamid(&status.id) {
            continue;
        }
        let existing = ledger.get(&status.id)?;
        if let Some(prev) = existing.as_ref().and_then(|r| r.status) {
            if !status_may_advance(prev, status.status) {
                out.push((status.id.clone(), LedgerApply::Unchanged));
                continue;
            }
        }
        let applied = if existing.is_some() {
            LedgerApply::Advanced
        } else {
            LedgerApply::Inserted
        };
        let mut record = existing.unwrap_or(WhatsAppLedgerRecord {
            wamid: status.id.clone(),
            inbound: None,
            status: None,
            status_timestamp: None,
            conversation_id: None,
            pricing_category: None,
            updated_at: now_secs(),
        });
        record.status = Some(status.status);
        record.status_timestamp = status.timestamp.clone();
        if let Some(conv) = &status.conversation {
            record.conversation_id = conv.id.clone();
        }
        if let Some(pricing) = &status.pricing {
            record.pricing_category = pricing.category.clone();
        }
        record.updated_at = now_secs();
        ledger.put(&record)?;
        out.push((status.id.clone(), applied));
    }
    Ok(out)
}

fn status_rank(kind: DeliveryStatusKind) -> u8 {
    match kind {
        DeliveryStatusKind::Sent => 1,
        DeliveryStatusKind::Delivered => 2,
        DeliveryStatusKind::Read => 3,
        DeliveryStatusKind::Failed => 2,
    }
}

/// Failed can replace sent/delivered (Meta may fail after accept) but must
/// not clobber an already-read message if callbacks arrive out of order.
fn status_may_advance(prev: DeliveryStatusKind, new: DeliveryStatusKind) -> bool {
    if prev == new {
        return false;
    }
    match (prev, new) {
        (DeliveryStatusKind::Failed, _) => false,
        (DeliveryStatusKind::Read, DeliveryStatusKind::Failed) => false,
        (DeliveryStatusKind::Read, _) => false,
        (_, DeliveryStatusKind::Failed) => true,
        (a, b) => status_rank(b) > status_rank(a),
    }
}

fn valid_wamid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.contains('/') && !id.contains('\\')
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 24h customer-service window as a computed hint from last inbound
/// timestamp. Meta enforces the window; we do not store policy state.
pub fn customer_window_open(last_inbound_unix: u64, now_unix: u64) -> bool {
    now_unix.saturating_sub(last_inbound_unix) < CUSTOMER_WINDOW_SECS
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    OptIn,
    OptOut,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub wa_id: String,
    pub kind: ConsentKind,
    pub at: u64,
}

pub trait WhatsAppConsent: Send + Sync {
    fn get(&self, wa_id: &str) -> Result<Option<ConsentRecord>, Error>;
    fn put(&self, record: &ConsentRecord) -> Result<(), Error>;
}

/// In-memory ledger for tests and library callers that do not want files.
#[derive(Default)]
pub struct MemoryWhatsAppLedger {
    inner: Mutex<HashMap<String, WhatsAppLedgerRecord>>,
    from: Mutex<HashMap<String, u64>>,
    dlq: Mutex<Vec<(String, String)>>,
}

impl MemoryWhatsAppLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dead_letters(&self) -> Vec<(String, String)> {
        self.dlq.lock().expect("ledger").clone()
    }
}

impl WhatsAppLedger for MemoryWhatsAppLedger {
    fn get(&self, wamid: &str) -> Result<Option<WhatsAppLedgerRecord>, Error> {
        Ok(self.inner.lock().expect("ledger").get(wamid).cloned())
    }

    fn put(&self, record: &WhatsAppLedgerRecord) -> Result<(), Error> {
        self.inner
            .lock()
            .expect("ledger")
            .insert(record.wamid.clone(), record.clone());
        Ok(())
    }

    fn put_dead_letter(&self, reason: &str, body_sha256: &str) -> Result<(), Error> {
        self.dlq
            .lock()
            .expect("ledger")
            .push((reason.into(), body_sha256.into()));
        Ok(())
    }

    fn last_inbound_at(&self, from: &str) -> Result<Option<u64>, Error> {
        Ok(self.from.lock().expect("ledger").get(from).copied())
    }

    fn remember_inbound_from(&self, from: &str, at: u64) -> Result<(), Error> {
        let mut map = self.from.lock().expect("ledger");
        let entry = map.entry(from.to_string()).or_insert(0);
        if at > *entry {
            *entry = at;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryWhatsAppConsent {
    inner: Mutex<HashMap<String, ConsentRecord>>,
}

impl MemoryWhatsAppConsent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WhatsAppConsent for MemoryWhatsAppConsent {
    fn get(&self, wa_id: &str) -> Result<Option<ConsentRecord>, Error> {
        Ok(self.inner.lock().expect("consent").get(wa_id).cloned())
    }

    fn put(&self, record: &ConsentRecord) -> Result<(), Error> {
        self.inner
            .lock()
            .expect("consent")
            .insert(record.wa_id.clone(), record.clone());
        Ok(())
    }
}

/// Token bucket paced at [`DEFAULT_THROUGHPUT_PER_SEC`]. One number, in-process.
#[derive(Debug)]
pub struct ThroughputQueue {
    last_ns: Mutex<u64>,
    min_interval_ns: u64,
}

impl ThroughputQueue {
    pub fn new(per_sec: u32) -> Self {
        let per_sec = per_sec.max(1);
        Self {
            last_ns: Mutex::new(0),
            min_interval_ns: 1_000_000_000 / u64::from(per_sec),
        }
    }

    pub fn default_cloud_api() -> Self {
        Self::new(DEFAULT_THROUGHPUT_PER_SEC)
    }

    /// Returns how many nanoseconds the caller should wait before sending.
    pub fn wait_ns(&self, now_ns: u64) -> u64 {
        let mut last = self.last_ns.lock().expect("throughput");
        if *last == 0 {
            *last = now_ns;
            return 0;
        }
        let ready_at = last.saturating_add(self.min_interval_ns);
        if now_ns >= ready_at {
            *last = now_ns;
            0
        } else {
            ready_at - now_ns
        }
    }
}

#[cfg(feature = "vault-file")]
pub struct FileWhatsAppLedger {
    root: PathBuf,
}

#[cfg(feature = "vault-file")]
impl FileWhatsAppLedger {
    pub fn new(home: impl AsRef<Path>) -> Result<Self, Error> {
        let root = home.as_ref().join("whatsapp");
        crate::vault_file::ensure_dir(&root.join("ledger"))?;
        crate::vault_file::ensure_dir(&root.join("dlq"))?;
        crate::vault_file::ensure_dir(&root.join("inbound"))?;
        Ok(Self { root })
    }

    fn record_path(&self, wamid: &str) -> Result<PathBuf, Error> {
        if !valid_wamid(wamid) {
            return Err(Error::InvalidName(wamid.into()));
        }
        Ok(self.root.join("ledger").join(format!("{wamid}.json")))
    }
}

#[cfg(feature = "vault-file")]
impl WhatsAppLedger for FileWhatsAppLedger {
    fn get(&self, wamid: &str) -> Result<Option<WhatsAppLedgerRecord>, Error> {
        let path = self.record_path(wamid)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, record: &WhatsAppLedgerRecord) -> Result<(), Error> {
        let path = self.record_path(&record.wamid)?;
        write_private(&path, &serde_json::to_vec(record)?)?;
        Ok(())
    }

    fn put_dead_letter(&self, reason: &str, body_sha256: &str) -> Result<(), Error> {
        if reason.contains('/') || reason.contains('\\') {
            return Err(Error::InvalidName(reason.into()));
        }
        let name = format!("{}-{body_sha256}.json", now_secs());
        let path = self.root.join("dlq").join(name);
        write_private(
            &path,
            &serde_json::to_vec(&serde_json::json!({
                "reason": reason,
                "body_sha256": body_sha256,
                "at": now_secs(),
            }))?,
        )?;
        Ok(())
    }

    fn last_inbound_at(&self, from: &str) -> Result<Option<u64>, Error> {
        if from.is_empty() || from.contains('/') {
            return Err(Error::InvalidName(from.into()));
        }
        let path = self.root.join("inbound").join(format!("{from}.json"));
        match fs::read(&path) {
            Ok(bytes) => {
                let v: serde_json::Value = serde_json::from_slice(&bytes)?;
                Ok(v.get("at").and_then(|x| x.as_u64()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn remember_inbound_from(&self, from: &str, at: u64) -> Result<(), Error> {
        if from.is_empty() || from.contains('/') {
            return Err(Error::InvalidName(from.into()));
        }
        let path = self.root.join("inbound").join(format!("{from}.json"));
        let prev = self.last_inbound_at(from)?.unwrap_or(0);
        if at <= prev {
            return Ok(());
        }
        write_private(&path, &serde_json::to_vec(&serde_json::json!({ "at": at }))?)?;
        Ok(())
    }
}

#[cfg(feature = "vault-file")]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(feature = "vault-file")]
pub struct FileWhatsAppConsent {
    root: PathBuf,
}

#[cfg(feature = "vault-file")]
impl FileWhatsAppConsent {
    pub fn new(home: impl AsRef<Path>) -> Result<Self, Error> {
        let root = home.as_ref().join("whatsapp").join("consent");
        crate::vault_file::ensure_dir(&root)?;
        Ok(Self { root })
    }
}

#[cfg(feature = "vault-file")]
impl WhatsAppConsent for FileWhatsAppConsent {
    fn get(&self, wa_id: &str) -> Result<Option<ConsentRecord>, Error> {
        if wa_id.is_empty() || wa_id.contains('/') {
            return Err(Error::InvalidName(wa_id.into()));
        }
        let path = self.root.join(format!("{wa_id}.json"));
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, record: &ConsentRecord) -> Result<(), Error> {
        if record.wa_id.is_empty() || record.wa_id.contains('/') {
            return Err(Error::InvalidName(record.wa_id.clone()));
        }
        write_private(
            &self.root.join(format!("{}.json", record.wa_id)),
            &serde_json::to_vec(record)?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Site;
    use crate::whatsapp::{DeliveryStatus, InboundMessage};

    fn inbound(id: &str, from: &str) -> InboundMessage {
        InboundMessage {
            id: id.into(),
            from: from.into(),
            kind: "text".into(),
            timestamp: Some("100".into()),
            text: Some("hi".into()),
            context_message_id: None,
            media: None,
            location: None,
            contacts: None,
            interactive: None,
            reaction: None,
            referral: None,
            order: None,
            unsupported: None,
        }
    }

    #[test]
    fn challenge_echoes_raw_token_and_rejects_mismatch() {
        assert_eq!(
            verify_webhook_challenge("secret", "subscribe", "secret", "1158").unwrap(),
            "1158"
        );
        assert_eq!(
            verify_webhook_challenge("secret", "subscribe", "other", "1158").unwrap_err(),
            "webhook_verify_token_mismatch"
        );
        assert_eq!(
            verify_webhook_challenge("secret", "unsubscribe", "secret", "1158").unwrap_err(),
            "webhook_challenge_mode"
        );
    }

    #[test]
    fn ledger_dedups_inbound_and_advances_status() {
        let ledger = MemoryWhatsAppLedger::new();
        let parsed = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![inbound("wamid.a", "6011")],
            statuses: vec![],
        };
        let first = ingest_parsed(&ledger, &parsed).unwrap();
        assert_eq!(first[0].1, LedgerApply::Inserted);
        let again = ingest_parsed(&ledger, &parsed).unwrap();
        assert_eq!(again[0].1, LedgerApply::Unchanged);
        let status = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![DeliveryStatus {
                id: "wamid.a".into(),
                status: DeliveryStatusKind::Sent,
                timestamp: Some("1".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            }],
        };
        assert_eq!(
            ingest_parsed(&ledger, &status).unwrap()[0].1,
            LedgerApply::Advanced
        );
        let regress = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![DeliveryStatus {
                id: "wamid.a".into(),
                status: DeliveryStatusKind::Sent,
                timestamp: Some("2".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            }],
        };
        // Same rank is unchanged; delivered would advance.
        assert_eq!(
            ingest_parsed(&ledger, &regress).unwrap()[0].1,
            LedgerApply::Unchanged
        );
        let delivered = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![DeliveryStatus {
                id: "wamid.a".into(),
                status: DeliveryStatusKind::Delivered,
                timestamp: Some("3".into()),
                errors: vec![],
                recipient_id: None,
                conversation: Some(crate::whatsapp::DeliveryConversation {
                    id: Some("conv-1".into()),
                    origin_type: Some("service".into()),
                }),
                pricing: Some(crate::whatsapp::DeliveryPricing {
                    billable: Some(false),
                    pricing_model: Some("PMP".into()),
                    category: Some("service".into()),
                }),
            }],
        };
        ingest_parsed(&ledger, &delivered).unwrap();
        let row = ledger.get("wamid.a").unwrap().unwrap();
        assert_eq!(row.status, Some(DeliveryStatusKind::Delivered));
        assert_eq!(row.pricing_category.as_deref(), Some("service"));
        assert!(row.inbound.as_ref().unwrap().text.is_none());
        let read = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![DeliveryStatus {
                id: "wamid.a".into(),
                status: DeliveryStatusKind::Read,
                timestamp: Some("4".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            }],
        };
        ingest_parsed(&ledger, &read).unwrap();
        let failed = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![DeliveryStatus {
                id: "wamid.a".into(),
                status: DeliveryStatusKind::Failed,
                timestamp: Some("5".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            }],
        };
        assert_eq!(
            ingest_parsed(&ledger, &failed).unwrap()[0].1,
            LedgerApply::Unchanged
        );
        assert_eq!(
            ledger.get("wamid.a").unwrap().unwrap().status,
            Some(DeliveryStatusKind::Read)
        );
        assert!(customer_window_open(now_secs() - 60, now_secs()));
        assert!(!customer_window_open(now_secs() - CUSTOMER_WINDOW_SECS, now_secs()));
    }

    #[test]
    fn throughput_queue_spaces_sends() {
        let q = ThroughputQueue::new(2);
        assert_eq!(q.wait_ns(1_000), 0);
        assert!(q.wait_ns(1_000) > 0);
        assert_eq!(q.wait_ns(1_000 + 500_000_000), 0);
    }

    #[test]
    fn consent_opt_out_is_local_only() {
        let store = MemoryWhatsAppConsent::new();
        store
            .put(&ConsentRecord {
                wa_id: "6011".into(),
                kind: ConsentKind::OptOut,
                at: 1,
            })
            .unwrap();
        assert_eq!(
            store.get("6011").unwrap().unwrap().kind,
            ConsentKind::OptOut
        );
    }
}
