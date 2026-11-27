#[cfg(feature = "desktop")]
mod codex_login;
#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
mod vm;
#[cfg(unix)]
mod tool_api;

use anyhow::{Context, ensure, Result};
use channel_node::{
    config::{Binding, Config},
    control, gateway,
    state::{self, App},
};
use clap::{Parser, Subcommand};
use std::{
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "modelcat",
    version = option_env!("MODELCAT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
    about = "Model service provider node · CLI and WebUI for a private channel gateway"
)]
struct Cli {
    #[arg(long, default_value = if cfg!(feature = "desktop") { "127.0.0.1:19701" } else { "127.0.0.1:19700" }, global = true)]
    bind: String,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Check a platform key against the gateway model catalogue without making an inference request.
    VerifyPlatform {
        #[arg(long)]
        gateway: String,
        /// Read the key from standard input when this argument is omitted.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Create/update an isolated buyer gateway app; fetches models without inference.
    SetupClient {
        #[arg(long, value_parser=["codex","claude","dsh"])]
        client: String,
        #[arg(long)]
        gateway: String,
        #[arg(long)]
        key_file: Option<PathBuf>,
        #[arg(long)]
        no_launch: bool,
    },
    /// Start the local service and open its management UI (the default for ModelCat.app).
    Launch,
    Serve,
    #[cfg(unix)]
    #[command(hide = true)]
    ToolRelay,
    Start,
    Stop,
    Restart,
    /// Stop the Mac workspace VM after closing its window.
    #[cfg(feature = "desktop")]
    WorkspaceStop,
    /// Manage an isolated programming tool in the Mac workspace VM.
    #[cfg(feature = "desktop")]
    Tool {
        #[command(subcommand)]
        action: ToolAction,
    },
    Status,
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    Open,
    Url,
    /// Print the local model API key; it cannot access management endpoints.
    ApiKey,
    /// Discover and register local Ollama models in a running workspace.
    Upstream {
        #[command(subcommand)]
        action: UpstreamAction,
    },
    /// Check cloud provider access from the workspace without sending credentials.
    Network {
        #[command(subcommand)]
        action: NetworkAction,
    },
    Bind {
        #[arg(long)]
        file: PathBuf,
    },
    Import {
        provider: String,
    },
    Config {
        #[arg(long)]
        file: PathBuf,
    },
    /// List catalog candidates and reference prices.
    OfferList,
    /// Save a quote. An omitted price uses the catalog reference for that side.
    OfferSave { model: String, #[arg(long)] upstream: Option<String>,
        #[arg(long)] input: Option<String>, #[arg(long)] output: Option<String> },
    /// Submit saved quotes, optionally limiting submission to the named models.
    Quotes { #[arg(long = "model")] models: Vec<String> },
    /// Access a local management endpoint. Use --body FILE (or - for stdin) to POST JSON.
    Api { endpoint: String, #[arg(long)] body: Option<PathBuf> },
    /// Make a local model call with your own prompt.
    Trial { model: String, prompt: String, #[arg(long)] stream: bool },
    /// View the bound node's earnings and quote review history.
    Earnings,
    /// Read request history or the detail for one request.
    Requests {
        #[arg(conflicts_with_all=["since","before","source","status"])] id: Option<String>,
        #[arg(long)] since: Option<i64>, #[arg(long)] before: Option<i64>,
        #[arg(long)] source: Option<String>, #[arg(long,value_parser=["ok","failed","cancelled"])] status: Option<String>,
    },
    /// Manage model provider connections in the running workspace.
    Connections { #[command(subcommand)] action: ConnectionAction },
    /// Sign in to the Mac workspace using a platform key from stdin or a file.
    #[cfg(feature = "desktop")]
    Login { #[arg(long)] key_file: Option<PathBuf> },
    /// Manage the same Codex account profiles used by the Mac app.
    #[cfg(feature = "desktop")]
    Account { #[arg(long, default_value="default")] profile: String, #[command(subcommand)] action: codex_login::AccountAction },
    /// Manage the Mac virtual machine without opening a window.
    #[cfg(feature = "desktop")]
    Workspace { #[command(subcommand)] action: vm::WorkspaceAction },
    /// Make one small local model call; consumes the provider's own subscription.
    Probe {
        model: String,
    },
    Pause,
    Resume,
    Autostart {
        #[arg(value_parser=["enable","disable"])]
        action: String,
    },
    /// Explicitly configured API-key upstream, also used for local end-to-end tests.
    AddUpstream {
        #[arg(long)]
        name: String,
        #[arg(long)]
        base_url: String,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        allow_loopback_http: bool,
    },
}

#[derive(Subcommand)]
enum ConnectionAction {
    List,
    /// Authorize a Codex connection with a device code, or renew one by ID.
    Authorize { label: String, #[arg(long)] id: Option<String> },
    /// Read JSON containing label, base, protocol and optionally key. Use - for stdin.
    Save { file: PathBuf, #[arg(long)] id: Option<String>, #[arg(long)] check_only: bool },
    Check { id: String },
    Rename { id: String, label: String },
    Refresh { id: String },
    Remove { id: String },
}

impl ConnectionAction {
    fn request(&self) -> Result<(String, Option<serde_json::Value>)> {
        let base = "/api/connections";
        let (id, suffix, body) = match self {
            Self::List => return Ok((base.into(), None)),
            Self::Authorize { .. } => anyhow::bail!("authorization requires the device flow"),
            Self::Save { file, id, check_only } => {
                let input = read_json(file)?;
                (id.as_deref(), if *check_only { "?probe_only=true" } else { "" }, input)
            }
            Self::Check { id } => (Some(id.as_str()), "/check", serde_json::json!({})),
            Self::Rename { id, label } => (Some(id.as_str()), "/rename", serde_json::json!({"label":label})),
            Self::Refresh { id } => (Some(id.as_str()), "/refresh", serde_json::json!({})),
            Self::Remove { id } => (Some(id.as_str()), "/remove", serde_json::json!({})),
        };
        let path = if let Some(id) = id {
            ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_')), "invalid connection id");
            format!("{base}/{id}{suffix}")
        } else { format!("{base}{suffix}") };
        Ok((path, Some(body)))
    }
}

async fn authorize_connection(base: &str, directory: Option<&Path>, label: &str, id: Option<&str>) -> Result<()> {
    let start = serde_json::json!({"label": label, "id": id});
    let session = management_rpc(base, directory, "/api/connections/codex/start", Some(start)).await?;
    let attempt = uuid::Uuid::parse_str(session["id"].as_str().context("missing attempt id")?)?;
    let code = session["code"].as_str().context("missing device code")?;
    eprintln!("打开 https://auth.openai.com/codex/device 并输入设备码 {code}");
    let interval = Duration::from_secs(session["interval"].as_u64().unwrap_or(5).clamp(1, 60));
    let endpoint = format!("/api/connections/codex/{attempt}");
    let wait = async {
        loop {
            tokio::time::sleep(interval).await;
            let result = management_rpc(base, directory, &format!("{endpoint}/poll"), Some(serde_json::json!({}))).await?;
            if result["pending"] != true {
                println!("{}", result);
                return Ok(());
            }
        }
    };
    tokio::select! {
        result = wait => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            management_rpc(base, directory, &format!("{endpoint}/cancel"), Some(serde_json::json!({}))).await?;
            anyhow::bail!("授权已取消")
        }
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    if path == Path::new("-") { Ok(serde_json::from_reader(std::io::stdin().lock())?) }
    else { Ok(serde_json::from_reader(fs::File::open(path)?)?) }
}

#[derive(Subcommand)]
enum UpstreamAction {
    /// List the models reachable by the workspace at an Ollama origin.
    Discover { url: String },
    /// Register one Ollama model for local inference.
    Register { url: String, model: String },
}

#[derive(Subcommand)]
enum NetworkAction {
    /// Check the Claude API or the Claude OAuth server endpoints.
    Check {
        #[arg(value_parser = ["claude", "claude-oauth"])]
        provider: String,
    },
}

#[cfg(feature = "desktop")]
#[derive(Subcommand)]
enum ToolAction {
    /// Authorize a private working directory inside the workspace VM.
    Authorize { tool: String },
    /// Change internet access for a stopped tool.
    Network { tool: String, #[arg(value_parser=["online","offline"])] mode: String },
    Start { tool: String },
    Stop { tool: String },
    Restart { tool: String },
    Status { tool: String },
    Logs { tool: String },
    Autostart {
        tool: String,
        #[arg(value_parser = ["enable", "disable"])]
        state: String,
    },
    /// Run the CLI in its existing container.
    Run {
        tool: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(dir) = &cli.data_dir {
        std::env::set_var("CHANNEL_NODE_DATA", dir);
        std::env::set_var("MODELCAT_DATA", dir);
    }
    let addr: std::net::SocketAddr = cli.bind.parse()?;
    ensure!(
        addr.ip().is_loopback(),
        "control service must bind loopback; use an SSH tunnel for remote WebUI access"
    );
    let base = format!("http://{}", cli.bind);
    let action = cli.command.unwrap_or(Action::Launch);
    match &action {
        #[cfg(unix)]
        Action::ToolRelay => tool_api::relay().await,
        #[cfg(feature = "desktop")]
        Action::Tool { action } => vm::run_tool_command(cli.data_dir.as_deref(), action),
        #[cfg(feature = "desktop")]
        Action::WorkspaceStop => vm::stop_workspace_command(cli.data_dir.as_deref()).await,
        Action::VerifyPlatform { gateway, key_file } => {
            use std::io::Read;
            let mut key = String::new();
            match key_file {
                Some(path) => key = std::fs::read_to_string(path)?,
                None => {
                    std::io::stdin().take(1024).read_to_string(&mut key)?;
                }
            }
            channel_node::onboarding::verify_platform(gateway, &key).await
        }
        Action::SetupClient {
            client,
            gateway,
            key_file,
            no_launch,
        } => {
            channel_node::onboarding::setup(client, gateway, key_file.as_deref(), *no_launch).await
        }
        Action::Launch => {
            #[cfg(feature = "desktop")]
            {
                desktop::run()
            }
            #[cfg(not(feature = "desktop"))]
            {
                if !healthy(&base).await {
                    start(&cli.bind).await?;
                }
                open_ui(&base)
            }
        }
        Action::Serve => serve(&cli.bind).await,
        Action::Start => start(&cli.bind).await,
        Action::Restart => {
            let _ = rpc(&base, "/api/shutdown", Some(serde_json::json!({}))).await;
            let mut released = false;
            for _ in 0..300 {
                if !healthy(&base).await {
                    if let Ok(lock) = lock_profile() {
                        drop(lock);
                        released = true;
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            ensure!(
                released,
                "node is still draining or its profile is busy; retry later"
            );
            start(&cli.bind).await
        }
        Action::Stop => {
            println!(
                "{}",
                rpc(&base, "/api/shutdown", Some(serde_json::json!({}))).await?
            );
            Ok(())
        }
        Action::Status => {
            let status = management_rpc(&base, cli.data_dir.as_deref(), "/api/status", None).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Action::Upstream { action } => {
            let (path, body) = match action {
                UpstreamAction::Discover { url } => (
                    "/api/ollama/discover", serde_json::json!({"origin":url}),
                ),
                UpstreamAction::Register { url, model } => (
                    "/api/ollama/register", serde_json::json!({"origin":url,"model":model}),
                ),
            };
            #[cfg(feature = "desktop")]
            if let Some((origin, token)) = vm::workspace_connection(cli.data_dir.as_deref())? {
                println!("{}", rpc_authenticated(&origin, path, Some(body), &token).await?);
                return Ok(());
            }
            println!("{}", rpc(&base, path, Some(body)).await?);
            Ok(())
        }
        Action::Network { action: NetworkAction::Check { provider } } => {
            let path = if provider == "claude-oauth" { "/api/proxy/claude-oauth" } else { "/api/proxy/test" };
            let body = Some(serde_json::json!({}));
            #[cfg(feature = "desktop")]
            if let Some((origin, token)) = vm::workspace_connection(cli.data_dir.as_deref())? {
                println!("{}", rpc_authenticated(&origin, path, body, &token).await?);
                return Ok(());
            }
            println!("{}", rpc(&base, path, body).await?);
            Ok(())
        }
        Action::Logs { lines } => {
            let text = fs::read_to_string(PathBuf::from(state::data_dir()).join("node.log"))?;
            let rows: Vec<_> = text.lines().collect();
            println!("{}", rows[rows.len().saturating_sub(*lines)..].join("\n"));
            Ok(())
        }
        Action::Url | Action::Open => {
            let token = read_token()?;
            let url = format!("{base}/#token={token}");
            if matches!(action, Action::Open) {
                #[cfg(target_os = "macos")]
                let opener = "open";
                #[cfg(target_os = "linux")]
                let opener = "xdg-open";
                ensure!(
                    Command::new(opener).arg(&url).status()?.success(),
                    "could not open browser"
                );
            } else {
                println!("{url}");
            }
            Ok(())
        }
        Action::ApiKey => {
            #[cfg(feature = "desktop")]
            if let Some((origin, token)) = vm::workspace_connection(cli.data_dir.as_deref())? {
                let response = rpc_authenticated(&origin, "/api/local-key", None, &token).await?;
                let key = response["key"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("workspace did not return a local model API key"))?;
                println!("{key}");
                return Ok(());
            }
            println!(
                "{}",
                fs::read_to_string(PathBuf::from(state::data_dir()).join("local-api.token"))?
                    .trim()
            );
            Ok(())
        }
        Action::Bind { file } => {
            let b: Binding = serde_json::from_slice(&fs::read(file)?)?;
            b.validate()?;
            management_rpc(&base, cli.data_dir.as_deref(), "/api/bind", Some(serde_json::to_value(b)?)).await?;
            println!("Node bound. Remove or securely store the binding file.");
            Ok(())
        }
        Action::Import { provider } => {
            println!(
                "{}",
                rpc(
                    &base,
                    "/api/import",
                    Some(serde_json::json!({"provider":provider}))
                )
                .await?
            );
            Ok(())
        }
        Action::Config { file } => {
            let cfg: Config = serde_json::from_slice(&fs::read(file)?)?;
            cfg.validate()?;
            management_rpc(&base, cli.data_dir.as_deref(), "/api/config", Some(serde_json::to_value(cfg)?)).await?;
            println!("Configuration saved.");
            Ok(())
        }
        #[cfg(feature = "desktop")]
        Action::Account { profile, action } => codex_login::account_cli(cli.data_dir.as_deref(), action, profile).await,
        #[cfg(feature = "desktop")]
        Action::Workspace { action } => vm::workspace_cli(cli.data_dir.as_deref(), action).await,
        #[cfg(feature = "desktop")]
        Action::Login { key_file } => {
            use std::io::Read;
            let mut key = String::new();
            if let Some(path) = key_file { key = fs::read_to_string(path)?; }
            else { std::io::stdin().take(1024).read_to_string(&mut key)?; }
            vm::login_platform(&key, &vm::profile_root(cli.data_dir.as_deref())?).await?;
            println!("Platform key verified. Workspace login saved.");
            Ok(())
        }
        Action::Api { endpoint, body } => {
            ensure!(endpoint.starts_with("/api/"), "expected a local /api/ endpoint");
            let payload = body.as_deref().map(read_json).transpose()?;
            print_response(&base, cli.data_dir.as_deref(), endpoint, payload).await
        }
        Action::Trial { model, prompt, stream } => {
            let body = serde_json::json!({"model":model,"prompt":prompt,"stream":stream});
            print_response(&base, cli.data_dir.as_deref(), "/api/inference/test", Some(body)).await
        }
        Action::Earnings => print_response(&base,cli.data_dir.as_deref(),"/api/earnings",None).await,
        Action::Requests { id, since, before, source, status } => {
            let mut url = reqwest::Url::parse("http://localhost/api/request-history")?;
            if let Some(id) = id {
                url.path_segments_mut().unwrap().push(id);
            } else {
                let mut query = url.query_pairs_mut();
                if let Some(value) = since { query.append_pair("since",&value.to_string()); }
                if let Some(value) = before { query.append_pair("before",&value.to_string()); }
                if let Some(value) = source { query.append_pair("source",value); }
                if let Some(value) = status { query.append_pair("status",value); }
            }
            print_response(&base,cli.data_dir.as_deref(), &url.as_str()[url.origin().ascii_serialization().len()..],None).await
        }
        Action::Connections { action: ConnectionAction::Authorize { label, id } } => {
            authorize_connection(&base, cli.data_dir.as_deref(), label, id.as_deref()).await
        }
        Action::Connections { action } => {
            let (path, body) = action.request()?;
            print_response(&base,cli.data_dir.as_deref(), &path,body).await
        }
        Action::OfferList => {
            let snapshot = management_rpc(&base, cli.data_dir.as_deref(), "/api/status", None).await?;
            ensure!(snapshot["quote_candidates"].is_array(), "update the workspace node for quote catalog support");
            println!("{}", serde_json::json!({"models":snapshot["quote_candidates"],"references":snapshot["reference_prices"],"saved":snapshot["config"]["offers"]}));
            Ok(())
        }
        Action::OfferSave { model, upstream, input, output } => {
            let edit = serde_json::json!([{"model":model,"upstream":upstream,"input":input,"output":output}]);
            print_response(&base, cli.data_dir.as_deref(), "/api/quotes/save", Some(edit)).await
        }
        Action::Quotes { models } => {
            let selection = if models.is_empty() { serde_json::json!({}) } else { serde_json::json!({"models":models}) };
            println!(
                "{}",
                management_rpc(&base, cli.data_dir.as_deref(), "/api/quotes/submit", Some(selection)).await?
            );
            Ok(())
        }
        Action::Probe { model } => {
            println!(
                "{}",
                management_rpc(
                    &base,
                    cli.data_dir.as_deref(),
                    "/api/probe",
                    Some(serde_json::json!({"model":model}))
                )
                .await?
            );
            Ok(())
        }
        Action::Pause | Action::Resume => {
            management_rpc(
                &base,
                cli.data_dir.as_deref(),
                "/api/pause",
                Some(serde_json::json!({"paused":matches!(action,Action::Pause)})),
            )
            .await?;
            println!("State updated; running requests are allowed to drain.");
            Ok(())
        }
        Action::Autostart { action } => autostart(&cli.bind, action == "enable"),
        Action::AddUpstream {
            name,
            base_url,
            key_file,
            allow_loopback_http,
        } => {
            let _profile_lock = lock_profile()?;
            ensure!(
                !healthy(&base).await,
                "stop the node before adding a direct upstream"
            );
            channel_node::accounts::validate_custom_base(base_url, *allow_loopback_http)?;
            let key = fs::read_to_string(key_file)?.trim().to_string();
            ensure!(!key.is_empty(), "empty upstream key");
            let app = App::open().await?;
            app.persist_credential(
                asale_client_core::cli_import::CliCred {
                    provider: "custom".into(),
                    access_token: key,
                    refresh_token: None,
                    expires_at: None,
                    account_hint: Some(name.clone()),
                    plan: None,
                    upstream_account_id: None,
                },
                "api_key",
                Some(base_url.clone()),
            )
            .await?;
            println!("Upstream added; configure a custom model offer before starting.");
            Ok(())
        }
    }
}

async fn healthy(base: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/healthz"))
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .ok()
        .is_some_and(|r| r.status().is_success())
}
fn read_token() -> Result<String> {
    Ok(
        fs::read_to_string(PathBuf::from(state::data_dir()).join("control.token"))?
            .trim()
            .to_string(),
    )
}
async fn rpc(base: &str, path: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value> {
    rpc_authenticated(base, path, body, &read_token()?).await
}

async fn management_rpc(
    base: &str,
    _directory: Option<&Path>,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let (origin, token) = management_target(base, _directory)?;
    rpc_authenticated(&origin, path, body, &token).await
}

fn management_target(base: &str, _directory: Option<&Path>) -> Result<(String, String)> {
    #[cfg(feature = "desktop")]
    if base == "http://127.0.0.1:19701" {
        if let Some(connection) = vm::workspace_connection(_directory)? {
            return Ok(connection);
        }
    }
    Ok((base.to_owned(), read_token()?))
}

async fn print_response(base: &str, directory: Option<&Path>, endpoint: &str, payload: Option<serde_json::Value>) -> Result<()> {
    let (origin, token) = management_target(base, directory)?;
    let mut response = management_request(&origin, endpoint, payload, &token).await?;
    if !response.status().is_success() {
        let status = response.status();
        let details = response.json::<serde_json::Value>().await.unwrap_or_default();
        anyhow::bail!("{status} {}", details["error"].as_str().unwrap_or("request failed"));
    }
    use std::io::Write;
    while let Some(chunk) = response.chunk().await? {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&chunk)?;
        stdout.flush()?;
    }
    println!();
    Ok(())
}

async fn management_request(base: &str, path: &str, body: Option<serde_json::Value>, token: &str) -> Result<reqwest::Response> {
    let http = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build()?;
    let req = if let Some(body) = body {
        http.post(format!("{base}{path}")).json(&body)
    } else {
        http.get(format!("{base}{path}"))
    };
    Ok(req
        .bearer_auth(token)
        .timeout(Duration::from_secs(if path == "/api/inference/test" { 600 } else { 30 }))
        .send()
        .await?)
}

async fn rpc_authenticated(base: &str, path: &str, body: Option<serde_json::Value>, token: &str) -> Result<serde_json::Value> {
    let response = management_request(base, path, body, token).await?;
    let status = response.status();
    let value: serde_json::Value = response.json().await?;
    ensure!(
        status.is_success(),
        "{}",
        value["error"].as_str().unwrap_or("node request failed")
    );
    Ok(value)
}
async fn start(bind: &str) -> Result<()> {
    let base = format!("http://{bind}");
    ensure!(
        !healthy(&base).await,
        "a service is already listening on {bind}"
    );
    let dir = PathBuf::from(state::data_dir());
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("node.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--bind", bind, "serve"])
        .env("CHANNEL_NODE_DATA", &dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    for _ in 0..150 {
        if healthy(&base).await {
            println!("Node started on {base}. Use `modelcat open` for WebUI.");
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("node exited {status}; inspect modelcat logs");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("node did not become ready; inspect logs (process may still be starting)")
}
#[cfg(not(feature = "desktop"))]
fn open_ui(base: &str) -> Result<()> {
    let token = read_token()?;
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "linux")]
    let opener = "xdg-open";
    ensure!(
        Command::new(opener)
            .arg(format!("{base}/#token={token}"))
            .status()?
            .success(),
        "could not open browser"
    );
    Ok(())
}

async fn serve(bind: &str) -> Result<()> {
    let _profile_lock = lock_profile()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,channel_node=info".into()),
        )
        .init();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let app = App::open().await?;
    #[cfg(unix)]
    let (stop_tools, tool_shutdown) = tokio::sync::watch::channel(false);
    #[cfg(unix)]
    let tools = std::env::var_os("MODELCAT_TOOL_API_DIR")
        .map(|dir| tool_api::serve(
            &PathBuf::from(dir), &app.api_token, control::local_router(app.clone()), tool_shutdown,
        ))
        .transpose()?;
    channel_node::traffic::checkpoint(&app, true).await?;
    let (stop_traffic, traffic_shutdown) = tokio::sync::oneshot::channel();
    let traffic = tokio::spawn(channel_node::traffic::collect(app.clone(), traffic_shutdown));
    let gateway = tokio::spawn(gateway::supervise(app.clone()));
    let prices = tokio::spawn(channel_node::pricing::maintain(app.clone()));
    let refresh_app = app.clone();
    let refresh = tokio::spawn(async move {
        loop {
            refresh_app.refresh_accounts().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    if std::io::stdout().is_terminal() {
        println!("MSP WebUI: http://{bind}/#token={}", app.token);
    }
    fs::write(
        PathBuf::from(state::data_dir()).join("node.pid"),
        std::process::id().to_string(),
    )?;
    let draining = app.clone();
    let result = axum::serve(listener, control::router(app))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{},_=draining.shutdown_requested.notified()=>{}}
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            #[cfg(unix)]
            let _ = stop_tools.send(true);
            draining.drain().await;
        })
        .await;
    prices.abort();
    gateway.abort();
    refresh.abort();
    let _ = gateway.await;
    let _ = stop_traffic.send(());
    #[cfg(unix)]
    if let Some(tools) = tools {
        let _ = tools.await;
    }
    let _ = fs::remove_file(PathBuf::from(state::data_dir()).join("node.pid"));
    traffic.await??;
    result?;
    Ok(())
}

fn lock_profile() -> Result<fs::File> {
    let dir = PathBuf::from(state::data_dir());
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("node.lock"))?;
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("another node process is using this data directory"))?;
    Ok(file)
}
#[cfg(target_os = "macos")]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn autostart(bind: &str, enable: bool) -> Result<()> {
    let executable = std::env::current_exe()?;
    let data = PathBuf::from(state::data_dir());
    let home = state::home_dir().ok_or_else(|| anyhow::anyhow!("home unavailable"))?;
    #[cfg(target_os = "macos")]
    {
        let path = home.join("Library/LaunchAgents/ai.channel-node.plist");
        if !enable {
            let _ = Command::new("launchctl").arg("unload").arg(&path).status();
            if path.exists() {
                fs::remove_file(path)?;
            }
            return Ok(());
        }
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path,format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>ai.channel-node</string><key>ProgramArguments</key><array><string>{}</string><string>--bind</string><string>{}</string><string>--data-dir</string><string>{}</string><string>serve</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",xml(&executable.display().to_string()),xml(bind),xml(&data.display().to_string()),xml(&data.join("node.log").display().to_string()),xml(&data.join("node.log").display().to_string())))?;
        ensure!(
            Command::new("launchctl")
                .arg("load")
                .arg(path)
                .status()?
                .success(),
            "launchctl failed"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let path = home.join(".config/systemd/user/channel-node.service");
        if enable {
            fs::create_dir_all(path.parent().unwrap())?;
            let escape = |s: &str| {
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('%', "%%")
            };
            fs::write(&path,format!("[Unit]\nDescription=Model service provider node\nAfter=network-online.target\n[Service]\nExecStart=\"{}\" --bind \"{}\" --data-dir \"{}\" serve\nRestart=on-failure\n[Install]\nWantedBy=default.target\n",escape(&executable.display().to_string()),escape(bind),escape(&data.display().to_string())))?;
            ensure!(
                Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .status()?
                    .success(),
                "systemd user session unavailable"
            );
            ensure!(
                Command::new("systemctl")
                    .args(["--user", "enable", "--now", "channel-node"])
                    .status()?
                    .success(),
                "could not enable service"
            );
        } else {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", "channel-node"])
                .status();
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
    }
    println!("Autostart {}.", if enable { "enabled" } else { "disabled" });
    Ok(())
}
