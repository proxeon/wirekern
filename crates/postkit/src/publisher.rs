use crate::error::Error;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
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

    async fn whoami(&self, app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error>;

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
