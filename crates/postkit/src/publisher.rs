use crate::ads::{
    CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd, CreatedAdCreative,
    CreativePreview, CreativePreviewRequest, UploadAdImageRequest, UploadedAdImage,
};
use crate::error::Error;
use crate::insights::{AdAccountsReply, InsightsQuery, InsightsReply};
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
    /// it could if dry-run were a flag inside `publish`. Same error shape
    /// as `thread_unsupported`: a flag the site cannot honor is a usage
    /// error, surfaced before any HTTP.
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

    /// Read metrics for a bounded range (026 §3 flag 1: the read seam).
    /// The default refusal keeps the capability honest — a connector that
    /// has not implemented insights cannot let a read slip through as
    /// something else, and the error lands before any HTTP.
    async fn insights(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::ReadMetrics,
        })
    }

    /// List credential-visible advertising accounts. It has its own
    /// capability because local vault aliases and remote ad accounts answer
    /// different operator questions; a metrics-only connector must refuse it.
    async fn ad_accounts(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::ReadAdAccounts,
        })
    }

    /// Create one advertising entity that is structurally paused. The
    /// default closes the management path for every connector that has not
    /// explicitly implemented it; a capability declaration alone is never
    /// permission to issue a write.
    async fn create_paused_ad(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &CreatePausedAdRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::CreatePausedAds,
        })
    }

    /// Upload one account-scoped image for later creative construction. The
    /// default refusal prevents a generic connector from accepting local media
    /// bytes merely because it can create some other advertising object.
    async fn upload_ad_image(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &UploadAdImageRequest,
        _deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::CreateAdCreative,
        })
    }

    /// Create one non-delivering image-link creative. A separate ad must
    /// still reference its returned ID and is forced to `PAUSED` by Tier B.
    async fn create_link_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &CreateLinkAdCreativeRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::CreateAdCreative,
        })
    }

    /// Render a stored ad creative without creating an ad. A connector must
    /// opt in explicitly because preview response bodies are remote HTML and
    /// their parsing/output contract must be reviewed per platform.
    async fn preview_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &CreativePreviewRequest,
        _deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        Err(Error::UnsupportedCapability {
            site: self.site().clone(),
            need: Capability::ReadAdPreviews,
        })
    }

    async fn refresh(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
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
