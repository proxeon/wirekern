//! Local WhatsApp operate surfaces: Meta callback challenge, a wamid ledger,
//! consent records, and a 24h window clock.
//!
//! This is not an inbox. The ledger answers "what happened to `wamid X`?"
//! Meta has no GET-by-wamid; history exists only if the operator stores
//! callbacks. Inbound rows keep only correlation metadata, so a status log
//! does not become a hosted conversation product or retain message content.

use crate::error::Error;
use crate::whatsapp::{DeliveryStatusKind, InboundMessage, InboundMessages};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "vault-file")]
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
#[cfg(feature = "vault-file")]
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
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
    let mut left = <H as Mac>::new_from_slice(b"postkit-verify-token").expect("hmac key");
    let mut right = <H as Mac>::new_from_slice(b"postkit-verify-token").expect("hmac key");
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
    /// Apply the operator's chosen retention cutoff to delivery state and
    /// return the number of wamid rows removed. This never touches consent
    /// because an opt-out can require separate storage.
    fn purge_before(&self, before_unix: u64) -> Result<usize, Error>;
}

/// Safe-to-display metadata for a signed webhook that Postkit could verify
/// but could not reduce into its current typed event model. The body and its
/// HMAC are intentionally absent: a list command must never become a PII
/// export endpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppDeadLetterSummary {
    pub id: String,
    pub reason: String,
    pub body_sha256: String,
    pub at: u64,
}

/// An opt-in store for encrypted raw signed webhooks. The normal ledger keeps
/// only a hash audit record. Operators who need to replay a newly supported
/// event after upgrading can configure this separate store with a key that is
/// never written below the Postkit home directory.
pub trait WhatsAppReplayableDeadLetters: Send + Sync {
    fn capture(
        &self,
        reason: &str,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<WhatsAppDeadLetterSummary, Error>;
    fn list(&self) -> Result<Vec<WhatsAppDeadLetterSummary>, Error>;
    fn load(&self, id: &str) -> Result<Option<WhatsAppDeadLetter>, Error>;
    fn delete(&self, id: &str) -> Result<(), Error>;
    fn purge_before(&self, before_unix: u64) -> Result<usize, Error>;
}

/// Private replay payload. It is only ever returned inside the library so
/// [`crate::Client::replay_whatsapp_dead_letter`] can verify it again before
/// reduction; neither CLI JSON nor the HTTP server serializes this value.
#[derive(Clone, Debug)]
pub struct WhatsAppDeadLetter {
    pub summary: WhatsAppDeadLetterSummary,
    pub signature: String,
    pub raw_body: Vec<u8>,
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
        // Correlation only: retain just the identifiers needed to relate an
        // event to its conversation window. Media captions, locations,
        // contacts, interactive titles, referrals and order data are all
        // customer content and must never leak into the delivery ledger.
        let mut stored = message.clone();
        stored.text = None;
        stored.media = None;
        stored.location = None;
        stored.contacts = None;
        stored.interactive = None;
        stored.reaction = None;
        stored.referral = None;
        stored.order = None;
        stored.unsupported = None;
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

/// 24h customer-service window from the last verified inbound timestamp.
/// Meta remains authoritative, while the file-backed strict send policy uses
/// this conservative local result to refuse free-form sends before Graph.
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
    dlq: Mutex<Vec<MemoryDeadLetter>>,
}

/// Private in-memory equivalent of the file dead-letter audit record. Keep a
/// timestamp here too so retention behaves the same for embedded callers.
#[derive(Clone)]
struct MemoryDeadLetter {
    reason: String,
    body_sha256: String,
    at: u64,
}

impl MemoryWhatsAppLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dead_letters(&self) -> Vec<(String, String)> {
        self.dlq
            .lock()
            .expect("ledger")
            .iter()
            .map(|entry| (entry.reason.clone(), entry.body_sha256.clone()))
            .collect()
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
        self.dlq.lock().expect("ledger").push(MemoryDeadLetter {
            reason: reason.into(),
            body_sha256: body_sha256.into(),
            at: now_secs(),
        });
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

    fn purge_before(&self, before_unix: u64) -> Result<usize, Error> {
        let mut removed = 0;
        self.inner.lock().expect("ledger").retain(|_, record| {
            let keep = record.updated_at >= before_unix;
            removed += usize::from(!keep);
            keep
        });
        // The window clock is delivery metadata too; retaining a stale clock
        // would incorrectly report an open customer-service window.
        self.from
            .lock()
            .expect("ledger")
            .retain(|_, at| *at >= before_unix);
        self.dlq
            .lock()
            .expect("ledger")
            .retain(|entry| entry.at >= before_unix);
        Ok(removed)
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

/// In-memory replay store for tests and library embedding. Unlike the file
/// implementation it is process-private by construction, so encryption is
/// unnecessary; production persistence uses [`EncryptedFileWhatsAppDeadLetters`].
#[derive(Default)]
pub struct MemoryWhatsAppReplayableDeadLetters {
    inner: Mutex<HashMap<String, WhatsAppDeadLetter>>,
    next: Mutex<u64>,
}

impl MemoryWhatsAppReplayableDeadLetters {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WhatsAppReplayableDeadLetters for MemoryWhatsAppReplayableDeadLetters {
    fn capture(
        &self,
        reason: &str,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<WhatsAppDeadLetterSummary, Error> {
        let mut next = self.next.lock().expect("replay dlq");
        *next += 1;
        let summary = WhatsAppDeadLetterSummary {
            id: format!("dlq-{}-{next}", now_secs()),
            reason: reason.into(),
            body_sha256: sha256_hex(raw_body),
            at: now_secs(),
        };
        self.inner.lock().expect("replay dlq").insert(
            summary.id.clone(),
            WhatsAppDeadLetter {
                summary: summary.clone(),
                signature: signature.into(),
                raw_body: raw_body.to_vec(),
            },
        );
        Ok(summary)
    }

    fn list(&self) -> Result<Vec<WhatsAppDeadLetterSummary>, Error> {
        let mut entries = self
            .inner
            .lock()
            .expect("replay dlq")
            .values()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    fn load(&self, id: &str) -> Result<Option<WhatsAppDeadLetter>, Error> {
        Ok(self.inner.lock().expect("replay dlq").get(id).cloned())
    }

    fn delete(&self, id: &str) -> Result<(), Error> {
        self.inner.lock().expect("replay dlq").remove(id);
        Ok(())
    }

    fn purge_before(&self, before_unix: u64) -> Result<usize, Error> {
        let mut removed = 0;
        self.inner.lock().expect("replay dlq").retain(|_, entry| {
            let keep = entry.summary.at >= before_unix;
            removed += usize::from(!keep);
            keep
        });
        Ok(removed)
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

    /// Phone numbers are necessary inside the record for the 24-hour window,
    /// but they must not become discoverable just by listing a directory.
    fn inbound_path(&self, from: &str) -> Result<PathBuf, Error> {
        valid_wa_id(from)?;
        Ok(self
            .root
            .join("inbound")
            .join(format!("{}.json", sha256_hex(from.as_bytes()))))
    }

    // Read pre-hardening records once so upgrading does not falsely close a
    // valid service window. New writes always use the hashed path above.
    fn legacy_inbound_path(&self, from: &str) -> Result<PathBuf, Error> {
        valid_wa_id(from)?;
        Ok(self.root.join("inbound").join(format!("{from}.json")))
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
        let path = self.inbound_path(from)?;
        match read_timestamp(&path) {
            Ok(Some(at)) => Ok(Some(at)),
            Ok(None) => read_timestamp(&self.legacy_inbound_path(from)?),
            Err(error) => Err(error),
        }
    }

    fn remember_inbound_from(&self, from: &str, at: u64) -> Result<(), Error> {
        let path = self.inbound_path(from)?;
        let prev = self.last_inbound_at(from)?.unwrap_or(0);
        if at <= prev {
            return Ok(());
        }
        write_private(
            &path,
            &serde_json::to_vec(&serde_json::json!({ "at": at }))?,
        )?;
        Ok(())
    }

    fn purge_before(&self, before_unix: u64) -> Result<usize, Error> {
        let ledger = purge_json_before(&self.root.join("ledger"), "updated_at", before_unix)?;
        // The clock and dead-letter audit are storage sidecars, not delivery
        // rows, so do not make the public count depend on file layout.
        let _ = purge_json_before(&self.root.join("inbound"), "at", before_unix)?;
        let _ = purge_json_before(&self.root.join("dlq"), "at", before_unix)?;
        Ok(ledger)
    }
}

/// Durable, encrypted raw-event store for the explicit replay workflow.
///
/// The 32-byte key comes from `POSTKIT_WHATSAPP_REPLAY_DLQ_KEY` and is never
/// generated or persisted by Postkit. Losing it makes the captured events
/// intentionally unrecoverable; rotating it requires draining/replaying the
/// old queue first. This is a safer contract than silently retaining customer
/// content in the normal delivery ledger.
#[cfg(feature = "vault-file")]
pub struct EncryptedFileWhatsAppDeadLetters {
    root: PathBuf,
    key: [u8; 32],
}

#[cfg(feature = "vault-file")]
#[derive(Serialize, Deserialize)]
struct StoredDeadLetter {
    #[serde(flatten)]
    summary: WhatsAppDeadLetterSummary,
    nonce: String,
    ciphertext: String,
}

#[cfg(feature = "vault-file")]
#[derive(Serialize, Deserialize)]
struct DeadLetterPlaintext {
    signature: String,
    raw_body: Vec<u8>,
}

#[cfg(feature = "vault-file")]
impl EncryptedFileWhatsAppDeadLetters {
    pub fn new(home: impl AsRef<Path>, key_hex: &str) -> Result<Self, Error> {
        let root = home.as_ref().join("whatsapp").join("replay-dlq");
        crate::vault_file::ensure_dir(&root)?;
        Ok(Self {
            root,
            key: parse_replay_key(key_hex)?,
        })
    }

    /// Return `None` unless the operator explicitly supplied a key. A bad
    /// configured key is an error: quietly disabling replay after an operator
    /// opted in would make the durability promise misleading.
    pub fn from_env(home: impl AsRef<Path>) -> Result<Option<Self>, Error> {
        match std::env::var("POSTKIT_WHATSAPP_REPLAY_DLQ_KEY") {
            Ok(key) => Self::new(home, &key).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(replay_error("whatsapp_replay_dlq_key_invalid")),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf, Error> {
        if !valid_dead_letter_id(id) {
            return Err(Error::InvalidName(id.into()));
        }
        Ok(self.root.join(format!("{id}.json")))
    }

    fn aad(summary: &WhatsAppDeadLetterSummary) -> Vec<u8> {
        // Metadata stays visible so operators can triage without decrypting.
        // Authenticate it too, so a substituted reason/hash cannot be paired
        // with a legitimate ciphertext during replay.
        format!(
            "postkit-whatsapp-replay-dlq/v1|{}|{}|{}|{}",
            summary.id, summary.reason, summary.body_sha256, summary.at
        )
        .into_bytes()
    }

    fn decode(&self, stored: StoredDeadLetter) -> Result<WhatsAppDeadLetter, Error> {
        let nonce = STANDARD_NO_PAD
            .decode(stored.nonce)
            .map_err(|_| replay_error("whatsapp_replay_dlq_corrupt"))?;
        if nonce.len() != 24 {
            return Err(replay_error("whatsapp_replay_dlq_corrupt"));
        }
        let ciphertext = STANDARD_NO_PAD
            .decode(stored.ciphertext)
            .map_err(|_| replay_error("whatsapp_replay_dlq_corrupt"))?;
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &Self::aad(&stored.summary),
                },
            )
            .map_err(|_| replay_error("whatsapp_replay_dlq_auth_failed"))?;
        let plain: DeadLetterPlaintext = serde_json::from_slice(&plaintext)
            .map_err(|_| replay_error("whatsapp_replay_dlq_corrupt"))?;
        if sha256_hex(&plain.raw_body) != stored.summary.body_sha256 {
            return Err(replay_error("whatsapp_replay_dlq_hash_mismatch"));
        }
        Ok(WhatsAppDeadLetter {
            summary: stored.summary,
            signature: plain.signature,
            raw_body: plain.raw_body,
        })
    }
}

#[cfg(feature = "vault-file")]
impl WhatsAppReplayableDeadLetters for EncryptedFileWhatsAppDeadLetters {
    fn capture(
        &self,
        reason: &str,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<WhatsAppDeadLetterSummary, Error> {
        if reason.is_empty() || reason.len() > 128 || reason.contains('/') || reason.contains('\\')
        {
            return Err(replay_error("whatsapp_replay_dlq_reason_invalid"));
        }
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce)
            .map_err(|_| replay_error("whatsapp_replay_dlq_random_failed"))?;
        let at = now_secs();
        let summary = WhatsAppDeadLetterSummary {
            // The random nonce makes same-second identical callback bodies
            // distinct without leaking a recipient or message identifier in
            // the file name.
            id: format!("dlq-{at}-{}", &sha256_hex(&nonce)[..16]),
            reason: reason.into(),
            body_sha256: sha256_hex(raw_body),
            at,
        };
        let plain = serde_json::to_vec(&DeadLetterPlaintext {
            signature: signature.into(),
            raw_body: raw_body.to_vec(),
        })?;
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &Self::aad(&summary),
                },
            )
            .map_err(|_| replay_error("whatsapp_replay_dlq_encrypt_failed"))?;
        let stored = StoredDeadLetter {
            summary: summary.clone(),
            nonce: STANDARD_NO_PAD.encode(nonce),
            ciphertext: STANDARD_NO_PAD.encode(ciphertext),
        };
        write_private(&self.path(&summary.id)?, &serde_json::to_vec(&stored)?)?;
        Ok(summary)
    }

    fn list(&self) -> Result<Vec<WhatsAppDeadLetterSummary>, Error> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let stored: StoredDeadLetter = serde_json::from_slice(&fs::read(entry.path())?)?;
            if valid_dead_letter_id(&stored.summary.id) {
                entries.push(stored.summary);
            }
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    fn load(&self, id: &str) -> Result<Option<WhatsAppDeadLetter>, Error> {
        let path = self.path(id)?;
        match fs::read(path) {
            Ok(bytes) => self.decode(serde_json::from_slice(&bytes)?).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn delete(&self, id: &str) -> Result<(), Error> {
        let path = self.path(id)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn purge_before(&self, before_unix: u64) -> Result<usize, Error> {
        let mut removed = 0;
        for summary in self.list()? {
            if summary.at < before_unix {
                self.delete(&summary.id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(feature = "vault-file")]
fn valid_dead_letter_id(id: &str) -> bool {
    id.starts_with("dlq-")
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(feature = "vault-file")]
fn parse_replay_key(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(replay_error("whatsapp_replay_dlq_key_invalid"));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = std::str::from_utf8(chunk)
            .ok()
            .and_then(|part| u8::from_str_radix(part, 16).ok())
            .ok_or_else(|| replay_error("whatsapp_replay_dlq_key_invalid"))?;
    }
    Ok(out)
}

#[cfg(feature = "vault-file")]
fn replay_error(reason: &str) -> Error {
    Error::InvalidQuery {
        site: crate::types::Site::new("whatsapp_cloud"),
        reason: reason.into(),
    }
}

#[cfg(feature = "vault-file")]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    // `atomic_write` creates its temporary file with mode 0600 and renames
    // it into place. Avoid fs::write + chmod, which briefly exposed PII under
    // the process umask and let readers observe a partially written record.
    crate::vault_file::atomic_write(path, bytes)
}

#[cfg(feature = "vault-file")]
fn valid_wa_id(wa_id: &str) -> Result<(), Error> {
    if wa_id.is_empty() || wa_id.contains('/') || wa_id.contains('\\') || wa_id.contains('\0') {
        return Err(Error::InvalidName(wa_id.into()));
    }
    Ok(())
}

#[cfg(feature = "vault-file")]
fn read_timestamp(path: &Path) -> Result<Option<u64>, Error> {
    match fs::read(path) {
        Ok(bytes) => {
            let v: serde_json::Value = serde_json::from_slice(&bytes)?;
            Ok(v.get("at").and_then(|x| x.as_u64()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(feature = "vault-file")]
fn purge_json_before(dir: &Path, timestamp_field: &str, before_unix: u64) -> Result<usize, Error> {
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }
        let body: serde_json::Value = serde_json::from_slice(&fs::read(entry.path())?)?;
        if body
            .get(timestamp_field)
            .and_then(|value| value.as_u64())
            .is_some_and(|at| at < before_unix)
        {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
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

    fn record_path(&self, wa_id: &str) -> Result<PathBuf, Error> {
        valid_wa_id(wa_id)?;
        // Keep the actual WhatsApp ID in the owner-only JSON for an operator
        // who needs to inspect consent, but keep it out of directory names.
        Ok(self
            .root
            .join(format!("{}.json", sha256_hex(wa_id.as_bytes()))))
    }

    fn legacy_record_path(&self, wa_id: &str) -> Result<PathBuf, Error> {
        valid_wa_id(wa_id)?;
        Ok(self.root.join(format!("{wa_id}.json")))
    }
}

#[cfg(feature = "vault-file")]
impl WhatsAppConsent for FileWhatsAppConsent {
    fn get(&self, wa_id: &str) -> Result<Option<ConsentRecord>, Error> {
        let path = self.record_path(wa_id)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            // A one-time fallback preserves existing consent records after
            // the privacy hardening migration; new writes use only hashes.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::read(self.legacy_record_path(wa_id)?) {
                    Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, record: &ConsentRecord) -> Result<(), Error> {
        write_private(
            &self.record_path(&record.wa_id)?,
            &serde_json::to_vec(record)?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Site;
    use crate::whatsapp::{
        DeliveryStatus, InboundContact, InboundInteractive, InboundLocation, InboundMedia,
        InboundMessage, InboundOrder, InboundReaction, InboundReferral, InboundUnsupported,
    };

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
        assert!(!customer_window_open(
            now_secs() - CUSTOMER_WINDOW_SECS,
            now_secs()
        ));
    }

    #[test]
    fn ledger_keeps_correlation_but_drops_inbound_customer_content() {
        let ledger = MemoryWhatsAppLedger::new();
        let parsed = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![InboundMessage {
                id: "wamid.private".into(),
                from: "60123456789".into(),
                kind: "image".into(),
                timestamp: Some("100".into()),
                text: Some("private body".into()),
                context_message_id: Some("wamid.parent".into()),
                media: Some(InboundMedia {
                    id: "media-1".into(),
                    mime_type: Some("image/jpeg".into()),
                    caption: Some("private caption".into()),
                    filename: Some("private.jpg".into()),
                }),
                location: Some(InboundLocation {
                    latitude: "3.139".into(),
                    longitude: "101.6869".into(),
                    name: Some("private location".into()),
                    address: Some("private address".into()),
                }),
                contacts: Some(vec![InboundContact {
                    formatted_name: Some("private contact".into()),
                }]),
                interactive: Some(InboundInteractive {
                    kind: "button_reply".into(),
                    id: Some("yes".into()),
                    title: Some("private title".into()),
                }),
                reaction: Some(InboundReaction {
                    emoji: Some("👍".into()),
                    message_id: Some("wamid.parent".into()),
                }),
                referral: Some(InboundReferral {
                    source_type: Some("ad".into()),
                    source_id: Some("private-source".into()),
                    source_url: Some("https://example.test/private".into()),
                }),
                order: Some(InboundOrder {
                    catalog_id: Some("private-catalog".into()),
                }),
                unsupported: Some(InboundUnsupported {
                    code: Some("131051".into()),
                    title: Some("private error".into()),
                }),
            }],
            statuses: vec![],
        };

        ingest_parsed(&ledger, &parsed).unwrap();
        let stored = ledger
            .get("wamid.private")
            .unwrap()
            .unwrap()
            .inbound
            .unwrap();
        assert_eq!(stored.from, "60123456789");
        assert_eq!(stored.context_message_id.as_deref(), Some("wamid.parent"));
        assert!(stored.text.is_none());
        assert!(stored.media.is_none());
        assert!(stored.location.is_none());
        assert!(stored.contacts.is_none());
        assert!(stored.interactive.is_none());
        assert!(stored.reaction.is_none());
        assert!(stored.referral.is_none());
        assert!(stored.order.is_none());
        assert!(stored.unsupported.is_none());
    }

    #[test]
    fn retention_removes_old_delivery_state_but_not_newer_rows() {
        let ledger = MemoryWhatsAppLedger::new();
        ledger
            .put(&WhatsAppLedgerRecord {
                wamid: "wamid.old".into(),
                inbound: None,
                status: Some(DeliveryStatusKind::Delivered),
                status_timestamp: None,
                conversation_id: None,
                pricing_category: None,
                updated_at: 10,
            })
            .unwrap();
        ledger
            .put(&WhatsAppLedgerRecord {
                wamid: "wamid.new".into(),
                inbound: None,
                status: Some(DeliveryStatusKind::Delivered),
                status_timestamp: None,
                conversation_id: None,
                pricing_category: None,
                updated_at: 20,
            })
            .unwrap();
        ledger.remember_inbound_from("6011", 10).unwrap();
        ledger.remember_inbound_from("6012", 20).unwrap();

        assert_eq!(ledger.purge_before(20).unwrap(), 1);
        assert!(ledger.get("wamid.old").unwrap().is_none());
        assert!(ledger.get("wamid.new").unwrap().is_some());
        assert!(ledger.last_inbound_at("6011").unwrap().is_none());
        assert_eq!(ledger.last_inbound_at("6012").unwrap(), Some(20));
    }

    #[cfg(feature = "vault-file")]
    #[test]
    fn file_ledger_hashes_phone_filenames_and_purges_expired_state() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = FileWhatsAppLedger::new(temp.path()).unwrap();
        ledger.remember_inbound_from("60123456789", 10).unwrap();
        ledger
            .put(&WhatsAppLedgerRecord {
                wamid: "wamid.old".into(),
                inbound: None,
                status: Some(DeliveryStatusKind::Delivered),
                status_timestamp: None,
                conversation_id: None,
                pricing_category: None,
                updated_at: 10,
            })
            .unwrap();

        let inbound_names = std::fs::read_dir(temp.path().join("whatsapp/inbound"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(inbound_names.len(), 1);
        assert!(!inbound_names[0].contains("60123456789"));
        assert_eq!(ledger.last_inbound_at("60123456789").unwrap(), Some(10));

        assert_eq!(ledger.purge_before(20).unwrap(), 1);
        assert!(ledger.get("wamid.old").unwrap().is_none());
        assert!(ledger.last_inbound_at("60123456789").unwrap().is_none());
    }

    #[cfg(feature = "vault-file")]
    #[test]
    fn encrypted_replay_dlq_hides_raw_customer_body_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let key = "ab".repeat(32);
        let store = EncryptedFileWhatsAppDeadLetters::new(temp.path(), &key).unwrap();
        let raw = br#"{"customer":"private message body","id":"wamid.private"}"#;
        let summary = store
            .capture("webhook_status_unsupported", "sha256=signature", raw)
            .unwrap();

        // The on-disk envelope has triage metadata, but never the raw body
        // or signature. A replay key is required to decrypt either value.
        let path = temp
            .path()
            .join("whatsapp/replay-dlq")
            .join(format!("{}.json", summary.id));
        let stored = std::fs::read_to_string(path).unwrap();
        assert!(!stored.contains("private message body"));
        assert!(!stored.contains("sha256=signature"));
        assert_eq!(store.list().unwrap(), vec![summary.clone()]);

        let replay = store.load(&summary.id).unwrap().unwrap();
        assert_eq!(replay.raw_body, raw);
        assert_eq!(replay.signature, "sha256=signature");
        store.delete(&summary.id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[cfg(feature = "vault-file")]
    #[test]
    fn replay_dlq_rejects_malformed_operator_key() {
        let temp = tempfile::tempdir().unwrap();
        let error = match EncryptedFileWhatsAppDeadLetters::new(temp.path(), "not-a-32-byte-key") {
            Ok(_) => panic!("malformed replay key must be refused"),
            Err(error) => error,
        };
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "whatsapp_replay_dlq_key_invalid")
        );
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
