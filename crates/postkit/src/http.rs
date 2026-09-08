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
            // Threads carries access_token (and, at exchange time, the
            // client_secret) in query strings, and the Bluesky PDS host comes
            // from the vault. reqwest's default policy follows redirects
            // across hosts, which would re-issue the credential-bearing URL
            // to whatever host Location names. No flow here redirects
            // legitimately, so hand every 3xx back and let it surface as an
            // error instead.
            .redirect(reqwest::redirect::Policy::none())
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

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    #[tokio::test]
    async fn cross_host_redirect_is_returned_not_followed() {
        // A 302 must come back as the 3xx response itself. reqwest's default
        // policy would re-issue the request — URL, query string and all — to
        // the host in Location; that is how a token-bearing URL leaks.
        let attacker = MockServer::start();
        let sink = attacker.mock(|when, then| {
            when.method(GET).path("/leak");
            then.status(200).body("leaked");
        });
        let origin = MockServer::start();
        origin.mock(|when, then| {
            when.method(GET).path("/me");
            then.status(302)
                .header("Location", format!("{}/leak", attacker.base_url()));
        });

        let http = Http::new().unwrap();
        let resp = http
            .send(
                http.get(&format!("{}/me", origin.base_url())),
                Deadline::from_secs(30),
                &Site::new("test"),
            )
            .await
            .unwrap();

        // the redirect response is handed to the caller…
        assert_eq!(resp.status().as_u16(), 302);
        // …and the other host never saw the request.
        assert_eq!(sink.hits(), 0);
    }
}
