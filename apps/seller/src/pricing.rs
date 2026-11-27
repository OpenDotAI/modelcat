//! Public base list prices in USD per million text tokens. A service task owns
//! all external requests; management clients only read the in-memory snapshot.
use crate::{
    connections,
    state::{data_dir, now, App},
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

const URL: &str = "https://models.dev/api.json";
const DAY: i64 = 86400;
const VENDORS: &[&str] = &[
    "openai",
    "anthropic",
    "deepseek",
    "google",
    "xai",
    "mistral",
    "cohere",
    "moonshotai",
    "zai",
    "alibaba",
];

#[derive(Clone, Deserialize, Serialize)]
struct Price {
    vendor: String,
    input: f64,
    output: f64,
}

#[derive(Default, Clone, Deserialize, Serialize)]
pub(crate) struct Catalog {
    checked: i64,
    etag: Option<String>,
    models: BTreeMap<String, Vec<Price>>,
    #[serde(skip)]
    failed: bool,
}

impl Catalog {
    fn parse(body: &[u8]) -> Result<Self> {
        let source: Value = serde_json::from_slice(body)?;
        let mut catalog = Self::default();
        for &vendor in VENDORS {
            let Some(models) = source[vendor]["models"].as_object() else {
                continue;
            };
            for (id, model) in models {
                if !model["modalities"]["output"]
                    .as_array()
                    .is_some_and(|m| m.iter().any(|v| v == "text"))
                {
                    continue;
                }
                let cost = &model["cost"];
                let (Some(input), Some(output)) = (cost["input"].as_f64(), cost["output"].as_f64())
                else {
                    continue;
                };
                if input < 0.0 || output < 0.0 {
                    continue;
                }
                let price = Price {
                    vendor: vendor.into(),
                    input,
                    output,
                };
                catalog.models.entry(id.clone()).or_default().push(price);
            }
        }
        ensure!(!catalog.models.is_empty(), "empty pricing catalog");
        Ok(catalog)
    }

    fn lookup(&self, name: &str) -> Option<&Price> {
        if let Some((vendor, id)) = name.split_once('/') {
            return self.models.get(id)?.iter().find(|p| p.vendor == vendor);
        }
        match self.models.get(name)?.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    pub(crate) fn price(&self, id: &str) -> Option<(f64, f64)> {
        self.lookup(id).map(|p| (p.input, p.output))
    }

    pub(crate) fn view<'a>(&self, names: impl Iterator<Item = &'a str>) -> Value {
        let prices: BTreeMap<_, _> = names
            .filter_map(|id| self.lookup(id).map(|p| (id, p)))
            .collect();
        json!({"checked":self.checked,"failed":self.failed,"prices":prices})
    }
}

async fn update(app: &App, previous: &Catalog) -> Result<Catalog> {
    let cfg = app.config.read().await.clone();
    let mut request = connections::client(&cfg, URL)?
        .get(URL)
        .timeout(Duration::from_secs(40));
    if let Some(tag) = &previous.etag {
        request = request.header("If-None-Match", tag);
    }
    let mut response = request.send().await?.error_for_status()?;
    let mut next = if response.status() == reqwest::StatusCode::NOT_MODIFIED && previous.checked > 0
    {
        previous.clone()
    } else {
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= 32 << 20,
                "pricing catalog exceeds size limit"
            );
            body.extend_from_slice(&chunk);
        }
        let mut next = Catalog::parse(&body)?;
        next.etag = etag;
        next
    };
    next.checked = now();
    next.failed = false;
    Ok(next)
}

pub async fn maintain(app: Arc<App>) {
    let path = PathBuf::from(data_dir()).join("reference-prices-v1.json");
    let mut catalog: Catalog = tokio::fs::read(&path)
        .await
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    *app.pricing.write().await = catalog.clone();
    let age = now().saturating_sub(catalog.checked);
    if (0..DAY).contains(&age) && !catalog.models.is_empty() {
        tokio::time::sleep(Duration::from_secs((DAY - age) as u64)).await;
    }
    let mut retry = 900;
    loop {
        let wait = match update(&app, &catalog).await {
            Ok(next) => {
                catalog = next;
                let tmp = path.with_extension("tmp");
                let saved: Result<()> = async {
                    tokio::fs::write(&tmp, serde_json::to_vec(&catalog)?).await?;
                    tokio::fs::rename(&tmp, &path).await?;
                    Ok(())
                }
                .await;
                if let Err(e) = saved {
                    tracing::warn!("price cache write failed: {e}");
                }
                retry = 900;
                DAY as u64
            }
            Err(e) => {
                tracing::warn!("price catalog refresh failed: {e}");
                catalog.failed = true;
                let wait = retry;
                retry = (retry * 2).min(21600);
                wait
            }
        };
        *app.pricing.write().await = catalog.clone();
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}
