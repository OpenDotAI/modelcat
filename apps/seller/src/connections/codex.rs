use super::*;
use crate::state::now;

const BASE: &str = "https://chatgpt.com/backend-api/codex/";
const AUTH: &str = "https://auth.openai.com";
const DEVICE: &str = "https://auth.openai.com/api/accounts/deviceauth";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credential {
    refresh: String,
    expires: i64,
    account: String,
}

impl Credential {
    fn from_tokens(tokens: &Value, previous: Option<&Self>) -> Result<(String, Self)> {
        let access = field(tokens, "access_token")?;
        HeaderValue::from_str(access).context("授权服务返回了无效令牌")?;
        let refresh = tokens["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| previous.map(|old| old.refresh.as_str()))
            .context("缺少刷新令牌，请重新授权")?;
        let claims = asale_client_core::cli_import::jwt_claims(access).unwrap_or_default();
        let account = asale_client_core::executor::chatgpt_account_id(access)
            .or_else(|| previous.map(|old| old.account.clone()))
            .unwrap_or_default();
        let expires = tokens["expires_in"]
            .as_i64()
            .map(|ttl| now().saturating_add(ttl))
            .or_else(|| claims["exp"].as_i64())
            .unwrap_or_else(|| now() + 3600);
        Ok((
            access.into(),
            Self {
                refresh: refresh.into(),
                account,
                expires,
            },
        ))
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("OpenAI 授权响应缺少必要字段")
}

pub(crate) struct Pending {
    id: String,
    outcome: Option<Result<Value, String>>,
    started: Instant,
    next_poll: Instant,
    interval: Duration,
    browser: Option<super::codex_browser::Browser>,
    device: String,
    code: String,
    label: String,
    target: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Start {
    #[serde(default)]
    browser: bool,
    label: String,
    id: Option<String>,
}

async fn json_reply(request: reqwest::RequestBuilder) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("无法连接 OpenAI，请检查工作区代理")?;
    ensure!(
        response.status().is_success(),
        "OpenAI 返回 HTTP {}，请检查账号授权或网络",
        response.status().as_u16()
    );
    Ok(response.json().await.context("无法读取 OpenAI 响应")?)
}

pub(crate) async fn start(State(app): State<Arc<App>>, Json(input): Json<Start>) -> Response {
    let result = async {
        let label = input.label.trim();
        ensure!(
            !label.is_empty() && label.chars().count() <= 64,
            "请输入 1 到 64 个字符的连接名称"
        );
        if let Some(id) = &input.id {
            ensure!(
                saved()?.iter().any(|c| c.id == *id && c.oauth.is_some()),
                "OAuth 连接已删除"
            );
        }
        let mut slot = app.codex_device.lock().await;
        if let Some(existing) = slot.as_ref().filter(|p| p.outcome.is_none() && p.started.elapsed() < Duration::from_secs(900)) {
            ensure!(existing.label==label && existing.target==input.id && existing.browser.is_some()==input.browser,"已有授权正在进行，请完成或取消后重试");
            return Ok(json!({"id":existing.id,"code":existing.code,"url":existing.browser.as_ref().map(|b|&b.url),"interval":existing.interval.as_secs()}));
        }
        *slot=None;
        if input.browser {
            let browser=super::codex_browser::Browser::start().await?;
            let id=Uuid::new_v4().to_string();
            let data=json!({"url":browser.url,"id":id,"interval":2});
            *slot=Some(Pending {id:id.clone(),outcome:None,started:Instant::now(),next_poll:Instant::now(),interval:Duration::from_secs(2),browser:Some(browser),device:String::new(),code:String::new(),label:label.into(),target:input.id});
            let workspace = app.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let mut slot = workspace.codex_device.lock().await;
                    let Some(session) = slot.as_mut().filter(|s| s.id == id) else { break };
                    if session.started.elapsed() >= Duration::from_secs(900) {
                        *slot = None;
                        break;
                    }
                    if !matches!(advance(&workspace, session).await, Ok(v) if v["pending"] == true) {
                        break;
                    }
                }
            });
            return Ok(data);
        }
        let http = client(&*app.config.read().await, AUTH)?;
        let data = json_reply(
            http.post(format!("{DEVICE}/usercode"))
                .json(&json!({"client_id":crate::oauth::codex_client_id()})),
        )
        .await?;
        let id = Uuid::new_v4().to_string();
        let code = data["user_code"]
            .as_str()
            .or(data["usercode"].as_str())
            .context("授权服务未返回设备码")?;
        let seconds = data["interval"]
            .as_u64()
            .or_else(|| data["interval"].as_str()?.parse().ok())
            .unwrap_or(5)
            .clamp(1, 60);
        let interval = Duration::from_secs(seconds);
        let started = Instant::now();
        *slot = Some(Pending {
            id: id.clone(),
            outcome: None,
            started,
            next_poll: started + interval,
            interval,
            browser: None,
            device: field(&data, "device_auth_id")?.into(),
            code: code.into(),
            label: label.into(),
            target: input.id,
        });
        Ok(json!({"id":id,"code":code,"interval":seconds}))
    }
    .await;
    reply(result)
}

pub(crate) async fn browser_launch(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let slot = app.codex_device.lock().await;
    let Some(attempt) = slot.as_ref().filter(|p| p.id == id && p.started.elapsed() < Duration::from_secs(900)) else {
        return reply(Err(anyhow!("授权已结束，请重新登录")));
    };
    let url = attempt.browser.as_ref().map_or("https://auth.openai.com/codex/device", |b| &b.url);
    let config = app.config.read().await;
    let proxy = match config.upstream_proxy.as_str() {
        "auto" => asale_client_core::http::auto_proxy().unwrap_or_else(|| "auto".into()),
        setting => setting.to_owned(),
    };
    let mut response = Json(json!({"url":url, "proxy":proxy})).into_response();
    response.headers_mut().insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

pub(crate) async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let mut slot = app.codex_device.lock().await;
    if slot.as_ref().is_some_and(|s| s.id == id) {
        *slot = None;
    }
    Json(json!({"ok":true})).into_response()
}

pub(crate) async fn poll(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let mut slot = app.codex_device.lock().await;
    let Some(session) = slot.as_mut().filter(|s| s.id == id) else {
        return failure(StatusCode::NOT_FOUND, "授权已取消或过期");
    };
    if session.started.elapsed() >= Duration::from_secs(900) {
        *slot = None;
        return failure(StatusCode::GONE, "设备码已过期，请重新授权");
    }
    reply(advance(&app, session).await)
}

async fn advance(app: &App, session: &mut Pending) -> Result<Value> {
    if let Some(outcome) = &session.outcome {
        return outcome.clone().map_err(anyhow::Error::msg);
    }
    let result = finish(app, session).await;
    if !matches!(&result, Ok(v) if v["pending"] == true) {
        session.browser.take();
        session.outcome = Some(result.as_ref().map(Clone::clone).map_err(|e| format!("{e:#}")));
    }
    result
}

async fn finish(app: &App, s: &mut Pending) -> Result<Value> {
    if Instant::now() < s.next_poll {
        return Ok(json!({"pending":true}));
    }
    s.next_poll = Instant::now() + s.interval;
    let config = app.config.read().await.clone();
    let http = client(&config, AUTH)?;
    let (code, verifier, redirect) = if let Some(browser) = &s.browser {
        let Some(code) = browser.take()? else {
            return Ok(json!({"pending":true}));
        };
        (
            code,
            browser.verifier.clone(),
            super::codex_browser::CALLBACK,
        )
    } else {
        let response = http
            .post(format!("{DEVICE}/token"))
            .json(&json!({"device_auth_id":s.device,"user_code":s.code}))
            .timeout(Duration::from_secs(20))
            .send()
            .await?;
        if matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ) {
            return Ok(json!({"pending":true}));
        }
        ensure!(
            response.status().is_success(),
            "设备授权失败，请重试（HTTP {}）",
            response.status().as_u16()
        );
        let data: Value = response.json().await?;
        (
            field(&data, "authorization_code")?.to_owned(),
            field(&data, "code_verifier")?.to_owned(),
            "https://auth.openai.com/deviceauth/callback",
        )
    };
    let tokens = json_reply(http.post(format!("{AUTH}/oauth/token")).form(&[
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("client_id", &crate::oauth::codex_client_id()),
        ("code_verifier", verifier.as_str()),
        ("redirect_uri", redirect),
    ]))
    .await.context("已收到授权码，换取令牌失败")?;
    let (key, credential) = Credential::from_tokens(&tokens, None).context("无法读取授权凭据")?;
    let connection = Connection {
        id: s
            .target
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
        label: s.label.clone(),
        base: BASE.into(),
        protocol: Protocol::Responses,
        key,
        oauth: Some(credential),
        models: vec![],
    };
    save_authorized(app, connection, s.target.as_deref()).await.context("保存 OAuth 连接失败")
}

async fn save_authorized(app: &App, mut connection: Connection, target: Option<&str>) -> Result<Value> {
    let guard = app.connection_setup.lock().await;
    let mut entries = saved()?;
    if let Some(id) = target {
        let previous = entries
            .iter_mut()
            .find(|c| c.id == *id && c.oauth.is_some())
            .context("原连接已删除")?;
        connection.models.clone_from(&previous.models);
        *previous = connection.clone();
    } else {
        ensure!(entries.len() < 128, "最多保存 128 个模型连接");
        entries.push(connection.clone());
    }
    persist(&entries)?;
    drop(guard);
    match super::check_saved(app, connection.id.clone()).await {
        Ok(updated) => Ok(updated.view()),
        Err(_) => {
            let mut view = connection.view();
            view["model_check_failed"] = json!(true);
            Ok(view)
        }
    }
}

fn reply(result: Result<Value>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

pub(super) async fn discover(c: &Connection, config: &Config) -> Result<Vec<String>> {
    let http = client(config, BASE)?;
    let data = json_reply(authorize(c, http.get(format!("{BASE}models"))).query(&[(
        "client_version",
        asale_client_core::executor::codex_cli_version(),
    )]))
    .await?;
    let mut models: Vec<String> = data["models"]
        .as_array()
        .context("无法读取订阅模型清单")?
        .iter()
        .filter_map(|r| r["slug"].as_str())
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .map(String::from)
        .collect();
    models.sort();
    models.dedup();
    Ok(models)
}

pub(super) fn authorize(
    c: &Connection,
    request: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut request = request.bearer_auth(&c.key);
    if let Some(credentials) = &c.oauth {
        if !credentials.account.is_empty() {
            request = request.header("ChatGPT-Account-Id", &credentials.account);
        }
    }
    request
}

pub(super) async fn fresh(app: &App, id: &str, force: bool) -> Result<Connection> {
    let _guard = app.connection_setup.lock().await;
    let mut entries = saved()?;
    let c = entries
        .iter_mut()
        .find(|c| c.id == id)
        .context("连接已删除")?;
    let old = c.oauth.as_ref().context("请选择 OAuth 连接")?;
    if force || old.expires <= now() + 120 {
        let http = client(&*app.config.read().await, AUTH)?;
        let tokens = json_reply(http.post(format!("{AUTH}/oauth/token")).form(&[
            ("client_id", &crate::oauth::codex_client_id()),
            ("grant_type", &"refresh_token".to_owned()),
            ("refresh_token", &old.refresh),
        ]))
        .await
        .context("刷新令牌失败，请检查网络或重新授权")?;
        let (key, credential) = Credential::from_tokens(&tokens, Some(old))?;
        c.key = key;
        c.oauth = Some(credential);
        let updated = c.clone();
        persist(&entries)?;
        return Ok(updated);
    }
    Ok(c.clone())
}

pub(crate) async fn refresh(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    reply(fresh(&app, &id, true).await.map(|c| c.view()))
}

#[derive(Deserialize)]
pub(crate) struct Callback {
    url: String,
}
pub(crate) async fn callback(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(input): Json<Callback>,
) -> Response {
    let slot = app.codex_device.lock().await;
    reply(
        slot.as_ref()
            .filter(|s| s.id == id && s.started.elapsed() < Duration::from_secs(900))
            .and_then(|s| s.browser.as_ref())
            .context("授权已结束")
            .and_then(|b| b.submit(&input.url))
            .map(|_| json!({"ok":true})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_survives_unavailable_model_list() {
        crate::testenv::with_temp_data_dir("oauth-save-before-discover", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let app = App::open_for_mode(true).await.unwrap();
                // The test proxy immediately rejects upstream connections.
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                app.config.write().await.upstream_proxy = format!("http://{}", listener.local_addr().unwrap());
                tokio::spawn(async move { while let Ok((socket, _)) = listener.accept().await { drop(socket); } });
                let (key, credential) = Credential::from_tokens(&json!({
                    "access_token":"fixture", "refresh_token":"fixture-refresh", "expires_in":3600
                }), None).unwrap();
                let mut connection = Connection {
                    id: "fixture".into(), label: "fixture".into(), base: BASE.into(),
                    protocol: Protocol::Responses, key, oauth: Some(credential), models: vec![],
                };
                let view = save_authorized(&app, connection.clone(), None).await.unwrap();
                assert_eq!(view["model_check_failed"], true);
                let mut entries = saved().unwrap();
                assert_eq!(entries.len(), 1);
                assert!(entries[0].oauth.is_some());
                entries[0].models.push("previous-model".into());
                persist(&entries).unwrap();
                connection.key = "renewed".into();
                save_authorized(&app, connection, Some("fixture")).await.unwrap();
                let entries = saved().unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].key, "renewed");
                assert_eq!(entries[0].models, ["previous-model"]);
            });
        });
    }
}
