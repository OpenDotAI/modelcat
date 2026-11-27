use crate::{
    claude_api, connections,
    config::{Binding, Config},
    gateway, keychain, runner,
    state::App,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
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
        .route("/api/activity", get(activity))
        .route("/api/request-history", get(request_history))
        .route("/api/request-history/:id", get(request_detail))
        .route("/api/traffic/:range", get(traffic))
        .route("/api/token-usage/:range", get(token_usage))
        .route("/api/connections", get(connections::list).post(connections::connect))
        .route("/api/connections/:id", post(connections::update))
        .route("/api/connections/codex/start", post(connections::codex::start))
        .route("/api/connections/codex/:id/browser", get(connections::codex::browser_launch))
        .route("/api/connections/codex/:id/callback", post(connections::codex::callback))
        .route("/api/connections/codex/:id/poll", post(connections::codex::poll))
        .route("/api/connections/codex/:id/cancel", post(connections::codex::cancel))
        .route("/api/connections/:id/refresh", post(connections::codex::refresh))
        .route("/api/connections/:id/rename", post(connections::rename))
        .route("/api/connections/:id/check", post(connections::recheck))
        .route("/api/connections/:id/remove", post(connections::remove))
        .route("/api/inference/test", post(connections::trial))
        .route("/api/config", get(config).post(save_config))
        .route("/api/bind", post(bind))
        .route("/api/import", post(import))
        .route("/api/models/discover", post(discover))
        .route("/api/import/kiro", post(import_kiro))
        .route("/api/accounts/state", post(account_state))
        .route("/api/oauth/start", post(oauth_start))
        .route("/api/oauth/:id", get(oauth_result))
        .route("/api/oauth/:id/code", post(oauth_code))
        .route("/api/quotes/save", post(save_quotes))
        .route("/api/quotes/submit", post(submit_quotes))
        .route("/api/earnings", get(earnings))
        .route("/api/proxy/test", post(proxy_test))
        .route("/api/proxy/claude-oauth", post(oauth_proxy_test))
        .route("/api/probe", post(probe))
        .route("/api/local-key", get(local_key))
        .route("/api/claude-key", post(connect_claude))
        .route("/api/claude-key/recheck", post(recheck_claude))
        .route("/api/claude-key/disconnect", post(disconnect_claude))
        .route("/api/ollama/discover", post(discover_ollama))
        .route("/api/ollama/register", post(register_ollama))
        .route("/api/pause", post(pause))
        .route("/api/shutdown", post(shutdown));
    #[cfg(feature = "oauth-preview")]
    let protected = protected.merge(crate::claude_oauth_preview::management());
    let protected = protected.layer(middleware::from_fn_with_state(app.clone(), auth));
    let public = Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/modelcat.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../web/modelcat.svg"),
                )
            }),
        )
        .route(
            "/grain.png",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/grain.png").as_slice(),
                )
            }),
        )
        .route(
            "/workspace.css",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/workspace.css"),
                )
            }),
        )
        .route(
            "/navigation.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/navigation.js"),
                )
            }),
        )
        .route(
            "/visuals.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/visuals.js"),
                )
            }),
        )
        .route(
            "/drawer.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/drawer.js"),
                )
            }),
        )
        .route(
            "/request-history.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/request-history.js"),
                )
            }),
        )
        .route(
            "/connections.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/connections.js"),
                )
            }),
        )
        .route(
            "/healthz",
            get(|| async { Json(json!({"ok":true,"name":"channel-node"})) }),
        );
    #[cfg(feature = "oauth-preview")]
    let public = public.merge(crate::claude_oauth_preview::browser());
    public
        .merge(protected)
        .layer(axum::extract::DefaultBodyLimit::max(8 << 20))
        .with_state(app.clone())
        .merge(local_router(app))
}

pub fn local_router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(local_proxy))
        .route("/v1/chat/completions", post(local_proxy))
        .route("/v1/messages", post(local_proxy))
        .route("/v1/messages/count_tokens", post(local_proxy))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth))
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
    let local_api = request.uri().path().starts_with("/v1/");
    let presented = match headers.get("authorization") {
        Some(value) => value.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")),
        None if local_api => headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        None => None,
    }.unwrap_or("");
    use sha2::{Digest, Sha256};
    let a = Sha256::digest(presented.as_bytes());
    let expected = if local_api {
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
#[derive(serde::Deserialize, Default)]
struct HistoryFilter {
    since: Option<i64>,
    before: Option<i64>,
    source: Option<String>,
    status: Option<String>,
}

async fn request_history(State(app): State<Arc<App>>, axum::extract::Query(filter): axum::extract::Query<HistoryFilter>) -> Response {
    use sqlx::Row;
    let result: anyhow::Result<Value> = async {
        let since = filter.since.unwrap_or(0);
        let mut tx = app.store.pool.begin().await?;
        let groups = sqlx::query("SELECT source, count(*) AS n, SUM(status NOT IN ('ok','cancelled')) AS failed FROM request_history WHERE at >= ? GROUP BY source")
            .bind(since).fetch_all(&mut *tx).await?;
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT rowid,payload FROM request_history WHERE at >= ");
        query.push_bind(since);
        if let Some(before) = filter.before {
            query.push(" AND rowid < ").push_bind(before);
        }
        if let Some(source) = filter.source {
            query.push(" AND source = ").push_bind(source);
        }
        match filter.status.as_deref() {
            Some("failed") => { query.push(" AND status NOT IN ('ok','cancelled')"); }
            Some("ok") => { query.push(" AND status = 'ok'"); }
            Some("cancelled") => { query.push(" AND status = 'cancelled'"); }
            _ => {}
        }
        query.push(" ORDER BY rowid DESC LIMIT 51");
        let rows = query.build().fetch_all(&mut *tx).await?;
        let next = (rows.len() > 50).then(|| rows[49].get::<i64,_>("rowid"));
        let mut items = Vec::new();
        for row in rows.iter().take(50) {
            items.push(serde_json::from_str::<Value>(row.get("payload"))?);
        }
        tx.commit().await?;
        let counts: Vec<_> = groups.iter().map(|r| json!({"source":r.get::<String,_>("source"),"count":r.get::<i64,_>("n"),"failed":r.get::<i64,_>("failed")})).collect();
        Ok(json!({"items":items,"next":next,"counts":counts}))
    }.await;
    match result { Ok(data) => Json(data).into_response(), Err(err) => error(err) }
}

async fn request_detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match sqlx::query_scalar::<_, String>("SELECT payload FROM request_history WHERE id = ?")
        .bind(id).fetch_optional(&app.store.pool).await {
        Ok(Some(payload)) => ([(axum::http::header::CONTENT_TYPE,"application/json")],payload).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error":"本机未保存这笔请求的明细"}))).into_response(),
        Err(err) => error(err),
    }
}

async fn activity(State(app): State<Arc<App>>) -> Json<Value> {
    let runtime = app.runtime.read().await.clone();
    let requests: Vec<_> = app.connection_requests.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    let records: Vec<_> = app.records.lock().unwrap_or_else(|e| e.into_inner()).iter().rev()
        .take(24).cloned().collect();
    let local_active = requests.iter().filter(|request| !request.gateway).count();
    Json(json!({"active": runtime.active + local_active, "requests": requests, "recent": records,
        "gateway": {"connection": runtime.connection, "last_error": runtime.last_error,
                    "connecting_since": runtime.connecting_since, "retry_at": runtime.retry_at,
                    "now": crate::state::now()}}))
}

async fn token_usage(State(app): State<Arc<App>>, Path(range): Path<String>) -> Response {
    let (window, step) = match range.as_str() {
        "5m" => (300, 60), "1h" => (3600, 60), "24h" => (86400, 900),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    // Include the current (partial) bucket and whole preceding buckets.
    let now = crate::state::now();
    let end = now / 60 * 60 + 60;
    let start = end - window;
    let rows = sqlx::query_as::<_, (i64,i64,i64,i64,i64)>(
        "SELECT (minute - ?) / ? * ? + ?, SUM(requests), SUM(input), SUM(output), SUM(cache)
         FROM token_minutes WHERE minute >= ? AND minute <= ? GROUP BY 1 ORDER BY 1"
    ).bind(start).bind(step).bind(step).bind(start).bind(start).bind(now).fetch_all(&app.store.pool).await;
    match rows {
        Ok(rows) => Json(json!({"now": now, "start": start, "end": end, "step": step, "rows": rows})).into_response(),
        Err(e) => error(e),
    }
}

async fn traffic(State(app): State<Arc<App>>, Path(range): Path<String>) -> Response {
    let window = match range.as_str() {
        "5m" => 300,
        "1h" => 3600,
        "24h" => 86400,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let history = app.traffic.read().await;
    let now = crate::state::now();
    let samples = history.rates(now, window);
    let latest = history.latest(now);
    Json(json!({"now": now, "samples": samples, "latest": latest})).into_response()
}

pub(crate) fn claude_view(cfg: &Config) -> Value {
    match claude_api::source() {
        Ok(Some(source)) => json!({"models":source.models,"paused":source.paused(cfg),"workspace_id":source.workspace_id,"base_url":source.base_url}),
        Ok(None) => Value::Null,
        Err(e) => json!({"error":e.to_string()}),
    }
}

async fn status(State(app): State<Arc<App>>) -> Response {
    let accounts = if app.vm_workspace { vec![] } else { app.accounts.read().await.clone() };
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
    let claude = claude_view(&cfg);
    let workspace = app.vm_workspace;
    let connections = connections::views().unwrap_or_default();
    let pricing = {
        let catalog = app.pricing.read().await;
        let connection_names = connections.iter().filter(|c| c["protocol"] != "ollama" && c["oauth"] != true)
            .filter_map(|c| c["models"].as_array()).flatten().filter_map(|m| m["name"].as_str());
        let names = connection_names.chain(accounts.iter().flat_map(|a| a.models.iter().map(String::as_str)))
            .chain(claude["models"].as_array().into_iter().flatten().filter_map(Value::as_str));
        catalog.view(names)
    };
    let ollama_models: Vec<_> = app.ollama_models.read().await.iter().map(|model| model.id()).collect();
    let candidates = crate::offers::candidates(&cfg, &accounts, &connections, &ollama_models, &claude, workspace);
    let (local_models, local_aliases) = app.local_catalog().await;
    Json(json!({"version":env!("CARGO_PKG_VERSION"),"runtime":app.runtime.read().await.clone(),"config":cfg,"accounts":accounts,"account_status":pool,"records":records,"binding":binding.map(|b|json!({"gateway_url":b.gateway_url,"node_id":b.node_id,"currency":b.currency})),"available_models":app.available_models().await,"local_models":local_models,"local_aliases":local_aliases,"connections":connections,"quote_candidates":candidates,"reference_prices":pricing,"ollama_models":ollama_models,"claude_api":claude,"vm_workspace":workspace,"oauth_preview":workspace&&cfg!(feature="oauth-preview")})).into_response()
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
    let provider = v["provider"].as_str().unwrap_or("");
    match app.begin_oauth(provider).await {
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
async fn save_quotes(State(app): State<Arc<App>>, Json(edits): Json<Vec<crate::offers::Edit>>) -> Response {
    match crate::offers::save(&app, edits).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}
async fn submit_quotes(State(app): State<Arc<App>>, Json(selection): Json<Value>) -> Response {
    let _batch = app.quote_submission.lock().await;
    let Some(binding) = app.binding.read().await.clone() else { return error("请先绑定节点"); };
    let account = match gateway::bound_account(&binding).await {
        Ok(account) => account,
        Err(err) => return error(format!("无法核对已有报价：{err}")),
    };
    let existing = match account.get("quotes") {
        Some(Value::Array(rows)) => rows.as_slice(),
        Some(Value::Null) => &[],
        _ => return error("无法读取已有报价"),
    };
    let mut skipped = 0;
    let offers = app.config.read().await.offers.clone();
    let mut results = vec![];
    let mut pending = Vec::new();
    for o in offers.iter().filter(|o| {
        o.enabled
            && selection["models"]
                .as_array()
                .is_none_or(|models| models.iter().any(|m| m.as_str() == Some(o.model.as_str())))
    }) {
        if !app.offer_source_allowed(&o.provider, &o.upstream_model).await {
            continue;
        }
        let micros = |price: &str| price.parse::<f64>().ok().map(|n| (n * 1e6).round() as i64);
        let active = existing.iter().filter(|q| {
            q["node_id"] == binding.node_id && q["enabled"] == true && q["model"] == o.model
        });
        let same_quote = |q: &Value| {
            q["currency"] == binding.currency && q["provider"] == o.provider &&
            q["upstream_model"] == o.upstream_model &&
            q["input_micros_per_m"].as_i64() == micros(&o.input_price_per_m) &&
            q["output_micros_per_m"].as_i64() == micros(&o.output_price_per_m)
        };
        if active.clone().any(|q| q["approved"] == false && !same_quote(q)) {
            pending.push(o.model.clone());
            continue;
        }
        if active.clone().any(same_quote) {
            skipped += 1;
            continue;
        }
        match gateway::submit_quote(&binding, o).await {
            Ok(v) => results.push(v),
            Err(e) => return error(format!("报价 {} 提交失败（已提交 {} 项）：{e}", o.model, results.len())),
        }
    }
    Json(json!({"quotes":results,"skipped":skipped,"pending_models":pending})).into_response()
}
async fn earnings(State(app): State<Arc<App>>) -> Response {
    match gateway::account(&app).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
async fn proxy_test(State(app): State<Arc<App>>) -> Response {
    let started = std::time::Instant::now();
    match claude_api::check_network(&app).await {
        Ok(status) => Json(json!({
            "reachable": true,
            "http_status": status.as_u16(),
            "elapsed_ms": started.elapsed().as_millis()
        })).into_response(),
        Err(e) => error(e),
    }
}

async fn oauth_proxy_test(State(app): State<Arc<App>>) -> Response {
    let config = app.config.read().await.clone();
    match claude_api::check_oauth_network(&config).await {
        Ok((authorization_status, token_status)) => Json(json!({
            "node_reachable":true,
            "metadata_verified":true,
            "authorization_http_status":authorization_status.as_u16(),
            "token_probe_http_status":token_status.as_u16(),
            "proxy_configured":true,
            "browser_route_verified":false,
        })).into_response(),
        Err(e) => error(e),
    }
}
async fn pause(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    match app.set_paused(v["paused"].as_bool().unwrap_or(true)).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}
async fn shutdown(State(app): State<Arc<App>>) -> Response {
    app.shutdown_requested.notify_one();
    Json(json!({"draining":true})).into_response()
}
async fn models(State(app): State<Arc<App>>) -> Response {
    let (ids, aliases) = app.local_catalog().await;
    let mut public_ids: std::collections::BTreeSet<_> = ids.into_iter().collect();
    for (name, route) in aliases {
        public_ids.remove(&route);
        public_ids.insert(name);
    }
    let models: Vec<_> = public_ids.into_iter()
        .map(|id| json!({"id":id,"object":"model","owned_by":"local"}))
        .collect();
    Json(json!({"object":"list","data":models})).into_response()
}

async fn local_key(State(app): State<Arc<App>>) -> Response {
    ([ ("cache-control", "no-store") ], Json(json!({"key":app.api_token}))).into_response()
}

#[derive(serde::Deserialize)]
struct ClaudeKey {
    key: String,
    workspace_id: Option<String>,
    base_url: Option<String>,
}

async fn connect_claude(State(app): State<Arc<App>>, Json(body): Json<ClaudeKey>) -> Response {
    let _guard = app.claude_setup.lock().await;
    let config = app.config.read().await.clone();
    match claude_api::connect(&config, &body.key, body.workspace_id.as_deref(), body.base_url.as_deref()).await {
        Ok(models) => Json(json!({"models":models})).into_response(),
        Err(e) => error(e),
    }
}

async fn disconnect_claude(State(app): State<Arc<App>>) -> Response {
    let _guard = app.claude_setup.lock().await;
    match claude_api::disconnect() {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(e),
    }
}

async fn recheck_claude(State(app): State<Arc<App>>) -> Response {
    let _guard = app.claude_setup.lock().await;
    let config = app.config.read().await.clone();
    match claude_api::recheck(&config).await {
        Ok(models) => Json(json!({"models":models})).into_response(),
        Err(e) => error(e),
    }
}

async fn discover_ollama(Json(body): Json<Value>) -> Response {
    let origin = match crate::ollama::origin(body["origin"].as_str().unwrap_or_default()) {
        Ok(origin) => origin,
        Err(e) => return error(e),
    };
    match crate::ollama::discover(&origin).await {
        Ok(models) => Json(json!({"models":models})).into_response(),
        Err(e) => error(e),
    }
}

async fn register_ollama(State(app): State<Arc<App>>, Json(body): Json<Value>) -> Response {
    let origin = match crate::ollama::origin(body["origin"].as_str().unwrap_or_default()) {
        Ok(origin) => origin,
        Err(e) => return error(e),
    };
    let name = body["model"].as_str().unwrap_or_default().to_owned();
    let model = crate::ollama::Model { origin: origin.to_string(), name };
    let model_id = model.id();
    match crate::ollama::register(&app, model).await {
        Ok(()) => Json(json!({"ok":true,"model_id":model_id})).into_response(),
        Err(e) => error(e),
    }
}

async fn local_proxy(
    State(app): State<Arc<App>>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    local_proxy_with_trace(app, uri, headers, body, None).await
}

async fn local_proxy_with_trace(
    app: Arc<App>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
    trace: Option<uuid::Uuid>,
) -> Response {
    if app.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return claude_error(StatusCode::SERVICE_UNAVAILABLE, "workspace is stopping");
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return error("request body must be valid JSON");
    };
    let model = request["model"].as_str().unwrap_or_default();
    let snapshot = |source: &str, host: Option<String>| crate::state::LocalRecord {
        model: model.to_owned(),
        source_label: Some(source.to_owned()),
        endpoint: host,
        upstream_bytes_out: body.len() as u64,
        ..Default::default()
    };
    if connections::is_model_id(model) {
        return connections::proxy(app, model, uri.path(), request.clone()).await;
    }
    let connection_alias = app.config.read().await.offers.iter()
        .find(|offer| offer.enabled && offer.model == model && connections::is_model_id(&offer.upstream_model))
        .map(|offer| offer.upstream_model.clone());
    if let Some(id) = connection_alias {
        return connections::proxy(app, &id, uri.path(), request).await;
    }
    let (_, aliases) = app.local_catalog().await;
    if let Some(id) = aliases.get(model) {
        return connections::proxy(app, id, uri.path(), request).await;
    }
    if uri.path() == "/v1/chat/completions" {
        return claude_error(StatusCode::NOT_FOUND, "select a Chat Completions connection");
    }
    #[cfg(feature = "oauth-preview")]
    if app.vm_workspace && model == crate::claude_oauth_preview::MODEL
        && matches!(uri.path(), "/v1/messages" | "/v1/messages/count_tokens")
    {
        let record = snapshot("Claude 模拟授权", Some(crate::claude_oauth_preview::HOST.into()));
        return connections::trace_response(app.clone(), record, trace, async {
            crate::claude_oauth_preview::forward(&app, &uri, body).await
                .map_err(|error| claude_error(StatusCode::BAD_GATEWAY, error))
        }).await;
    }
    let ollama_model = app.ollama_models.read().await.iter()
        .find(|entry| entry.id() == model).cloned();
    if let Some(ollama_model) = ollama_model {
        let record = snapshot("Ollama", crate::ollama::origin(&ollama_model.origin).ok().map(|url| url.authority().to_owned()));
        return connections::trace_response(app, record, trace, async {
            crate::ollama::forward(ollama_model, &uri, request).await
                .map_err(|error| claude_error(StatusCode::BAD_GATEWAY, error))
        }).await;
    }
    if matches!(uri.path(), "/v1/messages" | "/v1/messages/count_tokens") {
        let source = match claude_api::source() {
            Ok(source) => source,
            Err(e) => return claude_error(StatusCode::INTERNAL_SERVER_ERROR, e),
        };
        if let Some(source) = source.filter(|source| source.models.iter().any(|id| id == model)) {
            let record = snapshot("Claude API Key", source.origin().ok().map(|url| url.authority().to_owned()));
            return connections::trace_response(app.clone(), record, trace, async {
                claude_api::forward(&app, source, uri, headers, body).await
                    .map_err(|error| claude_error(StatusCode::BAD_GATEWAY, error))
            }).await;
        }
    }
    local_request(app, uri, request).await
}

fn claude_error(status: StatusCode, error: impl std::fmt::Display) -> Response {
    let error_type = if status == StatusCode::NOT_FOUND { "not_found_error" } else { "api_error" };
    (status, Json(json!({"type":"error","error":{"type":error_type,"message":error.to_string()}}))).into_response()
}

pub(crate) async fn trial_saved_model(app: Arc<App>, model: &str, prompt: &str, trace: Option<uuid::Uuid>, stream: bool) -> Response {
    let offer = app.config.read().await.offers.iter()
        .find(|offer| offer.enabled && offer.model == model).cloned();
    if let Some(offer) = &offer {
        if connections::is_model_id(&offer.upstream_model) {
            return connections::run_trial_with_trace(app, &offer.upstream_model, prompt, trace, stream).await;
        }
    }
    let claude_key_model = claude_api::source().is_ok_and(|source| {
        source.is_some_and(|source| source.models.iter().any(|id| id == model))
    });
    let claude_preview = {
        #[cfg(feature = "oauth-preview")]
        { model == crate::claude_oauth_preview::MODEL }
        #[cfg(not(feature = "oauth-preview"))]
        { false }
    };
    let upstream = offer.as_ref().map_or(model, |o| o.upstream_model.as_str());
    let limit = connections::TRIAL_OUTPUT_TOKENS.min(crate::model_capabilities::local_output_limit(upstream));
    let is_messages = claude_key_model || claude_preview
        || offer.is_some_and(|offer| offer.provider == "claude");
    let (uri, request) = if is_messages {
        ("/v1/messages", json!({"model":model, "messages":[{"role":"user","content":prompt}], "max_tokens":limit,"stream":stream}))
    } else {
        ("/v1/responses", json!({"model":model,"input":prompt,"max_output_tokens":limit,"stream":stream}))
    };
    let mut response = local_proxy_with_trace(
        app,
        uri.parse().expect("static local route"),
        HeaderMap::new(),
        axum::body::Bytes::from(request.to_string()),
        trace,
    ).await;
    response.headers_mut().insert("x-modelcat-output-limit", limit.into());
    response
}

async fn probe(State(app): State<Arc<App>>, Json(v): Json<Value>) -> Response {
    let model = v["model"].as_str().unwrap_or("");
    let cfg = app.config.read().await.clone();
    let Some(offer) = cfg.offers.iter().find(|o| o.model == model && o.enabled) else {
        return error("unknown enabled model");
    };
    if connections::is_model_id(&offer.upstream_model) {
        return connections::run_trial(app, &offer.upstream_model, "Reply with OK.").await;
    }
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
    local_request(app, uri.parse().unwrap(), body).await
}

async fn local_request(
    app: Arc<App>,
    uri: axum::http::Uri,
    mut body: Value,
) -> Response {
    let model = body["model"].as_str().unwrap_or("").to_string();
    if uri.path() == "/v1/messages/count_tokens" {
        return claude_error(StatusCode::NOT_FOUND, "token counting is unavailable for this model");
    }
    let cfg = app.config.read().await.clone();
    let Some(offer) = cfg
        .offers
        .iter()
        .find(|o| o.enabled && (o.model == model || o.upstream_model == model))
        .cloned()
    else {
        return error("model is not configured");
    };
    if !app.offer_source_allowed(&offer.provider, &offer.upstream_model).await {
        return claude_error(StatusCode::FORBIDDEN, "请在「模型配置」连接 Claude API Key 或登记 Ollama 模型。");
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
    if !(1..=crate::model_capabilities::local_output_limit(&offer.upstream_model)).contains(&budget)
    {
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
    tokio::spawn(runner::execute(app, payload, verifier, tx, cancel_rx, false));
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

pub(crate) fn buffered_response(bytes: &[u8]) -> Option<Value> {
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

pub(crate) fn prepare_codex_body(body: &mut Value) {
    if let Some(fields) = body.as_object_mut() {
        for key in ["max_tokens", "max_completion_tokens"] {
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
    fn concurrent_quote_retries_and_pending_price_changes_do_not_duplicate_reviews() {
        crate::testenv::with_temp_data_dir("quote-retry", || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                let rows = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
                let gateway = Router::new()
                    .route("/market/api/node/account", get({
                        let rows = rows.clone();
                        move || async move { Json(json!({"quotes":*rows.lock().await})) }
                    }))
                    .route("/market/api/node/quotes", post({
                        let rows = rows.clone();
                        move |Json(body): Json<Value>| async move {
                            let rate = body["input_price_per_m"].as_str().unwrap().parse::<f64>().unwrap();
                            let quote = json!({"node_id":"test","currency":"USD","enabled":true,"approved":false,
                                "model":body["model"],"provider":body["provider"],"upstream_model":body["upstream_model"],
                                "input_micros_per_m":(rate * 1e6).round() as i64,"output_micros_per_m":600000});
                            rows.lock().await.push(quote.clone());
                            Json(quote)
                        }
                    }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move { axum::serve(listener, gateway).await.unwrap(); });
                let app = App::open_for_mode(false).await.unwrap();
                *app.binding.write().await = Some(Binding { gateway_url:endpoint, node_id:"test".into(),
                    currency:"USD".into(), ..Default::default() });
                app.config.write().await.offers = vec![crate::config::Offer {
                    model:"test-model".into(), provider:"custom".into(), upstream_model:"test-model".into(),
                    input_price_per_m:"0.150000".into(), output_price_per_m:"0.60".into(), enabled:true,
                }];
                let submit = || submit_quotes(State(app.clone()), Json(json!({})));
                let (first, second) = tokio::join!(submit(),submit());
                assert_eq!(first.status(), StatusCode::OK);
                assert_eq!(second.status(), StatusCode::OK);
                assert_eq!(rows.lock().await.len(),1);
                app.config.write().await.offers[0].input_price_per_m = "0.150001".into();
                let blocked = submit().await;
                let body = axum::body::to_bytes(blocked.into_body(), 4096).await.unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["pending_models"], json!(["test-model"]));
                assert_eq!(body["quotes"], json!([]));
                assert_eq!(rows.lock().await.len(),1);
                rows.lock().await[0]["approved"] = json!(true);
                assert_eq!(submit().await.status(), StatusCode::OK);
                assert_eq!(rows.lock().await.len(),2);
                server.abort();
            });
        });
    }

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
    fn codex_probe_preserves_responses_output_limit_and_normalizes_input() {
        let mut body = json!({"input":"Reply with OK.","max_output_tokens":32,"max_tokens":32,"max_completion_tokens":32,"stream":true,"store":false,"instructions":"rules"});
        prepare_codex_body(&mut body);
        assert_eq!(body["input"][0]["content"][0]["text"], "Reply with OK.");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["max_output_tokens"], 32);
        for key in ["max_tokens", "max_completion_tokens"] {
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
