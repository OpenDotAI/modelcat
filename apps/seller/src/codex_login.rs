use anyhow::{anyhow, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tauri::{Manager, State, Url, WebviewWindow};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{timeout, Instant},
};

const CHROME: &str = "/Applications/Google Chrome.app";
pub(crate) const PROFILE_DIRECTORY: &str = "codex-account";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct CodexLogin(Mutex<Option<Login>>);

struct Login {
    profile: PathBuf,
    server: Server,
    id: String,
    deadline: Instant,
}

struct Server {
    _child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl Server {
    async fn start(profile: &Path, proxy: Option<&Url>) -> Result<Self> {
        let binary = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .context("找不到 Codex CLI，请先安装支持 app-server 的官方 Codex CLI")?;
        let mut command = Command::new(binary);
        command
            .args([
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "app-server",
                "--stdio",
            ])
            .env("CODEX_HOME", profile)
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .env_remove("OPENAI_BASE_URL")
            .env_remove("CODEX_LOGIN_BASE_URL")
            .current_dir(profile)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(proxy) = proxy {
            for variable in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                command.env(variable, proxy.as_str());
            }
            command.env("NO_PROXY", "localhost,127.0.0.1,::1");
            command.env("no_proxy", "localhost,127.0.0.1,::1");
        }
        let mut child = command.spawn().context("无法启动官方 Codex 登录服务")?;
        let mut server = Self {
            input: child.stdin.take().context("缺少 Codex 输入")?,
            output: BufReader::new(child.stdout.take().context("缺少 Codex 输出")?).lines(),
            _child: child,
        };
        server.request(1, "initialize", json!({
            "clientInfo": {"name": "modelcat", "title": "ModelCat", "version": env!("CARGO_PKG_VERSION")}
        })).await?;
        server.send(json!({"method": "initialized"})).await?;
        Ok(server)
    }

    async fn send(&mut self, request: Value) -> Result<()> {
        let mut message = serde_json::to_vec(&request)?;
        message.push(b'\n');
        self.input.write_all(&message).await?;
        Ok(())
    }

    async fn message(&mut self) -> Result<Value> {
        let line = self
            .output
            .next_line()
            .await?
            .context("Codex 登录服务已退出")?;
        serde_json::from_str(&line).context("无法读取 Codex 登录服务的响应")
    }

    async fn request(&mut self, id: u32, method: &str, params: Value) -> Result<Value> {
        timeout(REQUEST_TIMEOUT, async {
            self.send(json!({"id": id, "method": method, "params": params}))
                .await?;
            loop {
                let message = self.message().await?;
                if message["id"].as_u64() == Some(u64::from(id)) {
                    return message
                        .get("result")
                        .cloned()
                        .context("Codex 登录服务拒绝了请求");
                }
            }
        })
        .await
        .context("Codex 登录服务响应超时")?
    }

    async fn account(&mut self) -> Result<Value> {
        let result = self
            .request(2, "account/read", json!({"refreshToken": false}))
            .await?;
        let status = if result["account"]["type"] == "chatgpt" {
            "signed_in"
        } else {
            "signed_out"
        };
        Ok(json!({"state": status}))
    }
}

fn proxy_url(address: &str) -> Result<Option<Url>> {
    if address.trim().is_empty() {
        return Ok(None);
    }
    let url = Url::parse(address.trim()).map_err(|_| anyhow!("请输入有效的 Mac HTTP 代理地址"))?;
    ensure!(
        url.scheme() == "http"
            && url.host().is_some()
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "请输入不含账号和密码的 Mac HTTP 代理地址，例如 http://127.0.0.1:7890"
    );
    Ok(Some(url))
}

fn browser_proxy(setting: &str) -> Result<Option<String>> {
    match setting {
        "auto" => return Ok(None),
        "off" => return Ok(Some("--no-proxy-server".into())),
        _ => {}
    }
    let proxy = Url::parse(setting).map_err(|_| anyhow!("工作区代理地址无效"))?;
    let scheme = match proxy.scheme() {
        "http" | "https" => proxy.scheme(),
        "socks5" | "socks5h" => "socks5",
        _ => anyhow::bail!("浏览器不支持此代理协议"),
    };
    ensure!(scheme != "socks5" || (proxy.username().is_empty() && proxy.password().is_none()),
        "Chrome 不支持带账号密码的 SOCKS5 代理，请在网络设置中选择 HTTP 代理或免密 SOCKS5 代理");
    let host = match proxy.host_str().context("工作区代理缺少主机地址")? {
        "host.lima.internal" | "host.docker.internal" => "127.0.0.1",
        host => host,
    };
    let port = proxy.port_or_known_default().unwrap_or(1080);
    Ok(Some(format!("--proxy-server={scheme}://{host}:{port}")))
}

#[derive(Default)]
pub(crate) struct AuthorizationWindows(Mutex<Option<AuthorizationWindow>>);

struct AuthorizationWindow {
    attempt: uuid::Uuid,
    child: std::process::Child,
    _profile_lock: std::fs::File,
    proxy: String,
}

impl AuthorizationWindow {
    fn launch(attempt: uuid::Uuid, url: &Url, proxy: String, directory: &Path) -> Result<Self> {
        let binary = Path::new(CHROME).join("Contents/MacOS/Google Chrome");
        ensure!(binary.is_file(), "使用工作区代理授权需要安装 Google Chrome");
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("无法建立授权浏览器目录"),
        }
        ensure!(directory.symlink_metadata()?.is_dir(), "授权浏览器目录无效");
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true)
            .open(directory.join("modelcat.lock"))?;
        lock.try_lock().context("另一个 ModelCat 正在使用授权窗口")?;
        // Chrome forwards launches to an existing instance without applying the new proxy.
        ensure!(!(directory.join("SingletonSocket").canonicalize()
            .is_ok_and(|socket| std::os::unix::net::UnixStream::connect(socket).is_ok())),
            "请先退出此前的 ModelCat 授权窗口，再重新授权");
        let mut command = std::process::Command::new(binary);
        command.arg(format!("--user-data-dir={}", directory.display()))
            .arg(&proxy)
            .args(["--window-size=520,700", "--no-first-run", "--no-default-browser-check",
                "--disable-background-mode", "--disable-quic", "--proxy-bypass-list=localhost;127.0.0.1;[::1]"])
            .arg("--disable-extensions");
        let child = command.current_dir(directory).arg(format!("--app={url}"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
            .context("无法打开独立授权窗口")?;
        Ok(Self { attempt, child, _profile_lock: lock, proxy })
    }

    fn focus(&mut self) -> Result<bool> {
        if self.child.try_wait()?.is_some() { return Ok(false); }
        #[cfg(target_os = "macos")]
        {
            extern "C" { fn modelcat_focus_authorization(pid: i32) -> bool; }
            // This PID belongs to the child whose exit status we just checked.
            return Ok(unsafe { modelcat_focus_authorization(self.child.id() as i32) });
        }
        #[cfg(not(target_os = "macos"))]
        Ok(true)
    }
}

impl Drop for AuthorizationWindow {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) { return; }
        // Give Chrome time to commit its cookie database before releasing the profile.
        #[cfg(target_os = "macos")]
        unsafe {
            extern "C" { fn modelcat_quit_authorization(pid: i32) -> bool; }
            modelcat_quit_authorization(self.child.id() as i32);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) { return; }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn authorization_proxy(setting: &str) -> Result<String> {
    let detected;
    let address = match setting {
        "auto" => {
            detected = asale_client_core::http::auto_proxy()
                .context("未检测到代理，请在 ModelCat 网络设置中填写代理地址后重试")?;
            detected.as_str()
        }
        "off" => anyhow::bail!("工作区代理已关闭，请在 ModelCat 网络设置中填写授权要使用的代理地址"),
        address => address,
    };
    browser_proxy(address)?.context("未找到授权代理")
}

#[tauri::command]
pub async fn close_workspace_authorization(
    window: WebviewWindow,
    windows: State<'_, AuthorizationWindows>,
    attempt: String,
) -> std::result::Result<(), String> {
    if window.label() != "main" { return Err("授权窗口无效".into()); }
    let mut active = windows.0.lock().await;
    if active.as_ref().is_some_and(|w| w.attempt.to_string() == attempt) { active.take(); }
    Ok(())
}

#[tauri::command]
pub async fn workspace_authorization(
    window: WebviewWindow,
    state: State<'_, std::sync::Arc<crate::vm::VmState>>,
    windows: State<'_, AuthorizationWindows>,
    attempt: String,
) -> std::result::Result<(), String> {
    async {
        ensure!(window.label() == "main", "授权窗口无效");
        let id = uuid::Uuid::parse_str(&attempt).context("授权请求编号无效")?;
        let mut active = windows.0.lock().await;
        let management = crate::vm::vm_open(state.clone()).await.map_err(anyhow::Error::msg)?
            .context("请先开启工作区")?;
        let base = Url::parse(&management)?;
        let launch: Value = reqwest::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(REQUEST_TIMEOUT).build()?
            .get(base.join(&format!("api/connections/codex/{id}/browser"))?)
            .bearer_auth(crate::vm::management_token(&base)?)
            .send().await.context("无法读取工作区授权设置")?
            .error_for_status().context("授权已结束或工作区不可用")?.json().await?;
        let proxy = authorization_proxy(launch["proxy"].as_str().context("工作区未返回代理设置")?)?;
        let url = authorization_url(&launch["url"])?;
        if let Some(w) = active.as_mut() {
            if w.attempt == id && w.proxy == proxy && w.focus()? { return Ok(()); }
        }
        let directory = window.path().app_local_data_dir()?
            .join("Authorization").join(state.authorization_profile_id()?);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
        active.take();
        *active = Some(AuthorizationWindow::launch(id, &url, proxy, &directory)?);
        Ok(())
    }.await.map_err(|e: anyhow::Error| format!("{e:#}"))
}

fn authorization_url(value: &Value) -> Result<Url> {
    let url = Url::parse(value.as_str().context("Codex 未返回授权地址")?)
        .context("Codex 返回了无效的授权地址")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "/oauth/authorize" | "/codex/device"),
        "Codex 返回了意外的授权地址"
    );
    Ok(url)
}

async fn open_authorization(value: &Value, profile: &Path, proxy_arg: Option<&str>, session: &str) -> Result<()> {
    let url = authorization_url(value)?;
    let mut command = Command::new("/usr/bin/open");
    if let Some(proxy_arg) = proxy_arg {
        ensure!(Path::new(CHROME).is_dir(), "按工作区网络设置打开授权页需要 Google Chrome");
        let browser_profile = profile.join("browser").join(session);
        command
            .args(["-n", "-a", CHROME, "--args", "--new-window"])
            .arg(format!("--user-data-dir={}", browser_profile.display()))
            .arg(proxy_arg)
            .args(["--proxy-bypass-list=localhost;127.0.0.1;[::1]", "--no-first-run",
                "--no-default-browser-check", "--disable-quic"]);
    }
    let status = timeout(
        REQUEST_TIMEOUT,
        command
            .arg(url.as_str())
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .context("打开 OpenAI 授权页超时")?
    .context("无法打开 OpenAI 授权页")?;
    ensure!(status.success(), "无法打开 OpenAI 授权页");

    Ok(())
}

impl Login {
    async fn status(&mut self) -> Result<Value> {
        ensure!(Instant::now() < self.deadline, "授权已超时，请重新登录");
        let completion = timeout(Duration::from_millis(100), async {
            loop {
                let notification = self.server.message().await?;
                if notification["method"] == "account/login/completed"
                    && notification["params"]["loginId"].as_str() == Some(self.id.as_str())
                {
                    return Ok::<_, anyhow::Error>(notification["params"]["success"] == true);
                }
            }
        })
        .await;
        match completion {
            Err(_) => Ok(json!({"state": "waiting"})),
            Ok(Ok(true)) => self.server.account().await,
            Ok(Ok(false)) => Err(anyhow!(
                "OpenAI 授权未完成，请重试。若浏览器显示网络错误，请检查登录代理。"
            )),
            Ok(Err(error)) => Err(error),
        }
    }
}

#[tauri::command]
pub async fn codex_account(
    window: WebviewWindow,
    state: State<'_, CodexLogin>,
    action: String,
    proxy: Option<String>,
    profile_id: Option<String>,
) -> std::result::Result<Value, String> {
    async {
        ensure!(window.label() == "main", "账号操作无效");
        let root = crate::vm::codex_profile(action == "start")?;
        let profile = stored_profile(&root, profile_id.as_deref().unwrap_or("default"))?;
        let mut pending = state.0.lock().await;
        ensure!(
            pending
                .as_ref()
                .is_none_or(|login| login.profile == profile),
            "另一个账号正在授权，请先完成或取消"
        );

        match action.as_str() {
            "start" => {
                ensure!(pending.is_none(), "已有登录正在进行");
                let proxy = proxy_url(proxy.as_deref().unwrap_or_default())?;
                let mut server = Server::start(&profile, proxy.as_ref()).await?;
                let response = server
                    .request(3, "account/login/start", json!({"type": "chatgpt"}))
                    .await?;
                let id = response["loginId"]
                    .as_str()
                    .context("Codex 未返回登录会话")?
                    .to_owned();
                let proxy_arg = browser_proxy(proxy.as_ref().map_or("auto", Url::as_str))?;
                open_authorization(&response["authUrl"], &profile, proxy_arg.as_deref(), &uuid::Uuid::new_v4().to_string()).await?;
                *pending = Some(Login {
                    profile,
                    server,
                    id,
                    deadline: Instant::now() + LOGIN_TIMEOUT,
                });
                Ok(json!({"state": "waiting"}))
            }
            "status" => {
                if let Some(login) = pending.as_mut() {
                    let status = login.status().await;
                    if !status
                        .as_ref()
                        .is_ok_and(|value| value["state"] == "waiting")
                    {
                        *pending = None;
                        if status
                            .as_ref()
                            .is_ok_and(|value| value["state"] == "signed_in")
                        {
                            let _ = window.set_focus();
                        }
                    }
                    return status;
                }
                if !profile.is_dir() {
                    return Ok(json!({"state": "signed_out"}));
                }
                Server::start(&profile, None).await?.account().await
            }
            "cancel" => {
                *pending = None;
                Ok(json!({"state": "signed_out"}))
            }
            "logout" => {
                *pending = None;
                if !profile.is_dir() {
                    return Ok(json!({"state": "signed_out"}));
                }
                let mut server = Server::start(&profile, None).await?;
                server.request(3, "account/logout", json!({})).await?;
                server.account().await
            }
            _ => Err(anyhow!("账号操作无效")),
        }
    }
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn codex_profiles(
    window: WebviewWindow,
    name: Option<String>,
) -> std::result::Result<Value, String> {
    (|| -> Result<Value> {
        ensure!(window.label() == "main", "账号操作无效");
        let root = crate::vm::codex_profile(name.is_some())?;
        let directory = root.join("profiles");
        let mut rows = vec![json!({"id":"default","label":"默认账号"})];
        let mut selected = None;
        if let Some(label) = name.as_deref().map(str::trim) {
            ensure!(
                !label.is_empty() && label.chars().count() <= 64,
                "账号名称须为 1 到 64 个字符"
            );
            if !directory.exists() {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .or_else(|error| {
                        if error.kind() == std::io::ErrorKind::AlreadyExists {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    })?;
            }
            ensure!(directory.symlink_metadata()?.is_dir(), "账号目录无效");
            let id = uuid::Uuid::new_v4().simple().to_string();
            let path = directory.join(&id);
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            std::fs::write(path.join("modelcat-label"), label)?;
            selected = Some(id);
        }
        if directory.exists() {
            ensure!(directory.symlink_metadata()?.is_dir(), "账号目录无效");
            let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let id = entry.file_name().to_string_lossy().into_owned();
                if !valid_profile_id(&id) || !entry.file_type()?.is_dir() {
                    continue;
                }
                if let Ok(label) = std::fs::read_to_string(entry.path().join("modelcat-label")) {
                    rows.push(json!({"id":id,"label":label}));
                }
            }
        }
        Ok(json!({"profiles":rows,"selected":selected}))
    })()
    .map_err(|error| error.to_string())
}

fn valid_profile_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn stored_profile(root: &Path, id: &str) -> Result<PathBuf> {
    if id == "default" {
        return Ok(root.to_path_buf());
    }
    ensure!(valid_profile_id(id), "账号编号无效");
    let directory = root.join("profiles");
    let path = directory.join(id);
    ensure!(
        directory.symlink_metadata()?.is_dir() && path.symlink_metadata()?.is_dir(),
        "账号目录无效"
    );
    Ok(path)
}

pub(crate) fn profile(workspace: &Path, create: bool) -> Result<PathBuf> {
    let path = workspace.join(PROFILE_DIRECTORY);
    if create {
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("无法建立 Codex 登录目录"),
        }
    }
    match path.symlink_metadata() {
        Ok(metadata) => ensure!(metadata.is_dir(), "Codex 登录目录必须是工作区内的独立目录"),
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("无法读取 Codex 登录目录"),
    }
    Ok(path)
}

#[cfg(test)]
mod browser_tests {
    use super::*;

    #[test]
    fn workspace_routes_for_browser() -> Result<()> {
        assert_eq!(browser_proxy("auto")?, None);
        assert_eq!(browser_proxy("off")?.as_deref(), Some("--no-proxy-server"));
        assert_eq!(browser_proxy("socks5h://host.lima.internal:7890")?.as_deref(),
            Some("--proxy-server=socks5://127.0.0.1:7890"));
        assert_eq!(browser_proxy("https://user:password@[::1]")?.as_deref(),
            Some("--proxy-server=https://[::1]:443"));
        assert!(browser_proxy("socks5://user:password@localhost:1080").is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "launches Google Chrome twice with a synthetic localhost cookie"]
    async fn authorization_cookie_survives_window_close() -> Result<()> {
        let directory = std::env::temp_dir().join(format!("modelcat-test-{}", uuid::Uuid::new_v4()));
        let server = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = server.local_addr()?.port();
        let result = async {
            for path in ["set", "check"] {
                let url = Url::parse(&format!("http://127.0.0.1:{port}/{path}"))?;
                let _window = AuthorizationWindow::launch(uuid::Uuid::new_v4(), &url,
                    "--proxy-server=http://127.0.0.1:1".into(), &directory)?;
                timeout(Duration::from_secs(20), async {
                    loop {
                        let (stream, _) = server.accept().await?;
                        let mut reader = BufReader::new(stream);
                        let mut request = String::new();
                        reader.read_line(&mut request).await?;
                        if !request.starts_with(&format!("GET /{path} ")) { continue; }
                        let mut has_cookie = false;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).await? == 0 || line == "\r\n" { break; }
                            has_cookie |= line.trim().eq_ignore_ascii_case("Cookie: modelcat_fixture=1");
                        }
                        if path == "check" { ensure!(has_cookie, "persistent cookie missing"); }
                        reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nSet-Cookie: modelcat_fixture=1; Max-Age=600; HttpOnly; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await?;
                        return Ok::<_, anyhow::Error>(());
                    }
                }).await??;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        std::fs::remove_dir_all(directory)?;
        result
    }

    #[tokio::test]
    #[ignore = "launches Google Chrome with an isolated test profile on macOS"]
    async fn chrome_uses_recording_proxy() -> Result<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let proxy_arg = browser_proxy(&format!("http://{}", listener.local_addr()?))?;
        let dir = std::env::temp_dir().join(format!("modelcat-test-{}", uuid::Uuid::new_v4()));
        let window = AuthorizationWindow::launch(uuid::Uuid::new_v4(),
            &Url::parse("https://auth.openai.com/oauth/authorize")?, proxy_arg.unwrap(), &dir)?;
        let result = async {
            timeout(Duration::from_secs(25), async {
                loop {
                    let (stream, _) = listener.accept().await?;
                    let mut line = String::new();
                    timeout(Duration::from_secs(2), BufReader::new(stream).read_line(&mut line)).await.ok();
                    if line.starts_with("CONNECT auth.openai.com:443 ") { return Ok::<_, anyhow::Error>(()); }
                }
            }).await??;
            let callback = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
            let callback_url = format!("http://localhost:{}/auth/callback", callback.local_addr()?.port());
            let status = Command::new("/usr/bin/open")
                .args(["-n", "-a", CHROME, "--args"])
                .arg(format!("--user-data-dir={}", dir.display()))
                .arg(callback_url).status().await?;
            ensure!(status.success(), "failed to open callback");
            timeout(Duration::from_secs(12), async {
                loop {
                    let (stream, _) = callback.accept().await?;
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    stream.read_line(&mut line).await?;
                    if line.starts_with("GET /auth/callback ") {
                        stream.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            }).await??;
            Ok::<_, anyhow::Error>(())
        }.await;
        drop(window);
        std::fs::remove_dir_all(dir)?;
        result
    }
}
