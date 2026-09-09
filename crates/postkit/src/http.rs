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
            .map_err(|_| Error::Network {
                site: Site::new(""),
                // This error does not carry a request URL, but keeping every
                // public network error generic avoids future regressions.
                message: "client initialization failed".into(),
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
                // A reqwest transport error can include the complete URL.
                // That URL may contain OAuth credentials, so never preserve
                // its diagnostic text in a user-visible Error.
                Error::request_failed(site)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::net::TcpListener;
    use std::thread;

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

    #[tokio::test]
    async fn failed_request_never_exposes_query_credentials() {
        // Accept then close one connection. This creates a real reqwest
        // failure without DNS or public-network dependence, using a URL that
        // deliberately contains secrets which must never reach Error::Display.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let _ = listener.accept();
        });

        let token = "access-token-must-not-leak";
        let secret = "client-secret-must-not-leak";
        let url = format!("http://{address}/oauth?access_token={token}&client_secret={secret}");
        let http = Http::new().unwrap();
        let err = http
            .send(http.get(&url), Deadline::from_secs(2), &Site::new("test"))
            .await
            .unwrap_err();
        let rendered = err.to_string();

        assert!(matches!(err, Error::Network { ref message, .. } if message == "request failed"));
        assert!(!rendered.contains(token));
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("access_token"));
        assert!(!rendered.contains("client_secret"));
    }
}
