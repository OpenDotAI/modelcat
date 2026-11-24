pub(crate) mod codex;
mod codex_browser;
use crate::{
    config::Config,
    keychain,
    state::{App, FailureAt, LocalRecord},
};
use anyhow::{anyhow, ensure, Context, Result};
use asale_client_core::{
    executor::{LeasedToken, TokenProvider},
    protocol::HttpRequestPayload,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(crate) const TRIAL_OUTPUT_TOKENS: i64 = 4096;
const CONNECTIONS: &str = "node.model_connections";
const MAX_CATALOG_BYTES: usize = 2 << 20;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
    ChatCompletions,
    Messages,
    Responses,
    Ollama,
}

impl Protocol {
    fn offer_provider(self) -> &'static str {
        match self {
            Self::Messages => "claude",
            Self::ChatCompletions | Self::Responses | Self::Ollama => "custom",
        }
    }

    fn resource(self) -> &'static str {
        match self {
            Self::ChatCompletions | Self::Ollama => "chat/completions",
            Self::Messages => "messages",
            Self::Responses => "responses",
        }
    }

    fn authenticate(self, request: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        match self {
            Self::ChatCompletions | Self::Responses => {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", key))
                    .expect("valid authorization header");
                value.set_sensitive(true);
                request.header(header::AUTHORIZATION, value)
            }
            Self::Ollama => request,
            Self::Messages => {
                let mut value = HeaderValue::from_str(key).expect("validated API key");
                value.set_sensitive(true);
                request
                    .header("x-api-key", value)
                    .header("anthropic-version", "2023-06-01")
            }
        }
    }

    fn test_body(self, prompt: String) -> Value {
        if self == Self::Responses {
            return json!({"input":prompt,"stream":false,"store":false,"max_output_tokens":TRIAL_OUTPUT_TOKENS});
        }
        let mut body = json!({"messages":[{"role":"user","content":prompt}],"stream":false});
        if matches!(self, Self::Messages | Self::Ollama) {
            body["max_tokens"] = json!(TRIAL_OUTPUT_TOKENS);
        }
        body
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Connection {
    id: String,
    label: String,
    base: String,
    protocol: Protocol,
    models: Vec<String>,
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<codex::Credential>,
}

#[derive(Deserialize)]
pub(crate) struct ConnectionInput {
    label: String,
    base: String,
    protocol: Protocol,
    #[serde(default)]
    key: String,
}

fn saved() -> Result<Vec<Connection>> {
    keychain::get(CONNECTIONS)?
        .map(|raw| serde_json::from_str(&raw).context("无法读取已保存的模型连接"))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn persist(connections: &[Connection]) -> Result<()> {
    keychain::set(CONNECTIONS, &serde_json::to_string(connections)?)
}

fn local_service_host(host: &str) -> bool {
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unique_local(),
        Err(_) => matches!(host, "localhost" | "host.lima.internal" | "host.docker.internal") || host.ends_with(".localhost"),
    }
}

fn base_url(raw: &str) -> Result<Url> {
    let mut base = Url::parse(raw.trim()).map_err(|_| anyhow!("请输入有效的服务地址（含 http:// 或 https://）"))?;
    let host = base.host_str().context("请输入服务主机地址")?;
    let local = local_service_host(host);
    ensure!(base.scheme() == "https" || (base.scheme() == "http" && local),
        "公网服务请使用 HTTPS。本机和内网 IP 支持 HTTP。");
    ensure!(base.username().is_empty() && base.password().is_none() && base.query().is_none() && base.fragment().is_none(),
        "服务地址不能包含凭据、查询参数或片段");
    if base.path() == "/" {
        base.set_path("/v1/");
    } else if !base.path().ends_with('/') {
        base.path_segments_mut()
            .expect("validated service URL")
            .push("");
    }
    Ok(base)
}

pub(crate) fn client(config: &Config, base: &str) -> Result<Client> {
    let http_local = base_url(base)?.scheme() == "http";
    let proxy = match config.upstream_proxy.as_str() {
        _ if http_local => None,
        "off" => None,
        "auto" if std::env::var_os("MODELCAT_VM_WORKSPACE").is_some() => {
            let path = std::path::Path::new(&crate::state::data_dir()).join("automatic-proxy");
            match std::fs::read_to_string(path) {
                Ok(url) if !url.is_empty() => Some(url),
                Ok(_) => None,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => asale_client_core::http::auto_proxy(),
                Err(err) => return Err(err).context("无法读取工作区自动代理"),
            }
        }
        "auto" => asale_client_core::http::auto_proxy(),
        url => Some(url.to_owned()),
    };
    let mut builder = Client::builder()
        .https_only(!http_local)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120));
    if let Some(proxy) = proxy {
        let url = proxy
            .strip_prefix("socks5://")
            .map_or(proxy.clone(), |address| format!("socks5h://{address}"));
        builder =
            builder.proxy(reqwest::Proxy::all(url).map_err(|_| anyhow!("工作区代理地址无效"))?);
    }
    Ok(builder.build()?)
}

impl Connection {
    fn model_id(&self, index: usize) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.models[index].as_bytes());
        format!("connection/{}/{:x}", self.id, digest)
    }

    fn view(&self) -> Value {
        let models: Vec<_> = self
            .models
            .iter()
            .enumerate()
            .map(|(index, name)| json!({"id": self.model_id(index), "name": name}))
            .collect();
        let plan = self.oauth.as_ref()
            .and_then(|_| asale_client_core::cli_import::jwt_claims(&self.key))
            .and_then(|claims| claims["https://api.openai.com/auth"]["chatgpt_plan_type"]
                .as_str().filter(|s| s.len() <= 40 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                .map(str::to_owned));
        json!({"plan":plan,"id":self.id,"label":self.label,"base":self.base.as_str(),"protocol":self.protocol,"oauth":self.oauth.is_some(),"offer_provider":self.protocol.offer_provider(),"models":models})
    }

    async fn discover(&self, config: &Config) -> Result<Vec<String>> {
        if self.oauth.is_some() { return codex::discover(self, config).await; }
        if matches!(self.protocol, Protocol::Ollama) {
            return crate::ollama::discover(&crate::ollama::origin(&self.base)?)
                .await.context("工作区无法读取 Ollama 的模型列表。请确认服务已启动，再检查地址。");
        }
        let url = Url::parse(&self.base)?.join("models")?;
        let host = url.host_str().unwrap_or_default();
        let local = local_service_host(host) || host.ends_with(".local");
        let response = self
            .protocol
            .authenticate(
                client(config, &self.base)?.get(url),
                &self.key,
            )
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|error| {
                let cause = std::error::Error::source(&error);
                let network_error = std::iter::successors(cause, |&e| e.source())
                    .filter_map(|e| e.downcast_ref::<std::io::Error>())
                    .any(|e| matches!(e.kind(), std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable
                        | std::io::ErrorKind::PermissionDenied));
                if local && (error.is_timeout() || network_error) {
                    anyhow!(LocalServiceUnavailable)
                } else {
                    anyhow!("工作区无法连接此服务")
                }
            })?;
        if matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(anyhow!("服务拒绝了 API Key。请检查密钥是否有效，以及是否有读取模型的权限。"));
        }
        ensure!(response.status().is_success(), "读取模型列表失败（HTTP {}）", response.status().as_u16());
        let mut stream = response.bytes_stream();
        let mut data = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| anyhow!("模型列表读取失败"))?;
            ensure!(
                data.len() + chunk.len() <= MAX_CATALOG_BYTES,
                "模型列表过大"
            );
            data.extend_from_slice(&chunk);
        }
        let listing: Value =
            serde_json::from_slice(&data).map_err(|_| anyhow!("服务返回的模型列表格式不受支持"))?;
        let mut models: Vec<String> = listing["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["id"].as_str())
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .map(str::to_owned)
            .collect();
        models.sort_unstable();
        models.dedup();
        ensure!(!models.is_empty(), "此连接未返回可用模型");
        Ok(models)
    }
}

pub(crate) fn views() -> Result<Vec<Value>> {
    Ok(saved()?.iter().map(Connection::view).collect())
}

pub(crate) async fn list() -> Response {
    match views() {
        Ok(connections) => Json(json!({"connections":connections})).into_response(),
        Err(error) => failure(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

#[derive(Debug)]
struct LocalServiceUnavailable;

impl std::fmt::Display for LocalServiceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("工作区暂时无法连接本地服务")
    }
}

impl std::error::Error for LocalServiceUnavailable {}

#[derive(Default, Deserialize)]
pub(crate) struct ConnectionTest {
    #[serde(default)]
    probe_only: bool,
}

pub(crate) async fn connect(
    State(app): State<Arc<App>>,
    Query(test): Query<ConnectionTest>,
    Json(input): Json<ConnectionInput>,
) -> Response {
    test_and_save(&app, None, input, test).await
}

pub(crate) async fn update(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(test): Query<ConnectionTest>,
    Json(input): Json<ConnectionInput>,
) -> Response {
    test_and_save(&app, Some(id), input, test).await
}

async fn test_and_save(app: &App, id: Option<String>, input: ConnectionInput, test: ConnectionTest) -> Response {
    match if test.probe_only {
        tested_connection(app, id, input).await.map(|(connection, _)| connection)
    } else {
        save_connection(app, id, input).await
    } {
        Ok(connection) => Json(connection.view()).into_response(),
        Err(error) if error.is::<LocalServiceUnavailable>() => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"local_service_unreachable","message":error.to_string()}})),
        ).into_response(),
        Err(error) => failure(StatusCode::BAD_REQUEST, error),
    }
}

async fn tested_connection(
    app: &App,
    id: Option<String>,
    input: ConnectionInput,
) -> Result<(Connection, Option<Connection>)> {
    let original = id
        .as_ref()
        .map(|id| {
            saved()?
                .into_iter()
                .find(|entry| entry.id == *id)
                .context("连接已删除")
        })
        .transpose()?;
    ensure!(original.as_ref().is_none_or(|c| c.oauth.is_none()), "请通过重新授权更新 OAuth 连接");
    let base: String = if input.protocol == Protocol::Ollama {
        crate::ollama::origin(&input.base)?
    } else {
        base_url(&input.base)?
    }
    .into();
    let label = input.label.trim();
    ensure!(
        !label.is_empty() && label.chars().count() <= 64,
        "连接名称须为 1 到 64 个字符"
    );
    let local = matches!(input.protocol, Protocol::Ollama);
    let key = if local {
        ""
    } else if input.key.trim().is_empty() {
        original
            .as_ref()
            .filter(|old| old.base == base && old.protocol == input.protocol)
            .map(|old| old.key.as_str())
            .unwrap_or("")
    } else {
        input.key.trim()
    };
    ensure!(
        local
            || (!key.is_empty() && key.len() <= 4096 && key.bytes().all(|b| b.is_ascii_graphic())),
        "请输入有效的 API Key"
    );
    let mut connection = Connection {
        id: id.unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
        label: label.to_owned(),
        base,
        protocol: input.protocol,
        models: vec![],
        key: key.to_owned(),
        oauth: None,
    };
    let config = app.config.read().await.clone();
    connection.models = connection.discover(&config).await?;
    Ok((connection, original))
}

async fn save_connection(
    app: &App,
    id: Option<String>,
    input: ConnectionInput,
) -> Result<Connection> {
    let (connection, original) = tested_connection(app, id, input).await?;
    let _guard = app.connection_setup.lock().await;
    let mut connections = saved()?;
    if let Some(original) = original {
        let prior = connections
            .iter_mut()
            .find(|entry| entry.id == original.id)
            .context("连接已删除")?;
        ensure!(
            same_settings(prior, &original),
            "连接已变更，请重新打开编辑页"
        );
        *prior = connection.clone();
    } else {
        ensure!(connections.len() < 128, "最多保存 128 个模型连接");
        connections.push(connection.clone());
    }
    persist(&connections)?;
    Ok(connection)
}

#[derive(Deserialize)]
pub(crate) struct ConnectionName {
    label: String,
}

pub(crate) async fn rename(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(input): Json<ConnectionName>,
) -> Response {
    let label = input.label.trim();
    if label.is_empty() || label.chars().count() > 64 {
        return failure(StatusCode::BAD_REQUEST, "连接名称须为 1 到 64 个字符");
    }
    let _guard = app.connection_setup.lock().await;
    let result = saved().and_then(|mut entries| {
        let entry = entries.iter_mut().find(|entry| entry.id == id).context("连接已删除")?;
        entry.label = label.to_owned();
        let view = entry.view();
        persist(&entries)?;
        Ok(view)
    });
    match result {
        Ok(view) => Json(view).into_response(),
        Err(error) => failure(StatusCode::BAD_REQUEST, error),
    }
}

pub(crate) async fn recheck(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match check_saved(&app, id).await {
        Ok(connection) => Json(connection.view()).into_response(),
        Err(error) => failure(StatusCode::BAD_REQUEST, error),
    }
}

async fn check_saved(app: &App, id: String) -> Result<Connection> {
    let mut connection = saved()?
        .into_iter()
        .find(|entry| entry.id == id)
        .context("连接已删除")?;
    let config = app.config.read().await.clone();
    if connection.oauth.is_some() { connection = codex::fresh(app, &id, false).await?; }
    let models = connection.discover(&config).await?;
    let _guard = app.connection_setup.lock().await;
    let mut entries = saved()?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .context("连接已删除")?;
    ensure!(same_settings(entry, &connection), "连接已变更，请重新检测");
    connection.models = models;
    *entry = connection.clone();
    persist(&entries)?;
    Ok(connection)
}

fn same_settings(a: &Connection, b: &Connection) -> bool {
    a.base == b.base && a.label == b.label && a.protocol == b.protocol && a.key == b.key
}

pub(crate) async fn remove(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let _guard = app.connection_setup.lock().await;
    let result = saved().and_then(|mut entries| {
        entries.retain(|entry| entry.id != id);
        persist(&entries)
    });
    match result {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(error) => failure(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

/// Map unambiguous upstream names to stable per-connection route IDs.
pub(crate) fn local_ids(reserved: &std::collections::HashSet<&str>)
    -> (Vec<String>, std::collections::BTreeMap<String, String>)
{
    let entries = saved().unwrap_or_default();
    let mut ids = Vec::new();
    let mut aliases = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for connection in &entries {
        for (index, name) in connection.models.iter().enumerate() {
            let id = connection.model_id(index);
            if !is_model_id(name) && !reserved.contains(name.as_str()) {
                if seen.insert(name) {
                    aliases.insert(name.clone(), id.clone());
                } else {
                    aliases.remove(name);
                }
            }
            ids.push(id);
        }
    }
    (ids, aliases)
}

fn resolve(id: &str) -> Result<(Connection, String)> {
    let suffix = id.strip_prefix("connection/").context("模型来源无效")?;
    let (connection_id, _) = suffix.split_once('/').context("模型来源无效")?;
    let connection = saved()?
        .into_iter()
        .find(|entry| entry.id == connection_id)
        .context("模型连接已删除")?;
    let model = connection
        .models
        .iter()
        .enumerate()
        .find(|(index, _)| connection.model_id(*index) == id)
        .map(|(_, model)| model.clone())
        .context("此连接已不提供该模型")?;
    Ok((connection, model))
}

pub(crate) fn is_model_id(id: &str) -> bool {
    id.starts_with("connection/")
}

pub(crate) fn can_serve(provider: &str, id: &str) -> bool {
    resolve(id).is_ok_and(|(connection, _)| connection.oauth.is_none() && connection.protocol.offer_provider() == provider)
}

pub(crate) struct SaleRoute {
    connection: Connection,
    pub model: String,
    pub http: Client,
}

impl SaleRoute {
    pub(crate) fn id(&self) -> &str {
        &self.connection.id
    }
    pub(crate) fn label(&self) -> &str {
        &self.connection.label
    }
    pub(crate) fn endpoint(&self) -> Option<String> {
        Url::parse(&self.connection.base)
            .ok()
            .map(|url| url.authority().to_owned())
    }
}

impl TokenProvider for SaleRoute {
    fn acquire(&self, provider: &str, model: &str) -> Option<LeasedToken> {
        (provider == self.connection.protocol.offer_provider() && is_model_id(model)).then(|| {
            LeasedToken {
                token: self.connection.key.clone(),
                account_id: self.connection.id.clone(),
                upstream_base: Some(if matches!(self.connection.protocol, Protocol::Ollama) {
                    format!("{}/v1", self.connection.base.trim_end_matches('/'))
                } else {
                    self.connection.base.clone()
                }),
                upstream_model: Some(self.model.clone()),
                upstream_wire: Some(match self.connection.protocol {
                    Protocol::Messages => asale_protocol::ids::Wire::Claude,
                    Protocol::Responses => asale_protocol::ids::Wire::Responses,
                    _ => asale_protocol::ids::Wire::Openai,
                }),
                ..Default::default()
            }
        })
    }
}

pub(crate) async fn sale_route(app: &App, payload: &mut HttpRequestPayload) -> Result<SaleRoute> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let (connection, model) = resolve(&payload.model)?;
    ensure!(connection.oauth.is_none(), "此账号用于本地调用");
    let protocol = connection.protocol;
    ensure!(
        payload.upstream.provider == protocol.offer_provider(),
        "任务与报价来源不符"
    );
    let path = Url::parse(&payload.upstream.url)?.path().to_owned();
    ensure!(
        payload.upstream.method == "POST" && path == format!("/v1/{}", protocol.resource()),
        "任务协议与报价来源不符"
    );
    let mut body: Value = serde_json::from_slice(&STANDARD.decode(&payload.upstream.body_b64)?)?;
    ensure!(body.is_object(), "任务消息格式无效");
    let limit_key = if protocol == Protocol::Responses {
        "max_output_tokens"
    } else if body.get("max_completion_tokens").is_some() {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    let limit = body[limit_key].as_i64().unwrap_or(payload.budget_tokens);
    ensure!(limit > 0, "任务输出上限无效");
    body[limit_key] = json!(limit.min(payload.budget_tokens));
    payload.upstream.body_b64 = STANDARD.encode(serde_json::to_vec(&body)?);
    payload.upstream.url = format!("https://placeholder.invalid{path}");
    payload.upstream.headers.clear();
    payload
        .upstream
        .headers
        .insert("content-type".into(), json!("application/json"));
    let config = app.config.read().await.clone();
    let http = if matches!(protocol, Protocol::Ollama) {
        crate::ollama::client().clone()
    } else {
        client(&config, &connection.base)?
    };
    Ok(SaleRoute {
        connection,
        model,
        http,
    })
}

#[derive(Deserialize)]
pub(crate) struct Trial {
    model: String,
    prompt: String,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    trace: Option<String>,
}

pub(crate) async fn trial(State(app): State<Arc<App>>, Json(input): Json<Trial>) -> Response {
    let prompt = input.prompt.trim();
    if prompt.is_empty() || prompt.len() > 8192 {
        return failure(StatusCode::BAD_REQUEST, "请输入 1 到 8192 字节的测试消息");
    }
    let trace = input.trace.as_deref().and_then(|trace| Uuid::parse_str(trace).ok());
    if is_model_id(&input.model) {
        run_trial_with_trace(app, &input.model, prompt, trace, input.stream).await
    } else {
        crate::control::trial_saved_model(app, &input.model, prompt, trace, input.stream).await
    }
}

pub(crate) async fn run_trial(app: Arc<App>, id: &str, prompt: &str) -> Response {
    run_trial_with_trace(app, id, prompt, None, false).await
}

pub(crate) async fn run_trial_with_trace(app: Arc<App>, id: &str, prompt: &str, trace: Option<Uuid>, stream: bool) -> Response {
    match resolve(id) {
        Ok((connection, model)) => {
            let mut body = connection.protocol.test_body(prompt.to_owned());
            body["stream"] = json!(stream);
            if stream && matches!(connection.protocol, Protocol::ChatCompletions | Protocol::Ollama) {
                body["stream_options"] = json!({"include_usage":true});
            }
            let limit = body["max_output_tokens"].as_i64().or(body["max_tokens"].as_i64());
            let resource = connection.protocol.resource();
            let mut response = forward(app, connection, model, resource, body, trace).await;
            if let Some(limit) = limit {
                response.headers_mut().insert("x-modelcat-output-limit", limit.into());
            }
            response
        }
        Err(error) => failure(StatusCode::NOT_FOUND, error),
    }
}

pub(crate) async fn proxy(app: Arc<App>, id: &str, path: &str, body: Value) -> Response {
    match resolve(id) {
        Ok((connection, model)) => {
            let resource = match path.strip_prefix("/v1/") {
                Some("messages") if connection.protocol == Protocol::Ollama => "messages",
                Some(path) if path == connection.protocol.resource() => connection.protocol.resource(),
                _ => return failure(StatusCode::BAD_REQUEST, "请求接口与模型连接协议不匹配"),
            };
            forward(app, connection, model, resource, body, None).await
        }
        Err(error) => failure(StatusCode::NOT_FOUND, error),
    }
}

fn failure(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error":{"message":error.to_string()}}))).into_response()
}

#[derive(Clone, Serialize)]
pub(crate) struct ActiveRequest {
    at: i64,
    id: String,
    model: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    endpoint: String,
    phase: &'static str,
    pub(crate) gateway: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace: Option<String>,
}

pub(crate) struct InFlight {
    app: Arc<App>,
    id: String,
}

impl InFlight {
    pub(crate) fn start(app: &Arc<App>, record: &LocalRecord, trace: Option<Uuid>) -> Self {
        app.connection_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                record.id.clone(),
                ActiveRequest {
                    at: crate::state::now(),
                    id: record.id.clone(),
                    model: record.model.clone(),
                    source: record.source_label.clone().unwrap_or_default(),
                    source_id: record.source_id.clone(),
                    endpoint: record.endpoint.clone().unwrap_or_default(),
                    phase: "sending",
                    gateway: record.gateway,
                    trace: trace.map(|trace| trace.to_string()),
                },
            );
        Self { app: Arc::clone(app), id: record.id.clone() }
    }

    pub(crate) fn receiving(&self) {
        let mut requests = self.app.connection_requests.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = requests.get_mut(&self.id) {
            active.phase = "receiving";
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.app
            .connection_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

async fn forward(
    app: Arc<App>,
    connection: Connection,
    model: String,
    resource: &'static str,
    mut body: Value,
    trace: Option<Uuid>,
) -> Response {
    if app.stopping.load(std::sync::atomic::Ordering::Relaxed) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "工作区正在停止");
    }
    let config = app.config.read().await.clone();
    let route = || -> Result<(Client, Url)> {
        let base = Url::parse(&connection.base)?;
        if matches!(connection.protocol, Protocol::Ollama) {
            return Ok((
                crate::ollama::client().clone(),
                base.join("v1/")?.join(resource)?,
            ));
        }
        Ok((client(&config, &connection.base)?, base.join(resource)?))
    };
    let (client, endpoint) = match route() {
        Ok(value) => value,
        Err(error) => return failure(StatusCode::BAD_GATEWAY, error),
    };
    let oauth = connection.oauth.is_some();
    let buffer = oauth && body["stream"] != true;
    let connection = if oauth {
        crate::control::prepare_codex_body(&mut body);
        body["stream"] = json!(true);
        body["store"] = json!(false);
        if !body["instructions"].is_string() {body["instructions"] = json!("You are a helpful assistant.");}
        match codex::fresh(&app, &connection.id, false).await {
            Ok(current) => current,
            Err(error) => return failure(StatusCode::BAD_GATEWAY,error),
        }
    } else {connection};
    let request = if oauth { codex::authorize(&connection,client.post(endpoint.clone())) }
        else { connection.protocol.authenticate(client.post(endpoint.clone()), &connection.key) };
    body["model"] = json!(model);
    let bytes = body.to_string().into_bytes();
    let record = LocalRecord {
        model,
        source_id: Some(connection.id),
        source_label: Some(connection.label),
        endpoint: Some(endpoint.authority().to_owned()),
        upstream_bytes_out: bytes.len() as u64,
        ..Default::default()
    };
    let mut request = request;
    if connection.protocol == Protocol::Ollama && resource == "messages" {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let request = request
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(120))
        .body(bytes);
    trace_response(app, record, trace, async {
        match request.send().await {
            Ok(upstream) => {
                if upstream.status().is_redirection() {
                    return Ok(failure(StatusCode::BAD_GATEWAY, "模型服务返回了重定向"));
                }
                if oauth && !upstream.status().is_success() {
                    return Ok(failure(upstream.status(), "Codex 调用失败，请检查模型、额度和授权状态"));
                }
                if buffer {
                    let mut bytes = Vec::new();
                    let mut stream = upstream.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(|_| failure(StatusCode::BAD_GATEWAY, "Codex 回复中断"))?;
                        if bytes.len()+chunk.len()>32<<20 {return Err(failure(StatusCode::BAD_GATEWAY,"回复过大"));}
                        bytes.extend_from_slice(&chunk);
                    }
                    return crate::control::buffered_response(&bytes).map(|value|Json(value).into_response())
                        .ok_or_else(||failure(StatusCode::BAD_GATEWAY,"Codex 回复未完成"));
                }
                // Codex streams Responses events even when it omits the media type.
                let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned()
                    .or_else(|| oauth.then(|| HeaderValue::from_static("text/event-stream")));
                let mut response = (upstream.status(), Body::from_stream(upstream.bytes_stream())).into_response();
                if let Some(content_type) = content_type {
                    response.headers_mut().insert(header::CONTENT_TYPE, content_type);
                }
                Ok(response)
            }
            Err(error) if error.is_timeout() => Err(failure(StatusCode::GATEWAY_TIMEOUT, "等待模型服务响应超时")),
            Err(_) => Err(failure(StatusCode::BAD_GATEWAY, "无法连接模型服务")),
        }
    }).await
}

pub(crate) async fn trace_response(
    app: Arc<App>,
    mut record: LocalRecord,
    trace: Option<Uuid>,
    forward: impl std::future::Future<Output = std::result::Result<Response, Response>>,
) -> Response {
    let id = format!("local_{}", Uuid::new_v4().simple());
    let started = Instant::now();
    record.id.clone_from(&id);
    record.at = crate::state::now();
    let flight = InFlight::start(&app, &record, trace);
    let (response, failure_at) = match forward.await {
        Ok(response) => {
            flight.receiving();
            let failed = (!response.status().is_success()).then_some(FailureAt::Model);
            (response, failed)
        }
        Err(response) => (response, Some(FailureAt::ModelRoute)),
    };
    let (parts, body) = response.into_parts();
    record.http_status = Some(parts.status.as_u16());
    record.status = if parts.status.is_success() { "ok" } else { "upstream_error" }.into();
    record.failure_at = failure_at;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        let mut snippet = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    record.upstream_bytes_in += bytes.len() as u64;
                    let remaining = MAX_CATALOG_BYTES.saturating_sub(snippet.len());
                    snippet.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                    if tx.send(Ok(bytes)).await.is_err() {
                        record.status = "cancelled".into();
                        record.failure_at = None;
                        break;
                    }
                }
                Err(_) => {
                    record.status = "stream_error".into();
                    record.failure_at = Some(FailureAt::ModelRoute);
                    let _ = tx.send(Err(std::io::Error::other("上游响应中断"))).await;
                    break;
                }
            }
        }
        if let Ok(result) = serde_json::from_slice::<Value>(&snippet) {
            read_usage(&result, &mut record);
        } else {
            for line in snippet.split(|byte| *byte == b'\n') {
                if let Some(data) = line.strip_prefix(b"data:") {
                    if let Ok(event) = serde_json::from_slice::<Value>(data) {
                        read_usage(&event, &mut record);
                    }
                }
            }
        }
        record.duration_ms = Some(started.elapsed().as_millis() as u64);
        app.add_record(record).await;
        drop(flight);
    });
    let response_body = Body::from_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)));
    let mut response = Response::from_parts(parts, response_body);
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "x-modelcat-request-id",
        HeaderValue::from_str(&id).expect("valid request ID"),
    );
    response
}

fn read_usage(value: &Value, record: &mut LocalRecord) {
    let usage = value.get("usage")
        .or_else(|| value.pointer("/message/usage"))
        .or_else(|| value.pointer("/response/usage"));
    let Some(usage) = usage else { return };
    if let Some(tokens) = usage["prompt_tokens"].as_i64().or(usage["input_tokens"].as_i64()) {
        record.input_tokens = tokens;
    }
    if let Some(tokens) = usage["completion_tokens"].as_i64().or(usage["output_tokens"].as_i64()) {
        record.output_tokens = tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Router,
    };

    async fn fixture(reply: &'static str, status: StatusCode) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/api/tags", get(|| async { Json(json!({"models":[{"name":"same-model"}]})) }))
            .route("/v1/chat/completions", post(move |Json(body): Json<Value>| async move {
                assert_eq!(body["model"], "same-model");
                (status, [(header::CONNECTION, "close")],
                    Json(json!({"choices":[{"message":{"content":reply}}],"usage":{"prompt_tokens":3,"completion_tokens":1}})))
            }));
        let worker = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (address, worker)
    }

    #[test]
    fn gateway_bases_preserve_prefixes_and_scope_http_to_local_services() {
        for base in ["http://192.168.1.20:38080", "http://localhost:38080/v1", "http://[::1]:38080"] {
            assert_eq!(base_url(base).unwrap().join("responses").unwrap().path(), "/v1/responses");
        }
        assert_eq!(base_url("https://example.com/team/api").unwrap().join("messages").unwrap().path(), "/team/api/messages");
        for invalid in ["http://example.com", "http://8.8.8.8", "http://localhost:38080/?token=secret"] {
            assert!(base_url(invalid).is_err());
        }
    }

    #[test]
    fn unavailable_lan_can_be_probed_before_saving_once() {
        crate::testenv::with_temp_data_dir("lan-permission-wait", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                drop(listener);
                let app = App::open_for_mode(true).await.unwrap();
                let input = |key: &str| ConnectionInput {
                    label: "test".into(), base: format!("http://{address}"),
                    protocol: Protocol::Responses, key: key.into(),
                };
                let save = |key: &str, probe_only| test_and_save(&app, None, input(key), ConnectionTest { probe_only });
                let response = save("fixture", false).await;
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"], "local_service_unreachable");
                let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                let router = Router::new().route("/v1/models", get(|headers: axum::http::HeaderMap| async move {
                    if headers[header::AUTHORIZATION] == "Bearer fixture" {
                        Json(json!({"data":[{"id":"fixture"}]})).into_response()
                    } else {
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                }));
                let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
                assert_eq!(save("wrong", true).await.status(), StatusCode::BAD_REQUEST);
                assert_eq!(save("fixture", true).await.status(), StatusCode::OK);
                assert!(saved().unwrap().is_empty());
                assert_eq!(save("fixture", false).await.status(), StatusCode::OK);
                assert_eq!(saved().unwrap().len(), 1);
                server.abort();
            })
        })
    }

    #[test]
    fn local_http_gateway_discovers_models_and_routes_trial_requests() {
        crate::testenv::with_temp_data_dir("local-http-trial", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let router = Router::new()
                    .route("/v1/models", get(|| async { Json(json!({"data":[{"id":"local-test"}]})) }))
                    .route("/v1/responses", post(|Json(body): Json<Value>| async move {
                        assert_eq!(body["max_output_tokens"], TRIAL_OUTPUT_TOKENS);
                        Json(json!({"output_text":"long answer".repeat(500)}))
                    }));
                let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
                let app = App::open_for_mode(true).await.unwrap();
                let connection = save_connection(&app, None, ConnectionInput { label:"local".into(),
                    base, protocol:Protocol::Responses, key:"fixture".into() }).await.unwrap();
                let reply = run_trial_with_trace(app, &connection.model_id(0), "long answer", None, false).await;
                assert_eq!(reply.status(), StatusCode::OK);
                let body = axum::body::to_bytes(reply.into_body(), 65536).await.unwrap();
                let data: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(data["output_text"], "long answer".repeat(500));
                server.abort();
            })
        })
    }

    #[test]
    fn same_model_names_are_routed_by_connection_and_history_survives_edits() {
        crate::testenv::with_temp_data_dir("model-connections", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let (first, first_worker) = fixture("first endpoint", StatusCode::OK).await;
                let (second, second_worker) = fixture("second endpoint", StatusCode::OK).await;
                let input = |label: &str, base: &str| ConnectionInput {
                    label: label.into(),
                    base: base.into(),
                    protocol: Protocol::Ollama,
                    key: String::new(),
                };
                let app = App::open_for_mode(true).await.unwrap();
                let a = save_connection(&app, None, input("First", &first))
                    .await
                    .unwrap();
                let (_, aliases) = app.local_catalog().await;
                assert_eq!(aliases.get("same-model"), Some(&a.model_id(0)));
                let (_, reserved) = local_ids(&["same-model"].into_iter().collect());
                assert!(!reserved.contains_key("same-model"));
                let b = save_connection(&app, None, input("Second", &second))
                    .await
                    .unwrap();
                assert_ne!(a.model_id(0), b.model_id(0));
                let (routes, aliases) = app.local_catalog().await;
                assert!(!aliases.contains_key("same-model"));
                assert!(routes.contains(&a.model_id(0)) && routes.contains(&b.model_id(0)));
                let make_request = |connection: &Connection| {
                    let path = "/v1/chat/completions";
                    let id = connection.model_id(0);
                    let body = json!({"messages":[{"role":"user","content":"ping"}]});
                    let app = app.clone();
                    async move {
                        let response = proxy(app, &id, path, body).await;
                        assert_eq!(response.status(), StatusCode::OK);
                        let request_id = response.headers()["x-modelcat-request-id"]
                            .to_str()
                            .unwrap()
                            .to_owned();
                        let bytes = axum::body::to_bytes(response.into_body(), 4096)
                            .await
                            .unwrap();
                        let payload: Value = serde_json::from_slice(&bytes).unwrap();
                        (request_id, payload)
                    }
                };
                let ((first_id, first_body), (second_id, second_body)) =
                    tokio::join!(make_request(&a), make_request(&b));
                assert_eq!(
                    first_body["choices"][0]["message"]["content"],
                    "first endpoint"
                );
                assert_eq!(
                    second_body["choices"][0]["message"]["content"],
                    "second endpoint"
                );
                let renamed = save_connection(&app, Some(a.id.clone()), input("Renamed", &first))
                    .await
                    .unwrap();
                assert_eq!(a.model_id(0), renamed.model_id(0));
                let records = app.records.lock().unwrap();
                let first_record = records.iter().find(|row| row.id == first_id).unwrap();
                let second_record = records.iter().find(|row| row.id == second_id).unwrap();
                assert_eq!(first_record.source_label.as_deref(), Some("First"));
                assert_eq!(second_record.source_label.as_deref(), Some("Second"));
                assert_eq!(first_record.input_tokens, 3);
                assert_eq!(second_record.output_tokens, 1);
                first_worker.abort();
                second_worker.abort();
            });
        });
    }

    #[test]
    fn trace_distinguishes_model_http_failure_from_unreachable_endpoint() {
        crate::testenv::with_temp_data_dir("model-trace-failure", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let (address, worker) = fixture("rejected", StatusCode::UNAUTHORIZED).await;
                let app = App::open_for_mode(true).await.unwrap();
                let connection = save_connection(&app, None, ConnectionInput {
                    label: "Fixture source".into(),
                    base: address,
                    protocol: Protocol::Ollama,
                    key: String::new(),
                }).await.unwrap();
                let model = connection.model_id(0);
                let request = json!({"messages":[{"role":"user","content":"ping"}]});
                let call = |response: Response, expected_status, expected_failure| {
                    let app = Arc::clone(&app);
                    async move {
                        assert_eq!(response.status(), expected_status);
                        let id = response.headers()["x-modelcat-request-id"].to_str().unwrap().to_owned();
                        axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
                        let records = app.records.lock().unwrap();
                        let record = records.iter().find(|record| record.id == id).unwrap();
                        assert_eq!(record.failure_at, Some(expected_failure));
                    }
                };
                let path = "/v1/chat/completions";
                call(proxy(app.clone(), &model, path, request.clone()).await,
                    StatusCode::UNAUTHORIZED, FailureAt::Model).await;
                worker.abort();
                assert!(worker.await.unwrap_err().is_cancelled());
                call(proxy(app.clone(), &model, path, request).await,
                    StatusCode::BAD_GATEWAY, FailureAt::ModelRoute).await;
            });
        });
    }
}
