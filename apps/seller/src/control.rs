use crate::{
    config::{Binding, Config},
    gateway, keychain, runner,
    state::App,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

pub fn router(app: Arc<App>) -> Router {
    let protected = Router::new()
        .route("/api/status", get(status))
        .route("/api/config", get(config).post(save_config))
        .route("/api/bind", post(bind))
        .route("/api/import", post(import))
        .route("/api/models/discover", post(discover))
        .route("/api/import/kiro", post(import_kiro))
        .route("/api/accounts/state", post(account_state))
        .route("/api/oauth/start", post(oauth_start))
        .route("/api/oauth/:id", get(oauth_result))
        .route("/api/oauth/:id/code", post(oauth_code))
        .route("/api/quotes/submit", post(submit_quotes))
        .route("/api/earnings", get(earnings))
        .route("/api/proxy/test", post(proxy_test))
        .route("/api/probe", post(probe))
        .route("/api/pause", post(pause))
        .route("/api/shutdown", post(shutdown))
        .route("/v1/models", get(models))
        .route("/v1/responses", post(local_proxy))
        .route("/v1/messages", post(local_proxy))
        .layer(middleware::from_fn_with_state(app.clone(), auth));
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/healthz",
            get(|| async { Json(json!({"ok":true,"name":"channel-node"})) }),
        )
        .merge(protected)
        .layer(axum::extract::DefaultBodyLimit::max(8 << 20))
        .with_state(app)
}
async fn auth(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // No browser page on a different origin may drive the node, even if it guesses a local port.
    if let Some(origin) = headers.get("origin") {
        let host = headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let ok = origin
            .to_str()
            .ok()
            .and_then(|o| reqwest::Url::parse(o).ok())
            .is_some_and(|u| {
                u.host_str().is_some()
                    && format!(
                        "{}:{}",
                        u.host_str().unwrap(),
                        u.port_or_known_default().unwrap_or(80)
                    ) == host
            });
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let presented = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    use sha2::{Digest, Sha256};
    let a = Sha256::digest(presented.as_bytes());
    let expected = if request.uri().path().starts_with("/v1/") {
        &app.api_token
    } else {
        &app.token
    };
    let b = Sha256::digest(expected.as_bytes());
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    if diff != 0 {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    next.run(request).await
}
fn error(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":e.to_string()})),
    )
        .into_response()
}
async fn status(State(app): State<Arc<App>>) -> Response {
    let accounts = app.accounts.read().await.clone();
    let cfg = app.config.read().await.clone();
    let binding = app.binding.read().await.clone();
    let pool = app
        .pool
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .statuses(crate::state::now());
    let records = app
        .records
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Json(json!({"version":env!("CARGO_PKG_VERSION"),"runtime":app.runtime.read().await.clone(),"config":cfg,"accounts":accounts,"account_status":pool,"records":records,"binding":binding.map(|b|json!({"gateway_url":b.gateway_url,"node_id":b.node_id,"currency":b.currency})),"available_models":app.available_models().await})).into_response()
}
async fn config(State(app): State<Arc<App>>) -> Response {
    Json(json!(app.config.read().await.clone())).into_response()
}
async fn save_config(State(app): State<Arc<App>>, Json(cfg): Json<Config>) -> Response {
    match app.save_config(cfg).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}
async fn bind(State(app): State<Arc<App>>, Json(binding): Json<Binding>) -> Response {
    if let Err(e) = binding.validate() {
        return error(e);
    }
    if app.runtime.read().await.active > 0 {
        return error("pause and drain active requests before rebinding");
    }
    match serde_json::to_string(&binding)
        .map_err(anyhow::Error::from)
        .and_then(|s| keychain::set("node.binding", &s))
    {
        Ok(()) => {
            *app.binding.write().await = Some(binding);
            Json(json!({"ok":true})).into_response()
        }
        Err(e) => error(e),
    }
}
async fn import(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    match app.import_cli(v["provider"].as_str().unwrap_or("")).await {
        Ok(n) => {
            let models = app.discover_models().await.ok();
            Json(json!({"imported":n,"discovery":models})).into_response()
        }
        Err(e) => error(e),
    }
}
async fn discover(State(app): State<Arc<App>>) -> Response {
    match app.discover_models().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
async fn import_kiro(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    match crate::kiro::import(&app, v).await {
        Ok(()) => {
            let _ = app.discover_models().await;
            Json(json!({"ok":true})).into_response()
        }
        Err(e) => error(e),
    }
}
async fn account_state(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    match app
        .set_account_enabled(
            v["provider"].as_str().unwrap_or(""),
            v["id"].as_str().unwrap_or(""),
            v["enabled"].as_bool().unwrap_or(false),
        )
        .await
    {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}
async fn oauth_start(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    match app.begin_oauth(v["provider"].as_str().unwrap_or("")).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
async fn oauth_result(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let mut flows = app.flows.write().await;
    let value = flows
        .get(&id)
        .cloned()
        .unwrap_or(json!({"status":"missing"}));
    if value["status"] != "pending" {
        flows.remove(&id);
    }
    Json(value).into_response()
}
async fn oauth_code(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Response {
    let Some(code) = crate::oauth::extract_pasted_code(v["code"].as_str().unwrap_or("")) else {
        return error("no authorization code found");
    };
    if let Some(submitter) = app.submitters.read().await.get(&id) {
        if submitter.submit(code) {
            return Json(json!({"ok":true})).into_response();
        }
    }
    error("unknown or expired OAuth flow")
}
async fn submit_quotes(State(app): State<Arc<App>>, Json(selection): Json<Value>) -> Response {
    let offers = app.config.read().await.offers.clone();
    let mut results = vec![];
    for o in offers.iter().filter(|o| {
        o.enabled
            && selection["models"]
                .as_array()
                .is_none_or(|models| models.iter().any(|m| m.as_str() == Some(o.model.as_str())))
    }) {
        match gateway::submit_quote(&app, o).await {
            Ok(v) => results.push(v),
            Err(e) => return error(e),
        }
    }
    Json(json!({"quotes":results})).into_response()
}
async fn earnings(State(app): State<Arc<App>>) -> Response {
    match gateway::account(&app).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
async fn proxy_test() -> Response {
    let started = std::time::Instant::now();
    // A fixed host avoids turning the settings panel into a general URL fetch endpoint.
    match asale_client_core::http::upstream().get("https://api.anthropic.com").timeout(std::time::Duration::from_secs(10)).send().await{
        Ok(r)=>Json(json!({"reachable":true,"http_status":r.status().as_u16(),"elapsed_ms":started.elapsed().as_millis()})).into_response(),Err(_)=>error("upstream connectivity test failed; check proxy and network")
    }
}
async fn pause(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    let mut cfg = app.config.read().await.clone();
    cfg.paused = v["paused"].as_bool().unwrap_or(true);
    match app.save_config(cfg).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}
async fn shutdown(State(app): State<Arc<App>>) -> Response {
    app.shutdown_requested.notify_one();
    Json(json!({"draining":true})).into_response()
}
async fn models(State(app): State<Arc<App>>) -> Response {
    let models: Vec<_> = app
        .available_models()
        .await
        .into_iter()
        .map(|id| json!({"id":id,"object":"model","owned_by":"local"}))
        .collect();
    Json(json!({"object":"list","data":models})).into_response()
}

async fn local_proxy(
    State(app): State<Arc<App>>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> Response {
    local_request(app, uri, body, false).await
}

async fn probe(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    let model = v["model"].as_str().unwrap_or("");
    let cfg = app.config.read().await.clone();
    let Some(offer) = cfg.offers.iter().find(|o| o.model == model && o.enabled) else {
        return error("unknown enabled model");
    };
    let uri = if offer.provider == "claude" {
        "/v1/messages"
    } else {
        "/v1/responses"
    };
    let body = if uri == "/v1/messages" {
        json!({"model":model,"max_tokens":32,"messages":[{"role":"user","content":"Reply with OK."}],"stream":false})
    } else {
        json!({"model":model,"max_output_tokens":32,"input":"Reply with OK.","stream":false})
    };
    local_request(app, uri.parse().unwrap(), body, true).await
}

async fn local_request(
    app: Arc<App>,
    uri: axum::http::Uri,
    mut body: Value,
    probe: bool,
) -> Response {
    let model = body["model"].as_str().unwrap_or("").to_string();
    let cfg = app.config.read().await.clone();
    let Some(offer) = cfg
        .offers
        .iter()
        .find(|o| o.enabled && (o.model == model || o.upstream_model == model))
        .cloned()
    else {
        return error("model is not configured");
    };
    if cfg.paused && !probe {
        return error("node is paused");
    }
    if (uri.path() == "/v1/messages") != (offer.provider == "claude") {
        if offer.provider != "custom" && offer.provider != "kiro" {
            return error("request protocol does not match model provider");
        }
    }
    let stream = body["stream"].as_bool().unwrap_or(false);
    let budget = body["max_output_tokens"]
        .as_i64()
        .or(body["max_tokens"].as_i64())
        .unwrap_or(4096);
    if !(1..=32768).contains(&budget) {
        return error("invalid output budget");
    }
    body["model"] = json!(offer.upstream_model);
    let path = if uri.path() == "/v1/messages" {
        "messages"
    } else {
        "responses"
    };
    let url = if offer.provider == "claude" {
        "https://api.anthropic.com/v1/messages"
    } else if offer.provider == "custom" {
        if path == "messages" {
            "https://placeholder.invalid/v1/messages"
        } else {
            "https://placeholder.invalid/v1/responses"
        }
    } else {
        "https://chatgpt.com/backend-api/codex/responses"
    };
    if path == "responses" {
        body["stream"] = json!(true);
        body["store"] = json!(false);
        body["max_output_tokens"] = json!(budget);
        if body["instructions"].as_str().unwrap_or("").is_empty() {
            body["instructions"] = json!("You are a helpful assistant.")
        }
    }
    if offer.provider == "codex" {
        prepare_codex_body(&mut body);
    }
    let id = format!("local_{}", uuid::Uuid::new_v4().simple());
    let exp = crate::state::now() + 120;
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    let sig = B64.encode(
        key.sign(format!("{id}|{}|{budget}|{exp}", offer.upstream_model).as_bytes())
            .to_bytes(),
    );
    let payload = asale_client_core::protocol::HttpRequestPayload {
        id: id.clone(),
        task_id: id.clone(),
        quota_sig: sig,
        model: offer.upstream_model,
        exp,
        budget_tokens: budget,
        stream,
        upstream: asale_client_core::protocol::UpstreamPayload {
            provider: offer.provider,
            method: "POST".into(),
            url: url.into(),
            headers: serde_json::from_value(
                json!({"content-type":"application/json","anthropic-version":"2023-06-01"}),
            )
            .unwrap(),
            body_b64: B64.encode(serde_json::to_vec(&body).unwrap()),
        },
    };
    let verifier = asale_client_core::security::QuotaVerifier::from_pubkey_b64(
        &B64.encode(key.verifying_key().to_bytes()),
    )
    .unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let (cancel, cancel_rx) = oneshot::channel();
    tokio::spawn(runner::execute(app, payload, verifier, tx, cancel_rx));
    if stream {
        let (output, output_rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
        tokio::spawn(async move {
            let _cancel = cancel;
            while let Some(event) = rx.recv().await {
                match event.msg_type.as_str() {
                    "stream_chunk" => {
                        if let Some(data) = event.payload["data_b64"]
                            .as_str()
                            .and_then(|s| B64.decode(s).ok())
                        {
                            if output.send(Ok(data)).await.is_err() {
                                break;
                            }
                        }
                    }
                    "http_response" => {
                        if let Some(data) = event.payload["body_b64"]
                            .as_str()
                            .and_then(|s| B64.decode(s).ok())
                        {
                            let _ = output.send(Ok(data)).await;
                        }
                        break;
                    }
                    "stream_end" | "completion" => break,
                    "error" => {
                        let _ = output
                            .send(Ok(
                                format!("event: error\ndata: {}\n\n", event.payload).into_bytes()
                            ))
                            .await;
                        break;
                    }
                    _ => {}
                }
            }
        });
        (
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
            ],
            Body::from_stream(ReceiverStream::new(output_rx)),
        )
            .into_response()
    } else {
        let _cancel = cancel;
        let mut bytes = Vec::new();
        while let Some(event) = rx.recv().await {
            match event.msg_type.as_str() {
                "http_response" => {
                    let raw = event.payload["body_b64"]
                        .as_str()
                        .and_then(|s| B64.decode(s).ok())
                        .unwrap_or_default();
                    return ([("content-type", "application/json")], raw).into_response();
                }
                "stream_chunk" => {
                    if let Some(chunk) = event.payload["data_b64"]
                        .as_str()
                        .and_then(|s| B64.decode(s).ok())
                    {
                        bytes.extend(chunk);
                        if bytes.len() > 32 << 20 {
                            return error("response too large");
                        }
                    }
                }
                "stream_end" => {
                    if let Some(response) = buffered_response(&bytes) {
                        return Json(response).into_response();
                    }
                    return error("no completed response");
                }
                "error" => return upstream_error(&event.payload),
                _ => {}
            }
        }
        error("upstream disconnected")
    }
}

fn buffered_response(bytes: &[u8]) -> Option<Value> {
    let mut items = std::collections::BTreeMap::new();
    for line in bytes.split(|b| *b == b'\n') {
        let Some(raw) = line.strip_prefix(b"data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_slice::<Value>(raw) else {
            continue;
        };
        match event["type"].as_str() {
            Some("response.output_item.done") => {
                if let Some(index) = event["output_index"].as_u64().filter(|i| *i < 4096) {
                    if event["item"].is_object() {
                        items.insert(index, event["item"].clone());
                    }
                }
            }
            Some("response.completed" | "response.incomplete") => {
                let mut response = event["response"].clone();
                if !response.is_object() {
                    return None;
                }
                if response["output"].as_array().is_none_or(Vec::is_empty) && !items.is_empty() {
                    response["output"] = Value::Array(items.into_values().collect());
                }
                return Some(response);
            }
            _ => {}
        }
    }
    None
}

// Subscription Responses differs from the public API. Keep the signed budget
// in HttpRequestPayload; the Codex backend rejects it as a JSON parameter.
fn prepare_codex_body(body: &mut Value) {
    if let Some(fields) = body.as_object_mut() {
        for key in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
            fields.remove(key);
        }
        if let Some(text) = fields.get("input").and_then(Value::as_str) {
            let text = text.to_owned();
            fields.insert(
                "input".into(),
                json!([{"role":"user","content":[{"type":"input_text","text":text}]}]),
            );
        }
    }
}

fn upstream_error(payload: &Value) -> Response {
    // Never expose arbitrary provider messages, which can echo credentials or
    // prompts. Surface only fixed categories and the validated HTTP status.
    let code = payload["code"].as_str().unwrap_or("");
    let category = match code {
        "UPSTREAM_4XX" => "请求参数被拒绝",
        "UPSTREAM_5XX" => "上游服务或网络异常",
        "UPSTREAM_RATE_LIMIT" => "账号限流或额度不足",
        "TOKEN_EXPIRED" => "登录凭证失效，请重新授权",
        "LANE_UNUSABLE" => "账号或模型不可用",
        "JOB_REJECTED" => "节点繁忙或正在停止",
        _ => "模型请求失败",
    };
    let status = payload["message"]
        .as_str()
        .and_then(|s| s.strip_prefix("upstream "))
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (400..=599).contains(s));
    match status {
        Some(status) => error(format!("{category}（HTTP {status}）")),
        None => error(category),
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn completed_response_keeps_items_when_terminal_event_omits_output() {
        let events = [
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"message","content":[{"type":"output_text","text":"OK"}]}}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"read","arguments":"{}"}}),
            json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":6}}}),
        ];
        let wire = events
            .iter()
            .map(|v| format!("data:{v}\n\n"))
            .collect::<String>();
        let response = buffered_response(wire.as_bytes()).unwrap();
        assert_eq!(response["output"][0]["type"], "function_call");
        assert_eq!(response["output"][1]["content"][0]["text"], "OK");
        assert_eq!(response["usage"]["output_tokens"], 6);
    }

    #[test]
    fn codex_probe_uses_message_input_without_api_token_limits() {
        let mut body = json!({"input":"Reply with OK.","max_output_tokens":32,"max_tokens":32,"max_completion_tokens":32,"stream":true,"store":false,"instructions":"rules"});
        prepare_codex_body(&mut body);
        assert_eq!(body["input"][0]["content"][0]["text"], "Reply with OK.");
        assert_eq!(body["input"][0]["role"], "user");
        for key in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
            assert!(body.get(key).is_none());
        }
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "rules");
        let mut tools = json!({"input":[{"type":"function_call_output","call_id":"c1","output":"result"}],"tools":[{"type":"function","name":"read"}]});
        let before = tools.clone();
        prepare_codex_body(&mut tools);
        assert_eq!(tools, before);
    }

    #[tokio::test]
    async fn local_errors_show_http_status_without_raw_provider_text() {
        let response = upstream_error(
            &json!({"code":"UPSTREAM_4XX","message":"upstream 400","detail":"secret prompt or token"}),
        );
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("HTTP 400"));
        assert!(!text.contains("secret"));
    }
}
