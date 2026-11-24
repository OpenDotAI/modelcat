use crate::{oauth, state::App};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Path, Query, State},
    body::Bytes,
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::{Duration, Instant}};

const PROXY: &str = "http://127.0.0.1:18779";
const ISSUER: &str = "http://claude-oauth.fixture.invalid";
pub(crate) const HOST: &str = "claude-oauth.fixture.invalid";
const CLIENT_ID: &str = "modelcat-oauth-preview";
pub(crate) const MODEL: &str = "claude-oauth-preview";

pub(crate) struct Session {
    id: String,
    state: String,
    pkce: oauth::Pkce,
    access: Option<String>,
    refresh: Option<String>,
    status: &'static str,
    expires: Instant,
}

pub(crate) fn management() -> Router<Arc<App>> {
    Router::new()
        .route("/api/claude-oauth-preview", post(start).get(status))
        .route("/api/claude-oauth-preview/refresh", post(refresh))
}

pub(crate) fn browser() -> Router<Arc<App>> {
    Router::new()
        .route("/oauth-preview/:id/callback", get(callback))
        .route("/oauth-preview/:id/result", get(browser_result))
}

fn http() -> Result<Client> {
    Ok(Client::builder().no_proxy().proxy(reqwest::Proxy::all(PROXY)?)
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(6)).build()?)
}

fn provider() -> oauth::OAuthProvider {
    oauth::OAuthProvider {
        name: "claude-preview".into(),
        authorize_url: "http://claude-oauth.fixture.invalid/authorize",
        token_url: "http://claude-oauth.fixture.invalid/token",
        client_id: CLIENT_ID.into(),
        client_secret: None,
        scope: "preview:inference",
        extra_authorize_params: &[],
        anthropic_style: true,
        redirect_port: None,
        redirect_path: "/callback",
    }
}

fn callback_uri(id: &str) -> String {
    format!("http://127.0.0.1:19701/oauth-preview/{id}/callback")
}

async fn start(State(app): State<Arc<App>>) -> Response {
    if !app.vm_workspace {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut slot = app.claude_oauth_preview.lock().await;
    let ready = async {
        http()?.get(format!("{ISSUER}/ready")).send().await?.error_for_status()?;
        Ok::<_, anyhow::Error>(())
    }.await;
    if ready.is_err() {
        *slot = None;
        return failure("模拟授权代理不可用，请启动测试代理后重试。");
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let state = uuid::Uuid::new_v4().to_string();
    let pkce = oauth::gen_pkce();
    let mut url = Url::parse(provider().authorize_url).expect("constant preview authorize URL");
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"), ("client_id", CLIENT_ID), ("redirect_uri", &callback_uri(&id)),
        ("state", &state), ("code_challenge", &pkce.challenge), ("code_challenge_method", "S256"),
    ]);
    *slot = Some(Session {
        id, state, pkce, access: None, refresh: None, status: "pending",
        expires: Instant::now() + Duration::from_secs(300),
    });
    Json(json!({"url": url.as_str()})).into_response()
}

async fn status(State(app): State<Arc<App>>) -> Response {
    if !app.vm_workspace {
        return StatusCode::NOT_FOUND.into_response();
    }
    let slot = app.claude_oauth_preview.lock().await;
    let status = slot.as_ref().filter(|session| session.expires > Instant::now())
        .map_or("idle", |session| session.status);
    Json(json!({"status":status})).into_response()
}

fn pending<'a>(slot: &'a mut Option<Session>, id: &str) -> Option<&'a mut Session> {
    slot.as_mut().filter(|session| {
        session.id == id && session.expires > Instant::now() && session.status == "pending"
    })
}

fn page(title: &str, action: &str) -> Response {
    let body = format!(
        r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title>
        <style>html{{color-scheme:light dark;font:16px system-ui}}main{{max-width:34rem;margin:5rem auto;padding:1.25rem}}h1{{font-size:1.5rem}}button{{font:inherit;padding:.4rem .8rem}}</style>
        <main><h1>{title}</h1>{action}</main></html>"#
    );
    ([(header::CACHE_CONTROL, "no-store"), (header::REFERRER_POLICY, "no-referrer")], Html(body)).into_response()
}

#[derive(Deserialize)]
struct Grant {
    state: String,
    code: String,
}

async fn callback(State(app): State<Arc<App>>, Path(id): Path<String>, Query(grant): Query<Grant>) -> Response {
    let mut slot = app.claude_oauth_preview.lock().await;
    let Some(session) = pending(&mut slot, &id).filter(|session| {
        session.state == grant.state && grant.code.len() == 32
            && grant.code.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    }) else {
        return page("模拟授权已失效", "<p>请返回 ModelCat 重新发起。</p>");
    };
    let result = async {
        let code = oauth::AuthCode {
            code: grant.code,
            state: session.state.clone(), redirect_uri: callback_uri(&id),
            verifier: session.pkce.verifier.clone(),
        };
        let token = oauth::exchange_with_client(&http()?, &provider(), &code).await?;
        save_token(session, token)
    }.await;
    if result.is_err() {
        session.status = "paused";
    }
    Redirect::to(&format!("/oauth-preview/{id}/result")).into_response()
}

async fn browser_result(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let slot = app.claude_oauth_preview.lock().await;
    match slot.as_ref().filter(|session| session.id == id && session.expires > Instant::now())
        .map(|session| session.status)
    {
        Some("connected") => page("模拟授权成功", "<p>可以关闭此页并返回 ModelCat。</p>"),
        Some("paused") => page("模拟授权已暂停", "<p>请恢复代理，再从 ModelCat 重新发起。</p>"),
        _ => page("模拟授权已失效", "<p>请返回 ModelCat 重新发起。</p>"),
    }
}

fn save_token(session: &mut Session, token: Value) -> Result<()> {
    let access = token["access_token"].as_str()
        .filter(|token| token.starts_with("preview-access-")).context("missing preview access token")?;
    let refresh = token["refresh_token"].as_str()
        .filter(|token| token.starts_with("preview-refresh-")).context("missing preview refresh token")?;
    session.access = Some(access.to_owned());
    session.refresh = Some(refresh.to_owned());
    session.status = "connected";
    Ok(())
}

pub(crate) async fn model_ready(app: &App) -> bool {
    app.vm_workspace && app.claude_oauth_preview.lock().await.as_ref().is_some_and(|session| {
        session.status == "connected" && session.expires > Instant::now()
    })
}

pub(crate) async fn forward(app: &App, uri: &Uri, body: Bytes) -> Result<Response> {
    let mut slot = app.claude_oauth_preview.lock().await;
    let session = slot.as_mut().filter(|session| session.expires > Instant::now())
        .context("请先完成 Claude 模拟授权。")?;
    ensure!(session.status == "connected", "模拟调用已暂停，请恢复代理并刷新令牌。");
    let result = async {
        let access = session.access.as_deref().context("missing preview access token")?;
        let response = http()?.post(format!("{ISSUER}{}", uri.path()))
            .bearer_auth(access)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body).send().await?.error_for_status()?;
        ensure!(response.status().is_success(), "unexpected preview inference response");
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned()
            .context("missing preview response content type")?;
        Ok::<_, anyhow::Error>(([(header::CONTENT_TYPE, content_type)], response.bytes().await?).into_response())
    }.await;
    match result {
        Ok(response) => Ok(response),
        Err(_) => {
            session.status = "paused";
            anyhow::bail!("模拟调用已暂停，请恢复代理并刷新令牌。");
        }
    }
}

async fn refresh(State(app): State<Arc<App>>) -> Response {
    let mut slot = app.claude_oauth_preview.lock().await;
    let Some(session) = slot.as_mut().filter(|session| session.expires > Instant::now()) else {
        return failure("请先完成模拟授权。");
    };
    let Some(refresh_token) = session.refresh.as_deref() else {
        return failure("请先完成模拟授权。");
    };
    let request = json!({
        "grant_type":"refresh_token", "client_id":CLIENT_ID, "refresh_token":refresh_token
    });
    let result = async {
        let token = http()?.post(provider().token_url).json(&request)
            .send().await?.error_for_status()?.json().await?;
        save_token(session, token)
    }.await;
    if result.is_err() {
        session.status = "paused";
        return failure("模拟令牌刷新已暂停。请恢复代理，重试或重新发起授权。");
    }
    Json(json!({"status":"connected"})).into_response()
}

fn failure(message: &str) -> Response {
    (StatusCode::BAD_GATEWAY, Json(json!({"error":message}))).into_response()
}
