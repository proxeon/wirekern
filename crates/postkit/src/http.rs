use crate::error::Error;
use crate::types::{Deadline, Site, USER_AGENT};
use reqwest::RequestBuilder;

/// Shared reqwest client. No platform URLs — connectors own hosts.
pub struct Http {
    inner: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self, Error> {
        let inner = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .use_rustls_tls()
            .build()
            .map_err(|e| Error::Network {
                site: Site::new(""),
                message: e.to_string(),
            })?;
        Ok(Self { inner })
    }

    pub fn get(&self, url: &str) -> RequestBuilder {
        self.inner.get(url)
    }

    pub fn post(&self, url: &str) -> RequestBuilder {
        self.inner.post(url)
    }

    pub async fn send(
        &self,
        req: RequestBuilder,
        deadline: Deadline,
        site: &Site,
    ) -> Result<reqwest::Response, Error> {
        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return Err(Error::DeadlineExceeded { site: site.clone() });
        }
        req.timeout(remaining).send().await.map_err(|e| {
            if e.is_timeout() {
                Error::DeadlineExceeded { site: site.clone() }
            } else {
                Error::Network {
                    site: site.clone(),
                    message: e.to_string(),
                }
            }
        })
    }
}
