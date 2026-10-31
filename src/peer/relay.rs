//! The relay's management API (mumpeer), behind a narrow trait so the module logic can be tested
//! against a mock.
//!
//! All calls send `Authorization: Bearer <secret>` and JSON bodies:
//! - `GET /v1/status` → `{"instance":"…"}`; a new instance means the relay restarted.
//! - `PUT /v1/allowlist` with `{"hashes":[…]}`: the full set of certificate hashes (SHA-1 hex)
//!   of the users currently on the server. Only they may use the relay.
//! - `POST /v1/tokens` with `{"token","a","b","expires_at"}`: a short-lived token that lets the
//!   two users with certificate hashes `a` and `b` meet on the relay.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::peer::config::{PeerConfig, Secret};

/// Upper bound for one API call; the bot never waits longer on the relay.
pub const API_TIMEOUT: Duration = Duration::from_secs(5);

/// A token registered with the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenGrant {
    pub token: String,
    /// Certificate hash of the requester.
    pub a: String,
    /// Certificate hash of the peer.
    pub b: String,
    /// Unix seconds.
    pub expires_at: u64,
}

pub trait RelayApi {
    /// The relay's instance id; it changes when the relay restarts.
    fn status(&self) -> impl Future<Output = Result<String>> + Send;
    /// Replaces the allowlist with `hashes`.
    fn put_allowlist(&self, hashes: &[String]) -> impl Future<Output = Result<()>> + Send;
    fn register_token(&self, grant: &TokenGrant) -> impl Future<Output = Result<()>> + Send;
}

/// The real API over HTTP(S).
pub struct HttpRelay {
    client: reqwest::Client,
    base: String,
    secret: Secret,
}

#[derive(Deserialize)]
struct Status {
    instance: String,
}

#[derive(Serialize)]
struct Allowlist<'a> {
    hashes: &'a [String],
}

impl HttpRelay {
    pub fn new(config: &PeerConfig, secret: Secret) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("peer: cannot build the relay API client")?;
        Ok(Self {
            client,
            base: config.api_url.trim_end_matches('/').to_owned(),
            secret,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(self.secret.expose())
    }
}

/// Fails on any non-2xx answer; the body is not logged (it may echo request data).
fn check(response: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = response.status();
    if !status.is_success() {
        bail!("relay API {what}: HTTP {status}");
    }
    Ok(response)
}

impl RelayApi for HttpRelay {
    async fn status(&self) -> Result<String> {
        let response = self
            .request(reqwest::Method::GET, "/v1/status")
            .send()
            .await
            .context("relay API status")?;
        let status: Status = check(response, "status")?
            .json()
            .await
            .context("relay API status: invalid answer")?;
        Ok(status.instance)
    }

    async fn put_allowlist(&self, hashes: &[String]) -> Result<()> {
        let response = self
            .request(reqwest::Method::PUT, "/v1/allowlist")
            .json(&Allowlist { hashes })
            .send()
            .await
            .context("relay API allowlist")?;
        check(response, "allowlist").map(drop)
    }

    async fn register_token(&self, grant: &TokenGrant) -> Result<()> {
        let response = self
            .request(reqwest::Method::POST, "/v1/tokens")
            .json(grant)
            .send()
            .await
            .context("relay API token")?;
        check(response, "token").map(drop)
    }
}
