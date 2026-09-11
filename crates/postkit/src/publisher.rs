use crate::error::Error;
use crate::types::{
    AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Probe, Site, WhoAmI,
};
use async_trait::async_trait;

// 012 wants native async fn; `dyn Publisher` in Registry is not object-safe
// without boxing. async_trait is the dyn path.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthKind {
    None,
    AppPassword,
    OAuth2AuthCode,
    /// A long-lived bearer token issued by a platform outside RFC 6749's
    /// authorization-code flow (for example a Meta System User token).
    StaticToken,
}

#[derive(Clone, Debug)]
pub enum AuthStart {
    Browser {
        authorize_url: String,
        state: String,
    },
    PasteInstructions {
        hint: String,
    },
    None,
}

#[derive(Clone, Debug)]
pub enum AuthReply {
    Redirect {
        url: String,
    },
    Pasted {
        code: String,
    },
    AppPassword {
        identifier: String,
        secret: String,
        pds: Option<String>,
    },
}

/// Kernel seam: social publish + auth.
///
/// Frozen. Do not add insights, ads, pages, media, or messaging methods
/// here. Those are optional [`crate::facets`] attached on
/// [`crate::Connector`]. A new site implements this trait; extra verbs are
/// extra traits. Probe stays because it is the create-only half of publish,
/// not a new product surface.
#[async_trait]
pub trait Publisher: Send + Sync {
    fn site(&self) -> &Site;
    fn capabilities(&self) -> &[Capability];
    fn auth_kind(&self) -> AuthKind;

    async fn publish(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error>;

    /// Create-only publish probe (027): run every step of a publish except
    /// the one that makes it visible. The default refusal keeps connectors
    /// honest — only a site whose API genuinely splits creation from
    /// publication can offer a probe, and a connector that forgets to
    /// implement it cannot silently publish for real on a dry-run the way
    /// it could if dry-run were a flag inside `publish`.
    async fn probe(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Probe, Error> {
        Err(Error::InvalidPost {
            site: self.site().clone(),
            reason: "dry_run_unsupported".into(),
            limit: None,
        })
    }

    async fn whoami(&self, app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error>;

    /// Re-issue stored credentials. `deadline` is the *caller's* budget —
    /// issue 024: refresh shares the same end-to-end deadline as the
    /// publish/insight it serves, so `--deadline 1` bounds refresh plus
    /// request together instead of silently adding a fixed 30s of its own.
    async fn refresh(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        Err(Error::Auth {
            site: self.site().clone(),
            reason: "no_refresh".into(),
        })
    }

    async fn auth_start(&self, _app: &AppConfig) -> Result<AuthStart, Error> {
        Err(Error::Auth {
            site: self.site().clone(),
            reason: "unsupported_auth".into(),
        })
    }

    async fn auth_finish(
        &self,
        _app: &AppConfig,
        _reply: AuthReply,
    ) -> Result<AccountCreds, Error> {
        Err(Error::Auth {
            site: self.site().clone(),
            reason: "unsupported_auth".into(),
        })
    }
}
