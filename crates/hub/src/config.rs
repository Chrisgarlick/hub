//! `hub.toml`: where Hub listens, its database, and where Clients is.

use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address to listen on. Keep it on localhost, with nginx in front.
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// Postgres with pgvector, e.g. `postgres://hub:…@127.0.0.1/hub`.
    pub database_url: String,
    /// The public origin Hub is served from, e.g. `https://hub.kritano.com`.
    #[serde(default = "default_public_origin")]
    pub public_origin: String,
    /// Clients' API, e.g. `http://127.0.0.1:8130` on the same droplet.
    #[serde(default)]
    pub clients_url: Option<String>,
    /// Hub's service key for Clients (`ck_…`).
    #[serde(default)]
    pub clients_service_key: Option<String>,
    /// Where you sign in to Clients in a browser.
    #[serde(default = "default_clients_public_url")]
    pub clients_public_url: String,
    /// Local development only: `/login` signs you straight in. Refused
    /// unless `public_origin` is on localhost.
    #[serde(default)]
    pub dev_login: bool,
    /// Where text is embedded for search by meaning: `voyage`, or `fake`
    /// (words hashed locally: tests and development). Unset: search by
    /// words only.
    #[serde(default)]
    pub embedding_provider: Option<String>,
    /// The provider's API key (Voyage: `pa-…`).
    #[serde(default)]
    pub embedding_key: Option<String>,
    /// The model; Voyage's default is `voyage-3.5`.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// The provider's limits. Voyage's free tier is 3 requests and 10,000
    /// tokens a minute; raise these once the account has a payment method.
    #[serde(default = "default_rpm")]
    pub embedding_requests_per_minute: u32,
    #[serde(default = "default_tpm")]
    pub embedding_tokens_per_minute: u32,
}

fn default_rpm() -> u32 {
    3
}

fn default_tpm() -> u32 {
    10_000
}

fn default_clients_public_url() -> String {
    "https://clients.kritano.com".to_string()
}

fn default_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8160))
}

fn default_public_origin() -> String {
    "http://localhost:8160".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).context("parsing config")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !(self.public_origin.starts_with("http://")
            || self.public_origin.starts_with("https://"))
        {
            bail!("public_origin must start with http:// or https://");
        }
        if self.public_origin.ends_with('/') {
            bail!("public_origin must not end with a slash");
        }
        if self.dev_login
            && !(self.public_origin.starts_with("http://localhost")
                || self.public_origin.starts_with("http://127.0.0.1"))
        {
            bail!("dev_login only works with a localhost public_origin");
        }
        match self.embedding_provider.as_deref() {
            None | Some("fake") => {}
            Some("voyage") => {
                if self.embedding_key.as_deref().is_none_or(str::is_empty) {
                    bail!("embedding_provider = \"voyage\" needs embedding_key");
                }
            }
            Some(p) => bail!("embedding_provider `{p}` isn't one Hub knows (voyage, fake)"),
        }
        if self.embedding_requests_per_minute == 0 || self.embedding_tokens_per_minute < 1000 {
            bail!("embedding limits must allow at least 1 request and 1000 tokens a minute");
        }
        if !self.database_url.starts_with("postgres://")
            && !self.database_url.starts_with("postgresql://")
        {
            bail!("database_url must be a postgres:// URL");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        let c = Config::parse("database_url = \"postgres://hub@localhost/hub\"").unwrap();
        assert_eq!(c.bind.port(), 8160);
        assert!(c.clients_url.is_none());
        assert!(Config::parse("database_url = \"sqlite://x\"").is_err());
        assert!(
            Config::parse("database_url = \"postgres://x\"\npublic_origin = \"hub.kritano.com\"")
                .is_err()
        );
        assert!(
            Config::parse("database_url = \"postgres://x\"\ntypo = 1").is_err(),
            "unknown keys are errors"
        );
    }
}
