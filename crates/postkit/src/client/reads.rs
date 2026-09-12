//! Page directory and published-media reads.
use super::Client;
use crate::error::Error;
use crate::media::{MediaQuery, MediaReply};
use crate::pages::PagesReply;
use crate::types::{AccountKey, Capability, Deadline};

impl Client {
    /// Discover remote Pages visible to this credential. This follows the
    /// read-only account-discovery shape: capability before vault access,
    /// bounded refresh, then one retry only for a confirmed expired token.
    pub async fn pages(&self, key: &AccountKey, deadline: Deadline) -> Result<PagesReply, Error> {
        self.require_capability(&key.site, Capability::ReadPages)?;
        let directory = self.page_directory(&key.site)?;
        self.with_creds(key, deadline, move |app, creds| {
            let directory = directory.clone();
            Box::pin(async move { directory.pages(&app, &creds, deadline).await })
        })
        .await
    }

    /// Read one intentionally bounded page of published media. Like every
    /// remote read, this validates before credentials are loaded, then uses
    /// the standard one-refresh retry only for a confirmed expired token.
    pub async fn media(
        &self,
        key: &AccountKey,
        query: MediaQuery,
        deadline: Deadline,
    ) -> Result<MediaReply, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMedia)?;
        let reader = self.media_reader(&key.site)?;
        self.with_creds(key, deadline, move |app, creds| {
            let reader = reader.clone();
            Box::pin(async move { reader.media(&app, &creds, &query, deadline).await })
        })
        .await
    }
}
