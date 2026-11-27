use crate::{
    accounts::Account,
    config::{Config, Offer},
    state::App,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    id: String,
    upstream_id: String,
    provider: String,
    label: String,
    source_model: Option<String>,
    local_pricing: bool,
}

impl Candidate {
    fn new(id: &str, upstream: &str, provider: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            upstream_id: upstream.into(),
            provider: provider.into(),
            label: label.into(),
            source_model: None,
            local_pricing: provider == "custom",
        }
    }
}

pub(crate) fn candidates(
    config: &Config,
    accounts: &[Account],
    connections: &[Value],
    ollama: &[String],
    claude: &Value,
    workspace: bool,
) -> Vec<Candidate> {
    let mut rows: Vec<_> = ollama
        .iter()
        .map(|id| Candidate::new(id, id, "custom", "Ollama"))
        .collect();
    if workspace
        && claude["paused"] == false
        && claude["base_url"].as_str().unwrap_or("").is_empty()
    {
        for id in claude["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            rows.push(Candidate::new(
                id,
                &format!("claude-api/{id}"),
                "claude",
                "Claude API Key",
            ));
        }
    }
    for account in accounts {
        for id in &account.models {
            if !rows
                .iter()
                .any(|r| r.id == *id && r.provider == account.provider)
            {
                rows.push(Candidate::new(id, id, &account.provider, &account.provider));
            }
        }
    }
    for connection in connections.iter().filter(|c| c["oauth"] != true) {
        for model in connection["models"].as_array().into_iter().flatten() {
            let (Some(name), Some(upstream), Some(provider), Some(connection_id)) = (
                model["name"].as_str(),
                model["id"].as_str(),
                connection["offer_provider"].as_str(),
                connection["id"].as_str(),
            ) else {
                continue;
            };
            let id = config
                .offers
                .iter()
                .find(|o| o.upstream_model == upstream)
                .map(|o| o.model.clone())
                .unwrap_or_else(|| {
                    format!(
                        "{}-{}",
                        name.chars().take(100).collect::<String>(),
                        connection_id.chars().take(8).collect::<String>()
                    )
                });
            let mut row = Candidate::new(
                &id,
                upstream,
                provider,
                connection["label"].as_str().unwrap_or(""),
            );
            row.source_model = Some(name.into());
            row.local_pricing = connection["protocol"] == "ollama";
            rows.push(row);
        }
    }
    rows
}

#[derive(Deserialize)]
pub(crate) struct Edit {
    model: String,
    upstream: Option<String>,
    input: Option<String>,
    output: Option<String>,
}

pub(crate) async fn save(app: &App, edits: Vec<Edit>) -> Result<()> {
    ensure!(
        !edits.is_empty() && edits.len() <= 128,
        "请选择 1 至 128 个模型"
    );
    let _guard = app.mutation.lock().await;
    let mut cfg = app.config.read().await.clone();
    let accounts = if app.vm_workspace {
        vec![]
    } else {
        app.accounts.read().await.clone()
    };
    let connections = crate::connections::views()?;
    let ollama: Vec<_> = app
        .ollama_models
        .read()
        .await
        .iter()
        .map(|m| m.id())
        .collect();
    let claude = super::control::claude_view(&cfg);
    let rows = candidates(
        &cfg,
        &accounts,
        &connections,
        &ollama,
        &claude,
        app.vm_workspace,
    );
    let catalog = app.pricing.read().await;
    let mut seen = std::collections::HashSet::new();
    for edit in edits {
        ensure!(seen.insert(edit.model.clone()), "一个模型只能提交一项报价");
        let mut matches = rows.iter().filter(|r| {
            r.id == edit.model && edit.upstream.as_ref().is_none_or(|id| *id == r.upstream_id)
        });
        let row = matches
            .next()
            .context("找不到可报价的模型，请刷新模型列表")?;
        ensure!(matches.next().is_none(), "请指定上游模型 ID");
        let reference = if row.local_pricing {
            None
        } else {
            catalog.price(row.source_model.as_ref().unwrap_or(&row.id))
        };
        let price = |value: Option<String>, fallback: Option<f64>| {
            value
                .filter(|v| !v.trim().is_empty())
                .or_else(|| fallback.map(|v| v.to_string()))
                .context("缺少参考价，请填写单价")
        };
        let mut offer = Offer {
            model: row.id.clone(),
            provider: row.provider.clone(),
            upstream_model: row.upstream_id.clone(),
            input_price_per_m: price(edit.input, reference.map(|pair| pair.0))?,
            output_price_per_m: price(edit.output, reference.map(|pair| pair.1))?,
            enabled: true,
        };
        if let Some(old) = cfg.offers.iter_mut().find(|o| o.model == offer.model) {
            offer.enabled = old.enabled;
            *old = offer;
        } else {
            cfg.offers.push(offer);
        }
    }
    drop(catalog);
    app.save_config_locked(cfg).await
}
