use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub gateway_url: String,
    pub node_id: String,
    pub node_secret: String,
    pub gateway_public_key: String,
    pub currency: String,
}
impl Binding {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.gateway_url)?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        ensure!(
            url.scheme() == "https" || (local && url.scheme() == "http"),
            "remote gateway requires HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "gateway URL must be an origin"
        );
        ensure!(
            !self.node_id.is_empty() && !self.node_secret.is_empty(),
            "node identity required"
        );
        let _ =
            asale_client_core::security::QuotaVerifier::from_pubkey_b64(&self.gateway_public_key)?;
        ensure!(
            matches!(self.currency.as_str(), "USD" | "USDT"),
            "unsupported currency"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub model: String,
    pub provider: String,
    pub upstream_model: String,
    pub input_price_per_m: String,
    pub output_price_per_m: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}
impl Offer {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.model.is_empty() && self.model.len() <= 128 && !self.upstream_model.is_empty(),
            "model required"
        );
        ensure!(
            matches!(
                self.provider.as_str(),
                "claude" | "codex" | "custom" | "kiro"
            ),
            "unsupported provider"
        );
        for price in [&self.input_price_per_m, &self.output_price_per_m] {
            let parts: Vec<_> = price.split('.').collect();
            ensure!(
                parts.len() <= 2
                    && !parts[0].is_empty()
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
                    && parts.get(1).is_none_or(|s| s.len() <= 6),
                "price must be a decimal with at most 6 places"
            );
            let value: f64 = price.parse()?;
            ensure!((0.0..=1000.0).contains(&value), "price exceeds limit");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub paused: bool,
    pub concurrency: usize,
    pub offers: Vec<Offer>,
    pub upstream_proxy: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            name: "MSP node".into(),
            paused: true,
            concurrency: 2,
            offers: vec![],
            upstream_proxy: "auto".into(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=32).contains(&self.concurrency),
            "concurrency must be 1..32"
        );
        ensure!(self.offers.len() <= 128, "too many offers");
        let mut seen = std::collections::HashSet::new();
        for o in &self.offers {
            o.validate()?;
            ensure!(seen.insert(o.model.clone()), "duplicate public model");
        }
        if !matches!(self.upstream_proxy.as_str(), "auto" | "off") {
            asale_client_core::http::validate(&self.upstream_proxy).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
    pub fn apply_proxy(&self) {
        use asale_client_core::http::{set_preference, ProxyPref};
        let pref = match self.upstream_proxy.as_str() {
            "auto" => ProxyPref::Auto,
            "off" => ProxyPref::Direct,
            url => ProxyPref::Manual(url.to_string()),
        };
        set_preference(pref);
    }
}
