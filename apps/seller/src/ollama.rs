use crate::state::App;
use anyhow::{anyhow, ensure, Result};
use asale_client_core::{
    executor::{LeasedToken, TokenProvider},
    protocol::HttpRequestPayload,
};
use axum::{body::Body, http::Uri, response::Response};
use reqwest::{redirect::Policy, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

#[derive(Clone, Deserialize, Serialize)]
pub struct Model {
    pub origin: String,
    pub name: String,
}

impl Model {
    pub fn id(&self) -> String {
        format!("ollama/{}", self.name)
    }
}

pub(crate) fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("Ollama HTTP client")
    })
}

pub fn origin(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow!("enter an Ollama HTTP or HTTPS address"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "enter an Ollama origin without credentials, path, query or fragment"
    );
    Ok(url)
}

pub async fn discover(origin: &Url) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Tag {
        name: String,
    }
    #[derive(Deserialize)]
    struct Tags {
        models: Vec<Tag>,
    }

    let response = client()
        .get(origin.join("api/tags")?)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|_| anyhow!("the workspace cannot reach Ollama at this address"))?;
    ensure!(response.status().is_success(), "Ollama returned HTTP {}", response.status().as_u16());
    let tags: Tags = response.json().await.map_err(|_| anyhow!("the address did not return an Ollama model list"))?;
    let mut models: Vec<_> = tags.models.into_iter().map(|tag| tag.name).filter(|name| !name.is_empty()).collect();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

pub async fn register(app: &App, model: Model) -> Result<()> {
    ensure!(discover(&origin(&model.origin)?).await?.contains(&model.name), "Ollama model was not found");
    let _guard = app.mutation.lock().await;
    let id = model.id();
    ensure!(
        !app.config.read().await.offers.iter().any(|offer| conflicts(offer, &id)),
        "this Ollama model ID conflicts with an existing offer"
    );
    let mut next = app.ollama_models.read().await.clone();
    next.retain(|entry| entry.name != model.name);
    ensure!(next.len() < 128, "too many registered Ollama models");
    next.push(model);
    app.store.set_setting("node.ollama_models", &serde_json::to_string(&next)?).await?;
    *app.ollama_models.write().await = next;
    Ok(())
}

pub(crate) fn conflicts(offer: &crate::config::Offer, id: &str) -> bool {
    (offer.model == id || offer.upstream_model == id)
        && (offer.provider != "custom" || offer.upstream_model != id)
}

pub(crate) async fn sale_model(app: &App, provider: &str, id: &str) -> Option<Model> {
    if provider != "custom" {
        return None;
    }
    app.ollama_models.read().await.iter().find(|model| model.id() == id).cloned()
}

pub(crate) fn prepare_sale(payload: &mut HttpRequestPayload) -> Result<()> {
    ensure!(payload.upstream.method == "POST", "Ollama inference requires POST");
    let url = Url::parse(&payload.upstream.url)?;
    ensure!(matches!(url.path(), "/v1/messages" | "/v1/responses" | "/v1/chat/completions"),
        "unsupported Ollama inference endpoint");
    // Only the API dialect comes from the gateway. The registered origin and
    // model determine the connection and no gateway-supplied header travels on it.
    payload.upstream.url = format!("https://placeholder.invalid{}", url.path());
    payload.upstream.headers.clear();
    payload.upstream.headers.insert("content-type".into(), Value::String("application/json".into()));
    Ok(())
}

pub(crate) struct SaleToken(pub(crate) Model);

impl TokenProvider for SaleToken {
    fn acquire(&self, provider: &str, model: &str) -> Option<LeasedToken> {
        if provider != "custom" || model != self.0.id() {
            return None;
        }
        let base = origin(&self.0.origin).ok()?.join("v1").ok()?;
        Some(LeasedToken {
            account_id: self.0.id(),
            upstream_base: Some(base.into()),
            upstream_model: Some(self.0.name.clone()),
            ..Default::default()
        })
    }
}

pub async fn forward(model: Model, uri: &Uri, mut body: Value) -> Result<Response> {
    let path = match uri.path() {
        "/v1/messages" => "v1/messages",
        "/v1/messages/count_tokens" => "v1/messages/count_tokens",
        "/v1/responses" => "v1/responses",
        _ => return Err(anyhow!("unsupported Ollama model endpoint")),
    };
    body["model"] = Value::String(model.name);
    let upstream = client()
        .post(origin(&model.origin)?.join(path)?)
        .header("anthropic-version", "2023-06-01")
        .timeout(Duration::from_secs(600))
        .json(&body)
        .send()
        .await
        .map_err(|_| anyhow!("Ollama inference endpoint is unavailable"))?;
    let mut response = Response::builder().status(upstream.status());
    if let Some(content_type) = upstream.headers().get("content-type") {
        response = response.header("content-type", content_type);
    }
    Ok(response.body(Body::from_stream(upstream.bytes_stream()))?)
}
