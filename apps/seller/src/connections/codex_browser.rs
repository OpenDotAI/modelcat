use super::*;
use axum::routing::get;

pub(super) const CALLBACK: &str = "http://localhost:1455/auth/callback";

pub(super) struct Browser {
    pub(super) url: String,
    pub(super) verifier: String,
    state: String,
    result: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    listener: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(handle) = self.listener.take() {
            handle.abort();
        }
    }
}

impl Browser {
    pub(super) async fn start() -> Result<Self> {
        let provider = crate::oauth::provider("codex").context("缺少 Codex 授权配置")?;
        let pkce = crate::oauth::gen_pkce();
        let state = Uuid::new_v4().to_string();
        let mut url = Url::parse(provider.authorize_url)?;
        url.query_pairs_mut()
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", provider.client_id.as_str()),
                ("redirect_uri", CALLBACK),
                ("scope", provider.scope),
                ("code_challenge", &pkce.challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
            ])
            .extend_pairs(provider.extra_authorize_params);
        let result = Arc::new(std::sync::Mutex::new(None));
        let listener = if let Ok(socket) = tokio::net::TcpListener::bind(("127.0.0.1", 1455)).await
        {
            let expected = state.clone();
            let output = result.clone();
            let handler = get(move |uri: axum::http::Uri| async move {
                let location = format!("{CALLBACK}?{}", uri.query().unwrap_or_default());
                match parse_callback(&location, &expected) {
                    Ok(value) => {
                        let accepted = value.is_ok();
                        output
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_insert(value);
                        if accepted {
                            callback_page(StatusCode::OK, "已收到授权回调", "请返回 ModelCat 查看连接保存结果。")
                        } else {
                            callback_page(StatusCode::OK, "授权未完成", "请返回 ModelCat 重新登录。")
                        }
                    }
                    Err(_) => callback_page(StatusCode::BAD_REQUEST, "授权链接已失效", "请从 ModelCat 当前的连接窗口重新打开授权页。"),
                }
            });
            Some(tokio::spawn(async move {
                let service = axum::Router::new().route("/auth/callback", handler);
                let _ = tokio::time::timeout(
                    Duration::from_secs(900),
                    axum::serve(socket, service).into_future(),
                )
                .await;
            }))
        } else {
            None
        };
        Ok(Self {
            url: url.into(),
            state,
            verifier: pkce.verifier,
            result,
            listener,
        })
    }
    pub(super) fn submit(&self, url: &str) -> Result<()> {
        let value = parse_callback(url, &self.state)?;
        self.result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(value);
        Ok(())
    }
    pub(super) fn take(&self) -> Result<Option<String>> {
        self.result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .transpose()
            .map_err(|e| anyhow!(e))
    }
}
use std::future::IntoFuture;

fn callback_page(status: StatusCode, title: &str, detail: &str) -> Response {
    let html = include_str!("../../web/oauth-callback.html")
        .replace("<!--logo-->", include_str!("../../web/modelcat.svg"))
        .replace("<!--title-->", title)
        .replace("<!--detail-->", detail);
    (status, [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'"),
    ], axum::response::Html(html)).into_response()
}

fn parse_callback(raw: &str, expected: &str) -> Result<Result<String, String>> {
    ensure!(raw.len() <= 12000, "回调地址过长");
    let url = Url::parse(raw.trim()).context("请粘贴浏览器的完整回调地址")?;
    ensure!(
        url.as_str().split('?').next() == Some(CALLBACK),
        "回调地址不匹配"
    );
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    ensure!(
        query("state").as_deref() == Some(expected),
        "回调不属于本次授权，请重新检查"
    );
    if query("error").is_some() {
        return Ok(Err("OpenAI 授权已取消或被拒绝".into()));
    }
    Ok(Ok(query("code")
        .filter(|c| !c.is_empty())
        .context("回调缺少授权码")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_checks_destination_state_and_outcome() {
        assert!(parse_callback(&format!("{CALLBACK}?state=old&code=code"), "current").is_err());
        assert!(parse_callback("https://example.com/?state=current&code=a", "current").is_err());
        assert!(parse_callback(
            &format!("{CALLBACK}?state=current&error=access_denied"),
            "current"
        )
        .unwrap()
        .is_err());
        assert_eq!(
            parse_callback(&format!("{CALLBACK}?state=current&code=ok"), "current")
                .unwrap()
                .unwrap(),
            "ok"
        );
    }
}
