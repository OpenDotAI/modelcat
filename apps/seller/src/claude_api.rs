use crate::{config::Config, keychain, state::App};
use anyhow::{anyhow, ensure, Context, Result};
use asale_client_core::{executor::{LeasedToken, TokenProvider}, protocol::HttpRequestPayload};
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderValue, Uri},
    response::Response,
};
use futures_util::TryStreamExt;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{error::Error as _, io, sync::Arc, time::Duration};

const SOURCE: &str = "node.claude_api";
const SALE_MODEL_PREFIX: &str = "claude-api/";
const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com/";
const API_VERSION: &str = "2023-06-01";
const WORKSPACE_HEADER: &str = "anthropic-workspace-id";
const OAUTH_ISSUER: &str = "https://identity.anthropic.com/users";
const OAUTH_AUTHORIZATION: &str = "https://auth.claude.com/oauth/authorize";
const OAUTH_TOKEN: &str = "https://auth.claude.com/v1/oauth/token";
const OAUTH_KEYS: &str = "https://identity.anthropic.com/users/jwks.json";

#[derive(Deserialize, Serialize)]
pub struct Source {
    key: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    checked_route: String,
    #[serde(default)]
    unavailable: bool,
}

impl Source {
    pub fn paused(&self, config: &Config) -> bool {
        self.unavailable || self.checked_route != route_id(proxy(config).as_deref())
    }

    pub(crate) fn origin(&self) -> Result<Url> {
        api_origin(self.base_url.as_deref())
    }
}

fn api_origin(base_url: Option<&str>) -> Result<Url> {
    let input = base_url.unwrap_or(ANTHROPIC_ORIGIN);
    let origin = Url::parse(input).map_err(|_| anyhow!("enter a valid Claude API address"))?;
    ensure!(
        origin.scheme() == "https" && origin.host().is_some()
            && origin.username().is_empty() && origin.password().is_none()
            && origin.path() == "/" && origin.query().is_none() && origin.fragment().is_none(),
        "use a Claude API address like https://api.example.com, without a path or credentials"
    );
    Ok(origin)
}

pub fn source() -> Result<Option<Source>> {
    keychain::get(SOURCE)?
        .map(|entry| serde_json::from_str(&entry).context("saved Claude source is invalid"))
        .transpose()
}

pub fn disconnect() -> Result<()> {
    keychain::delete(SOURCE)
}

fn proxy(config: &Config) -> Option<String> {
    match config.claude_api_proxy_setting() {
        "off" => None,
        "auto" => asale_client_core::http::auto_proxy().as_deref().map(proxy_url),
        configured => Some(proxy_url(configured)),
    }
}

fn proxy_url(url: &str) -> String {
    // Resolve the Claude destination through the proxy for saved socks5 URLs.
    match url.trim().strip_prefix("socks5://") {
        Some(address) => format!("socks5h://{address}"),
        None => url.into(),
    }
}

fn route_id(proxy: Option<&str>) -> String {
    match proxy {
        Some(url) => format!("proxy:{:x}", Sha256::digest(url.as_bytes())),
        None => "direct".into(),
    }
}

fn client(proxy: Option<&str>, origin: &Url) -> Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .https_only(origin.scheme() == "https")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    if let Some(proxy) = proxy {
        let proxy = reqwest::Proxy::all(proxy).map_err(|_| anyhow!("invalid Claude network proxy"))?;
        builder = builder.proxy(proxy);
    }
    builder.build().context("could not configure the Claude connection")
}

pub(crate) async fn check_network(app: &App) -> Result<reqwest::StatusCode> {
    probe_at(app, None).await
}

async fn probe_at(app: &App, origin_override: Option<&Url>) -> Result<reqwest::StatusCode> {
    let (config, source) = {
        let _guard = app.claude_setup.lock().await;
        (app.config.read().await.clone(), source()?)
    };
    let origin = match origin_override {
        Some(origin) => origin.clone(),
        None => api_origin(source.as_ref().and_then(|source| source.base_url.as_deref()))?,
    };
    let result = check_network_at(&config, &origin).await;
    if let (Err(_), Some(source)) = (&result, source) {
        pause_source(app, &source.revision).await?;
    }
    result
}

async fn check_network_at(config: &Config, origin: &Url) -> Result<reqwest::StatusCode> {
    let proxy = proxy(config);
    let response = client(proxy.as_deref(), origin)?
        .get(origin.join("v1/models")?)
        .query(&[("limit", "1")])
        .header("anthropic-version", API_VERSION)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| anyhow!("Claude is unreachable from this workspace"))?;
    let status = response.status();
    ensure!(!status.is_redirection(), "Claude returned an unexpected redirect");
    Ok(status)
}

pub(crate) async fn check_oauth_network(
    config: &Config,
) -> Result<(reqwest::StatusCode, reqwest::StatusCode)> {
    let discovery = Url::parse(&format!("{OAUTH_ISSUER}/.well-known/openid-configuration"))?;
    let authorization = Url::parse(OAUTH_AUTHORIZATION)?;
    let token = Url::parse(OAUTH_TOKEN)?;
    check_oauth_network_at(config, &discovery, &authorization, &token).await
}

fn probe_connection_interrupted(error: &reqwest::Error) -> bool {
    if !error.is_connect() {
        return false;
    }
    std::iter::successors(error.source(), |&cause| cause.source()).any(|cause| {
        cause.downcast_ref::<io::Error>().is_some_and(|error| {
            matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset)
        })
    })
}

async fn oauth_probe(
    request: impl Fn() -> reqwest::RequestBuilder,
    endpoint: &str,
) -> Result<reqwest::Response> {
    let send = || request().timeout(Duration::from_secs(6)).send();
    let mut result = send().await;
    if result.as_ref().is_err_and(probe_connection_interrupted) {
        result = send().await;
    }
    result.map_err(|error| {
        let reason = if error.is_timeout() {
            "connection timed out"
        } else if probe_connection_interrupted(&error) {
            "connection closed during the proxy or TLS handshake"
        } else if error.is_connect() {
            "could not connect or verify TLS"
        } else {
            "request failed"
        };
        anyhow!("{endpoint}: {reason}")
    })
}

async fn check_oauth_network_at(
    config: &Config,
    discovery: &Url,
    authorization: &Url,
    token: &Url,
) -> Result<(reqwest::StatusCode, reqwest::StatusCode)> {
    #[derive(Deserialize)]
    struct Discovery {
        issuer: String,
        authorization_endpoint: String,
        token_endpoint: String,
        jwks_uri: String,
    }

    let configured_proxy = config.dedicated_proxy()
        .map(proxy_url)
        .context("Save a dedicated workspace proxy to check Claude account authorization")?;
    let http = client(Some(&configured_proxy), discovery)?;
    let result = oauth_probe(|| http.get(discovery.clone()), "Claude identity metadata").await?;
    ensure!(result.status().is_success(), "Claude identity metadata returned HTTP {}", result.status().as_u16());
    let metadata: Discovery = result.json().await.map_err(|_| anyhow!("Claude returned invalid identity metadata"))?;
    ensure!(metadata.issuer == OAUTH_ISSUER
        && metadata.authorization_endpoint == OAUTH_AUTHORIZATION
        && metadata.token_endpoint == OAUTH_TOKEN
        && metadata.jwks_uri == OAUTH_KEYS,
        "Claude identity metadata does not match the official OAuth endpoints");

    let result = oauth_probe(|| http.get(authorization.clone()), "Claude authorization endpoint").await?;
    let authorization_status = result.status();
    ensure!(authorization_status.is_success() || authorization_status.is_redirection()
            || authorization_status == reqwest::StatusCode::BAD_REQUEST,
        "Claude authorization endpoint returned HTTP {}", authorization_status.as_u16());

    let result = oauth_probe(
        || http.post(token.clone())
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("grant_type=authorization_code"),
        "Claude token endpoint",
    ).await?;
    let token_status = result.status();
    ensure!(
        matches!(token_status, reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN),
        "Claude token endpoint returned unexpected HTTP {}", token_status.as_u16()
    );
    Ok((authorization_status, token_status))
}

fn key_header(key: &str) -> Result<HeaderValue> {
    ensure!(!key.is_empty() && key.len() <= 512, "enter a Claude API Key");
    let mut value = HeaderValue::from_str(key).map_err(|_| anyhow!("invalid Claude API Key"))?;
    value.set_sensitive(true);
    Ok(value)
}

pub async fn connect(config: &Config, key: &str, workspace_id: Option<&str>, base_url: Option<&str>) -> Result<Vec<String>> {
    let origin = api_origin(base_url.map(str::trim).filter(|value| !value.is_empty()))?;
    let relay = (origin.as_str() != ANTHROPIC_ORIGIN).then_some(origin.as_str());
    ensure!(relay.is_none() || workspace_id.is_none_or(|id| id.trim().is_empty()),
        "Claude workspace ID is only supported on the official API");
    discover_and_save(config, key, workspace_id, &origin, relay).await
}

pub async fn recheck(config: &Config) -> Result<Vec<String>> {
    recheck_at(config, None).await
}

async fn recheck_at(config: &Config, origin_override: Option<&Url>) -> Result<Vec<String>> {
    let mut source = source()?.context("Connect a Claude API Key first")?;
    let origin = match origin_override {
        Some(origin) => origin.clone(),
        None => source.origin()?,
    };
    let result = discover_and_save(config, &source.key, source.workspace_id.as_deref(), &origin, source.base_url.as_deref()).await;
    if result.is_err() {
        source.unavailable = true;
        save(&source)?;
    }
    result
}

fn save(source: &Source) -> Result<()> {
    keychain::set(SOURCE, &serde_json::to_string(source)?)
}

// The caller holds the setup lock so a concurrent connection cannot clear the pause.
pub(crate) fn require_recheck() -> Result<()> {
    if let Some(current) = source()? {
        pause(current)?;
    }
    Ok(())
}

#[derive(Debug)]
struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Claude access is paused because its workspace network route is unavailable")
    }
}

impl std::error::Error for Unavailable {}

#[cfg(test)]
async fn connect_at(config: &Config, key: &str, workspace_id: Option<&str>, origin: &Url) -> Result<Vec<String>> {
    discover_and_save(config, key, workspace_id, origin, None).await
}

async fn discover_and_save(config: &Config, key: &str, workspace_id: Option<&str>, origin: &Url, base_url: Option<&str>) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    #[derive(Deserialize)]
    struct Page {
        data: Vec<Model>,
        #[serde(default)]
        has_more: bool,
        last_id: Option<String>,
    }

    let key = key.trim();
    let header = key_header(key)?;
    let workspace_id = workspace_id.map(str::trim).filter(|id| !id.is_empty());
    if let Some(id) = workspace_id {
        ensure!(
            id.len() <= 128 && id.strip_prefix("wrkspc_").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            }),
            "invalid Claude workspace ID (expected wrkspc_…)"
        );
    }
    let proxy = proxy(config);
    let http = client(proxy.as_deref(), origin)?;
    let mut models = Vec::new();
    let mut after = None::<String>;
    for _ in 0..20 {
        let mut request = http.get(origin.join("v1/models")?)
            .header("x-api-key", header.clone())
            .header("anthropic-version", API_VERSION)
            .query(&[("limit", "100")])
            .timeout(Duration::from_secs(15));
        if let Some(id) = workspace_id {
            request = request.header(WORKSPACE_HEADER, id);
        }
        if let Some(after) = &after {
            request = request.query(&[("after_id", after)]);
        }
        let response = request.send().await.map_err(|_| anyhow!("Claude is unreachable from this workspace"))?;
        ensure!(response.status().is_success(), "Claude rejected the model list request (HTTP {})", response.status().as_u16());
        let page: Page = response.json().await.map_err(|_| anyhow!("Claude returned an invalid model list"))?;
        models.extend(page.data.into_iter().map(|model| model.id)
            .filter(|id| !id.is_empty() && (base_url.is_none() || id.starts_with("claude-"))));
        if !page.has_more {
            models.sort_unstable();
            models.dedup();
            ensure!(!models.is_empty(), "the Claude key has no available models");
            save(&Source {
                key: key.into(),
                models: models.clone(),
                workspace_id: workspace_id.map(str::to_owned),
                base_url: base_url.map(str::to_owned),
                revision: uuid::Uuid::new_v4().to_string(),
                checked_route: route_id(proxy.as_deref()),
                unavailable: false,
            })?;
            return Ok(models);
        }
        ensure!(page.last_id.as_ref().is_some_and(|id| !id.is_empty()) && page.last_id != after, "invalid Claude model pagination");
        after = page.last_id;
    }
    Err(anyhow!("Claude model list exceeded the page limit"))
}

pub(crate) async fn pause_source(app: &App, revision: &str) -> Result<()> {
    let _guard = app.claude_setup.lock().await;
    if let Some(current) = source()?.filter(|source| source.revision == revision) {
        pause(current)?;
    }
    Ok(())
}

fn pause(mut current: Source) -> Result<()> {
    if !current.unavailable {
        current.unavailable = true;
        save(&current)?;
    }
    Ok(())
}

pub(crate) fn sale_models(config: &Config) -> Vec<String> {
    source().ok().flatten().filter(|source| source.base_url.is_none() && !source.paused(config))
        .map(|source| source.models.into_iter().map(|model| format!("{SALE_MODEL_PREFIX}{model}")).collect())
        .unwrap_or_default()
}

pub(crate) struct SaleRoute {
    pub http: Client,
    pub revision: String,
    pub endpoint: String,
    key: String,
    base: String,
}

impl TokenProvider for SaleRoute {
    fn acquire(&self, provider: &str, _: &str) -> Option<LeasedToken> {
        (provider == "claude").then(|| LeasedToken {
            token: self.key.clone(),
            account_id: "claude-api".into(),
            upstream_base: Some(self.base.clone()),
            ..Default::default()
        })
    }
}

pub(crate) async fn sale_route(app: &App, request: &mut HttpRequestPayload) -> Result<SaleRoute> {
    sale_route_at(app, request, &Url::parse(ANTHROPIC_ORIGIN)?).await
}

async fn sale_route_at(app: &App, request: &mut HttpRequestPayload, origin: &Url) -> Result<SaleRoute> {
    prepare_sale(request)?;
    let _guard = app.claude_setup.lock().await;
    let config = app.config.read().await;
    let model = request.model.strip_prefix(SALE_MODEL_PREFIX).ok_or(Unavailable)?;
    let current = source()?.filter(|source| {
        source.base_url.is_none() && !source.paused(&config)
            && source.models.iter().any(|available| available == model)
    }).ok_or(Unavailable)?;
    let http = match client(proxy(&config).as_deref(), origin) {
        Ok(http) => http,
        Err(error) => {
            pause(current)?;
            return Err(error);
        }
    };
    if let Some(workspace_id) = current.workspace_id {
        request.upstream.headers.insert(WORKSPACE_HEADER.into(), workspace_id.into());
    }
    Ok(SaleRoute {
        http,
        revision: current.revision,
        endpoint: origin.authority().to_owned(),
        key: current.key,
        base: origin.join("v1")?.into(),
    })
}

pub(crate) fn prepare_sale(request: &mut HttpRequestPayload) -> Result<()> {
    let upstream = &mut request.upstream;
    ensure!(upstream.method == "POST" && Url::parse(&upstream.url)?.path() == "/v1/messages",
        "Claude sales require the Messages API");
    let mut body: serde_json::Value = serde_json::from_slice(&B64.decode(&upstream.body_b64)?)?;
    ensure!(body.is_object(), "invalid Claude request body");
    let output = body["max_tokens"].as_i64().filter(|limit| *limit > 0)
        .context("missing Claude output limit")?;
    body["max_tokens"] = output.min(request.budget_tokens).into();
    let model = request.model.strip_prefix(SALE_MODEL_PREFIX).context("unknown Claude API model")?;
    body["model"] = model.into();
    body["stream"] = request.stream.into();
    upstream.body_b64 = B64.encode(serde_json::to_vec(&body)?);
    upstream.url = Url::parse(ANTHROPIC_ORIGIN)?.join("v1/messages")?.into();
    upstream.headers.clear();
    upstream.headers.insert("content-type".into(), "application/json".into());
    upstream.headers.insert("anthropic-version".into(), API_VERSION.into());
    Ok(())
}

pub async fn forward(app: &Arc<App>, source: Source, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Response> {
    let origin = source.origin()?;
    forward_at(app, source, uri, headers, body, &origin).await
}

async fn forward_at(app: &Arc<App>, snapshot: Source, uri: Uri, headers: HeaderMap, body: Bytes, origin: &Url) -> Result<Response> {
    let revision = snapshot.revision;
    let path = match uri.path() {
        "/v1/messages" => "v1/messages",
        "/v1/messages/count_tokens" => "v1/messages/count_tokens",
        _ => return Err(anyhow!("unsupported Claude endpoint")),
    };
    let mut url = origin.join(path)?;
    url.set_query(uri.query());
    let mut request = {
        let _guard = app.claude_setup.lock().await;
        let current = source()?.filter(|source| source.revision == revision && !source.unavailable)
            .ok_or(Unavailable)?;
        let proxy = proxy(&*app.config.read().await);
        if current.checked_route != route_id(proxy.as_deref()) {
            pause(current)?;
            return Err(Unavailable.into());
        }
        let http = match client(proxy.as_deref(), origin) {
            Ok(http) => http,
            Err(error) => {
                pause(current)?;
                return Err(error);
            }
        };
        let request = http.post(url)
            .timeout(Duration::from_secs(600))
            .header("x-api-key", key_header(&current.key)?)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(id) = current.workspace_id {
            request.header(WORKSPACE_HEADER, id)
        } else {
            request
        }
    };
    if !headers.contains_key("anthropic-version") {
        request = request.header("anthropic-version", API_VERSION);
    }
    for (name, value) in &headers {
        if matches!(name.as_str(), "anthropic-version" | "anthropic-beta") {
            request = request.header(name, value);
        }
    }
    let response = match request.body(body).send().await {
        Ok(response) => response,
        Err(_) => {
            pause_source(app, &revision).await?;
            return Err(Unavailable.into());
        }
    };
    if response.status().is_redirection() {
        pause_source(app, &revision).await?;
        return Err(anyhow!("Claude returned an unexpected redirect"));
    }
    let mut downstream = Response::builder().status(response.status());
    for (name, value) in response.headers() {
        if matches!(name.as_str(), "content-type" | "retry-after" | "x-should-retry" | "request-id" | "content-encoding")
            || name.as_str().starts_with("anthropic-ratelimit-")
        {
            downstream = downstream.header(name, value);
        }
    }
    let app = app.clone();
    let revision = Arc::<str>::from(revision);
    let stream = response.bytes_stream().or_else(move |_| {
        let app = app.clone();
        let revision = revision.clone();
        async move {
            pause_source(&app, &revision).await?;
            Err::<Bytes, anyhow::Error>(Unavailable.into())
        }
    });
    Ok(downstream.body(Body::from_stream(stream))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Query,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::collections::HashMap;

    #[test]
    fn signed_claude_api_sales_use_workspace_proxy_and_pause_on_failure() {
        use crate::{config::{Binding, Offer}, runner};
        use asale_client_core::executor;
        use ed25519_dalek::{Signer, SigningKey};
        use tokio::sync::{mpsc, oneshot};

        crate::testenv::with_temp_data_dir("claude-api-sale", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let proxy = format!("http://{}", listener.local_addr().unwrap());
                let fixture = Router::new().fallback(async |uri: Uri, headers: HeaderMap, body: Bytes| {
                    if uri.path() == "/market/api/node/quotes" {
                        assert_eq!(headers["authorization"], "Bearer fixture-node-secret");
                        assert_eq!(headers["x-node-id"], "fixture-node");
                        assert!(!headers.contains_key("x-api-key"));
                        let quote: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(quote["model"], "claude-public");
                        assert_eq!(quote["provider"], "claude");
                        assert_eq!(quote["upstream_model"], "claude-api/claude-fixture");
                        return Json(json!({"id":"fixture-quote","status":"pending"}));
                    }
                    assert_eq!(uri.host(), Some("claude.fixture.invalid"));
                    assert_eq!(headers["x-api-key"], "fixture-api-key");
                    assert_eq!(headers[WORKSPACE_HEADER], "wrkspc_fixture");
                    assert!(!headers.contains_key("authorization"));
                    assert!(!headers.contains_key("anthropic-beta"));
                    if uri.path() == "/v1/models" {
                        return Json(json!({"data":[{"id":"claude-fixture"}]}));
                    }
                    assert_eq!(uri.path(), "/v1/messages");
                    assert_eq!(headers["anthropic-version"], API_VERSION);
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["model"], "claude-fixture");
                    assert_eq!(value["max_tokens"], 5);
                    assert_eq!(value["system"], "Use the supplied context.");
                    Json(json!({"type":"message","content":[{"type":"text","text":"fixture reply"}],
                        "usage":{"input_tokens":3,"output_tokens":2}}))
                });
                let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
                let origin = Url::parse("http://claude.fixture.invalid/").unwrap();
                let app = App::open_for_mode(true).await.unwrap();
                let model = "claude-api/claude-fixture";
                let offer = Offer {
                    model: "claude-public".into(), provider: "claude".into(), upstream_model: model.into(),
                    input_price_per_m: "1".into(), output_price_per_m: "2".into(), enabled: true,
                };
                let config = Config {
                    upstream_proxy: proxy.clone(),
                    paused: false,
                    offers: vec![offer.clone(), Offer {
                        model: "retired".into(), upstream_model: "claude-fixture".into(), ..offer
                    }],
                    ..Default::default()
                };
                app.save_config(config.clone()).await.unwrap();
                connect_at(&config, "fixture-api-key", Some("wrkspc_fixture"), &origin).await.unwrap();
                assert_eq!(app.available_models().await, ["claude-public"]);
                let key = SigningKey::generate(&mut rand::rngs::OsRng);
                *app.binding.write().await = Some(Binding {
                    gateway_url: proxy, node_id: "fixture-node".into(), node_secret: "fixture-node-secret".into(),
                    gateway_public_key: B64.encode(key.verifying_key().to_bytes()), currency: "USD".into(),
                });
                let quote = crate::gateway::submit_quote(app.binding.read().await.as_ref().unwrap(), &config.offers[0]).await.unwrap();
                assert_eq!(quote["status"], "pending");
                let signed_job = |id: &str| {
                    let expires = crate::state::now() + 60;
                    let body = json!({"model":"gateway-model","max_tokens":120,
                        "system":"Use the supplied context.","messages":[{"role":"user","content":"Hello"}]});
                    let payload = json!({
                        "id":id,"task_id":id,"model":model,"exp":expires,"budget_tokens":5,"stream":false,
                        "quota_sig":B64.encode(key.sign(format!("{id}|{model}|5|{expires}").as_bytes()).to_bytes()),
                        "upstream":{"provider":"claude","method":"POST","url":"https://wrong.invalid/v1/messages",
                            "headers":{"authorization":"Bearer fixture","x-api-key":"wrong","anthropic-beta":"oauth"},
                            "body_b64":B64.encode(body.to_string())}
                    });
                    let bytes = serde_json::to_vec(&payload).unwrap();
                    runner::SignedJob {
                        public_model: "claude-public".into(), request_b64: B64.encode(&bytes),
                        signature: B64.encode(key.sign(&bytes).to_bytes()),
                    }
                };
                let (mut payload, verifier) = runner::verify_job(&app, "success", signed_job("success")).await.unwrap();
                let route = sale_route_at(&app, &mut payload, &origin).await.unwrap();
                let (_cancel, cancel_rx) = oneshot::channel();
                let (out, mut events) = mpsc::unbounded_channel();
                executor::execute(&route.http, &route, payload, &out, None, &verifier, cancel_rx).await;
                let response = events.try_recv().unwrap();
                assert_eq!(response.msg_type, "http_response");
                assert_eq!(response.payload["usage"]["output_tokens"], 2);
                assert_eq!(response.payload["usage"]["input_tokens"], 3);

                server.abort();
                let _ = server.await;
                let (payload, verifier) = runner::verify_job(&app, "failed", signed_job("failed")).await.unwrap();
                let (_cancel, cancel_rx) = oneshot::channel();
                let (out, mut events) = mpsc::channel(8);
                runner::execute(app.clone(), payload, verifier, out, cancel_rx, false).await;
                assert_eq!(events.try_recv().unwrap().payload["code"], "UPSTREAM_5XX");
                assert!(source().unwrap().unwrap().paused(&config));
                assert!(app.available_models().await.is_empty());
                assert!(runner::verify_job(&app, "after", signed_job("after")).await.is_err());
            });
        });
    }

    #[tokio::test]
    async fn claude_socks_proxy_resolves_the_destination() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Config {
            upstream_proxy: format!("socks5://{}", listener.local_addr().unwrap()),
            ..Config::default()
        };
        let hostname = "claude.fixture.invalid";
        let origin = Url::parse(&format!("https://{hostname}/")).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            socket.write_all(&[5, 0]).await.unwrap();

            let mut connect = [0; 5];
            socket.read_exact(&mut connect).await.unwrap();
            assert_eq!(connect, [5, 1, 0, 3, hostname.len() as u8]);
            let mut address = vec![0; hostname.len()];
            socket.read_exact(&mut address).await.unwrap();
            assert_eq!(address, hostname.as_bytes());
            assert_eq!(socket.read_u16().await.unwrap(), 443);
            socket.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(4), async {
            assert!(check_network_at(&config, &origin).await.is_err());
            server.await.unwrap();
        }).await.unwrap();
    }

    #[tokio::test]
    async fn oauth_probe_retries_a_tls_disconnect_on_the_same_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let origin = Url::parse("https://claude.fixture.invalid/").unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                assert!(request.starts_with(b"CONNECT claude.fixture.invalid:443 HTTP/1.1\r\n"));
                socket.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                assert_eq!(socket.read_u8().await.unwrap(), 22); // TLS handshake record
                socket.shutdown().await.unwrap();
            }
        });
        let http = client(Some(&proxy), &origin).unwrap();
        tokio::time::timeout(Duration::from_secs(4), async {
            let error = oauth_probe(|| http.get(origin.clone()), "Claude identity metadata").await.unwrap_err();
            assert!(error.to_string().contains("connection closed during the proxy or TLS handshake"));
            server.await.unwrap();
        }).await.unwrap();
    }

    #[tokio::test]
    async fn oauth_probe_uses_workspace_proxy_and_checks_metadata_before_authorization() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let valid = Arc::new(AtomicBool::new(true));
        let auth_requests = Arc::new(AtomicUsize::new(0));
        let token_requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Config {
            upstream_proxy: format!("http://{}", listener.local_addr().unwrap()),
            claude_api_direct: true,
            ..Config::default()
        };
        let fixture = Router::new().fallback({
            let valid = valid.clone();
            let auth_requests = auth_requests.clone();
            let token_requests = token_requests.clone();
            move |method: axum::http::Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let valid = valid.clone();
                let auth_requests = auth_requests.clone();
                let token_requests = token_requests.clone();
                async move {
                    assert_eq!(uri.host(), Some("claude.fixture.invalid"));
                    assert!(!headers.contains_key("authorization"));
                    assert!(!headers.contains_key("x-api-key"));
                    match uri.path() {
                        "/metadata" => Json(json!({
                            "issuer": if valid.load(Ordering::SeqCst) { OAUTH_ISSUER } else { "https://wrong.invalid" },
                            "authorization_endpoint": OAUTH_AUTHORIZATION,
                            "token_endpoint": OAUTH_TOKEN,
                            "jwks_uri": OAUTH_KEYS,
                        })).into_response(),
                        "/authorize" => {
                            auth_requests.fetch_add(1, Ordering::SeqCst);
                            (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, "http://claude.fixture.invalid/unexpected")]).into_response()
                        }
                        "/token" => {
                            token_requests.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(method, axum::http::Method::POST);
                            assert_eq!(headers[header::CONTENT_TYPE], "application/x-www-form-urlencoded");
                            assert_eq!(&body[..], b"grant_type=authorization_code");
                            StatusCode::FORBIDDEN.into_response()
                        }
                        _ => panic!("unexpected OAuth probe path: {uri}"),
                    }
                }
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
        let metadata = Url::parse("http://claude.fixture.invalid/metadata").unwrap();
        let authorization = Url::parse("http://claude.fixture.invalid/authorize").unwrap();
        let token = Url::parse("http://claude.fixture.invalid/token").unwrap();
        for mode in ["auto", "off"] {
            let config = Config { upstream_proxy: mode.into(), ..Config::default() };
            let error = check_oauth_network_at(&config, &metadata, &authorization, &token).await.unwrap_err();
            assert!(error.to_string().contains("Save a dedicated workspace proxy"));
        }
        assert_eq!(
            check_oauth_network_at(&config, &metadata, &authorization, &token).await.unwrap(),
            (StatusCode::TEMPORARY_REDIRECT, StatusCode::FORBIDDEN)
        );
        valid.store(false, Ordering::SeqCst);
        let error = check_oauth_network_at(&config, &metadata, &authorization, &token).await.unwrap_err();
        assert!(error.to_string().contains("metadata does not match"));
        assert_eq!(auth_requests.load(Ordering::SeqCst), 1);
        assert_eq!(token_requests.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn workspace_proxy_is_used_for_claude_and_switching_back_requires_recheck() {
        use axum::extract::State;
        use crate::config::ProxyMode;
        use std::sync::atomic::{AtomicUsize, Ordering};

        crate::testenv::with_temp_data_dir("claude-workspace-proxy", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let configured_proxy = format!("http://{}", listener.local_addr().unwrap());
                let requests = Arc::new(AtomicUsize::new(0));
                let proxy_fixture = Router::new().fallback(async |
                    State(requests): State<Arc<AtomicUsize>>,
                    uri: Uri,
                    headers: HeaderMap,
                | {
                    assert_eq!(uri.host(), Some("claude.fixture.invalid"));
                    if uri.query() == Some("limit=1") {
                        assert_eq!(uri.path(), "/v1/models");
                        assert_eq!(headers["anthropic-version"], API_VERSION);
                        assert!(!headers.contains_key("x-api-key"));
                        assert!(!headers.contains_key("authorization"));
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    assert_eq!(headers["x-api-key"], "provider-test-key");
                    assert!(!headers.contains_key("authorization"));
                    assert!(!headers.contains_key(WORKSPACE_HEADER));
                    requests.fetch_add(1, Ordering::SeqCst);
                    match uri.path() {
                        "/v1/models" => Json(json!({"data":[{"id":"claude-first"}]})).into_response(),
                        "/v1/messages" => Json(json!({"type":"message","id":"via-proxy"})).into_response(),
                        _ => StatusCode::NOT_FOUND.into_response(),
                    }
                }).with_state(requests.clone());
                let proxy_server = tokio::spawn(async move { axum::serve(listener, proxy_fixture).await.unwrap() });
                let origin = Url::parse("http://claude.fixture.invalid/").unwrap();
                let app = App::open().await.unwrap();
                let legacy: Config = serde_json::from_value(json!({"upstream_proxy":configured_proxy})).unwrap();
                assert_eq!(legacy.proxy_mode, ProxyMode::Rules);
                assert_eq!(legacy.claude_api_proxy_setting(), configured_proxy);
                let direct = Config { claude_api_direct: true, ..legacy };
                assert!(proxy(&direct).is_none());
                let through_proxy = Config { proxy_mode: ProxyMode::Global, ..direct.clone() };
                assert!(Config { upstream_proxy: "auto".into(), ..through_proxy.clone() }.validate().is_err());
                assert!(Config { upstream_proxy: "off".into(), ..through_proxy.clone() }.validate().is_err());
                app.save_config(through_proxy.clone()).await.unwrap();
                assert_eq!(connect_at(&through_proxy, "provider-test-key", None, &origin).await.unwrap(), ["claude-first"]);
                let call = || forward_at(&app, source().unwrap().unwrap(), "/v1/messages".parse().unwrap(),
                    HeaderMap::new(), Bytes::from_static(br#"{"model":"claude-first"}"#), &origin);
                let response = call().await.unwrap();
                let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["id"], "via-proxy");
                assert_eq!(requests.load(Ordering::SeqCst), 2);
                let before_switch = source().unwrap().unwrap();

                app.save_config(direct).await.unwrap();
                app.save_config(through_proxy.clone()).await.unwrap();
                assert_eq!(probe_at(&app, Some(&origin)).await.unwrap(), StatusCode::UNAUTHORIZED);
                assert!(call().await.unwrap_err().is::<Unavailable>());
                assert_eq!(requests.load(Ordering::SeqCst), 2);
                assert_eq!(recheck_at(&through_proxy, Some(&origin)).await.unwrap(), ["claude-first"]);
                let stale = forward_at(&app, before_switch, "/v1/messages".parse().unwrap(),
                    HeaderMap::new(), Bytes::from_static(br#"{"model":"claude-first"}"#), &origin).await;
                assert!(stale.unwrap_err().is::<Unavailable>());
                assert_eq!(requests.load(Ordering::SeqCst), 3);
                assert_eq!(call().await.unwrap().status(), StatusCode::OK);
                assert_eq!(requests.load(Ordering::SeqCst), 4);
                proxy_server.abort();
                let _ = proxy_server.await;
                assert!(probe_at(&app, Some(&origin)).await.is_err());
                assert!(source().unwrap().unwrap().paused(&through_proxy));
                assert!(call().await.unwrap_err().is::<Unavailable>());
            });
        });
    }

    #[test]
    fn validates_before_replacing_key_and_forwards_claude_requests() {
        const WORKSPACE: &str = "wrkspc_fixtureA";
        crate::testenv::with_temp_data_dir("claude-api-key", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let fixture = Router::new()
                    .route("/v1/models", get(models))
                    .route("/v1/messages", post(messages))
                    .route("/v1/messages/count_tokens", post(count_tokens));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
                let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
                let config = Config { upstream_proxy: "off".into(), ..Config::default() };
                let app = App::open().await.unwrap();
                app.save_config(config.clone()).await.unwrap();

                assert!(connect_at(&config, "rejected-key", Some(WORKSPACE), &origin).await.is_err());
                assert!(connect_at(&config, "provider-test-key", None, &origin).await.is_err());
                let invalid = connect_at(&config, "provider-test-key", Some("wrkspc_???"), &origin).await.unwrap_err();
                assert!(invalid.to_string().contains("invalid Claude workspace ID"));
                assert!(source().unwrap().is_none());
                let listed = connect_at(&config, "provider-test-key", Some(WORKSPACE), &origin).await.unwrap();
                assert_eq!(listed, ["claude-first", "claude-second"]);
                assert!(connect_at(&config, "rejected-key", Some("wrkspc_other"), &origin).await.is_err());
                assert_eq!(source().unwrap().unwrap().key, "provider-test-key");
                assert_eq!(source().unwrap().unwrap().workspace_id.as_deref(), Some(WORKSPACE));

                let mut headers = HeaderMap::new();
                headers.insert("authorization", HeaderValue::from_static("Bearer local-only-key"));
                headers.insert("x-api-key", HeaderValue::from_static("local-only-key"));
                headers.insert("anthropic-beta", HeaderValue::from_static("fixture-beta"));
                headers.insert(WORKSPACE_HEADER, HeaderValue::from_static("wrkspc_localOverride"));
                let query = "/v1/messages?beta=true".parse().unwrap();
                let body = Bytes::from_static(br#"{"model":"claude-first","stream":true}"#);
                let response = forward_at(&app, source().unwrap().unwrap(), query, headers.clone(), body.clone(), &origin).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()["content-type"], "text/event-stream");
                let events = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                assert!(events.starts_with(b"event: message_stop"));

                let uri = "/v1/messages?beta=true&disconnect=true".parse().unwrap();
                let response = forward_at(&app, source().unwrap().unwrap(), uri, headers.clone(), body.clone(), &origin).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert!(!source().unwrap().unwrap().paused(&config));
                let mut stream = response.into_body().into_data_stream();
                let first = stream.try_next().await.unwrap().unwrap();
                assert!(first.starts_with(b"event: message_start"));
                assert!(stream.try_next().await.is_err());
                assert!(source().unwrap().unwrap().paused(&config));
                assert_eq!(recheck_at(&config, Some(&origin)).await.unwrap(), listed);

                let uri = "/v1/messages/count_tokens".parse().unwrap();
                let response = forward_at(&app, source().unwrap().unwrap(), uri, headers.clone(), body.clone(), &origin).await.unwrap();
                assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(response.headers()["retry-after"], "2");
                assert!(!source().unwrap().unwrap().paused(&config));

                let unreachable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let dead_origin = Url::parse(&format!("http://{}", unreachable.local_addr().unwrap())).unwrap();
                drop(unreachable);
                let failure = forward_at(&app, source().unwrap().unwrap(), "/v1/messages".parse().unwrap(),
                    headers.clone(), body.clone(), &dead_origin).await.unwrap_err();
                assert!(failure.is::<Unavailable>());
                assert!(source().unwrap().unwrap().paused(&config));
                let blocked = forward_at(&app, source().unwrap().unwrap(), "/v1/messages?beta=true".parse().unwrap(),
                    headers.clone(), body.clone(), &origin).await.unwrap_err();
                assert!(blocked.is::<Unavailable>());
                assert_eq!(recheck_at(&config, Some(&origin)).await.unwrap(), listed);
                assert!(!source().unwrap().unwrap().paused(&config));
                assert_eq!(forward_at(&app, source().unwrap().unwrap(), "/v1/messages?beta=true".parse().unwrap(),
                    headers, body, &origin).await.unwrap().status(), StatusCode::OK);

                let stopped_proxy = Config { upstream_proxy: dead_origin.to_string(), ..config.clone() };
                assert!(source().unwrap().unwrap().paused(&stopped_proxy));
                assert!(recheck_at(&stopped_proxy, Some(&origin)).await.is_err());
                assert!(source().unwrap().unwrap().paused(&config));
                assert_eq!(source().unwrap().unwrap().key, "provider-test-key");
                assert_eq!(recheck_at(&config, Some(&origin)).await.unwrap(), listed);

                let bad_proxy = Config { upstream_proxy: "https://".into(), ..Config::default() };
                assert!(connect_at(&bad_proxy, "provider-test-key", Some(WORKSPACE), &origin).await.is_err());
                disconnect().unwrap();
                assert!(source().unwrap().is_none());
                server.abort();
            });
        });
    }

    async fn models(headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Response {
        if headers["x-api-key"] != "provider-test-key" {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if headers.get(WORKSPACE_HEADER).is_none_or(|value| value != "wrkspc_fixtureA") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        assert_eq!(headers["anthropic-version"], API_VERSION);
        if query.contains_key("after_id") {
            assert_eq!(query["after_id"], "claude-first");
            Json(json!({"data":[{"id":"claude-second"}], "has_more":false})).into_response()
        } else {
            Json(json!({"data":[{"id":"claude-first"}], "has_more":true, "last_id":"claude-first"})).into_response()
        }
    }

    fn assert_forwarded(headers: &HeaderMap, body: &Value) {
        assert_eq!(headers["x-api-key"], "provider-test-key");
        assert!(!headers.contains_key("authorization"));
        assert_eq!(headers["anthropic-version"], API_VERSION);
        assert_eq!(headers["anthropic-beta"], "fixture-beta");
        assert_eq!(headers.get_all(WORKSPACE_HEADER).iter().collect::<Vec<_>>(), ["wrkspc_fixtureA"]);
        assert_eq!(body["model"], "claude-first");
    }

    async fn messages(headers: HeaderMap, Query(query): Query<HashMap<String, String>>, Json(body): Json<Value>) -> Response {
        assert_forwarded(&headers, &body);
        assert_eq!(query["beta"], "true");
        if query.contains_key("disconnect") {
            use futures_util::{stream, StreamExt};
            let start = Bytes::from_static(b"event: message_start\ndata: {}\n\n");
            let error = stream::once(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
            });
            let events = stream::iter([Ok(start)]).chain(error);
            return ([("content-type", "text/event-stream")], Body::from_stream(events)).into_response();
        }
        ([("content-type", "text/event-stream")], "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n").into_response()
    }

    async fn count_tokens(headers: HeaderMap, Json(body): Json<Value>) -> Response {
        assert_forwarded(&headers, &body);
        (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "2")], Json(json!({"type":"error"}))).into_response()
    }
}
