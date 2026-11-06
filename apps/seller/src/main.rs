use anyhow::{ensure, Result};
use channel_node::{
    config::{Binding, Config},
    control, gateway,
    state::{self, App},
};
use clap::{Parser, Subcommand};
use std::{
    fs,
    io::IsTerminal,
    path::PathBuf,
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
    #[arg(long, default_value = "127.0.0.1:19700", global = true)]
    bind: String,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
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
    Start,
    Stop,
    Restart,
    Status,
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    Open,
    Url,
    /// Print the local model API key; it cannot access management endpoints.
    ApiKey,
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
    Quotes,
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
        Action::SetupClient {
            client,
            gateway,
            key_file,
            no_launch,
        } => {
            channel_node::onboarding::setup(client, gateway, key_file.as_deref(), *no_launch).await
        }
        Action::Launch => {
            if !healthy(&base).await {
                start(&cli.bind).await?;
            }
            open_ui(&base)
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
            let status = rpc(&base, "/api/status", None).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
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
            rpc(&base, "/api/bind", Some(serde_json::to_value(b)?)).await?;
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
            rpc(&base, "/api/config", Some(serde_json::to_value(cfg)?)).await?;
            println!("Configuration saved.");
            Ok(())
        }
        Action::Quotes => {
            println!(
                "{}",
                rpc(&base, "/api/quotes/submit", Some(serde_json::json!({}))).await?
            );
            Ok(())
        }
        Action::Probe { model } => {
            println!(
                "{}",
                rpc(
                    &base,
                    "/api/probe",
                    Some(serde_json::json!({"model":model}))
                )
                .await?
            );
            Ok(())
        }
        Action::Pause | Action::Resume => {
            rpc(
                &base,
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
    let http = reqwest::Client::new();
    let req = if let Some(body) = body {
        http.post(format!("{base}{path}")).json(&body)
    } else {
        http.get(format!("{base}{path}"))
    };
    let response = req
        .bearer_auth(read_token()?)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
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
    let gateway = tokio::spawn(gateway::supervise(app.clone()));
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
    axum::serve(listener, control::router(app))
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
            draining.drain().await;
        })
        .await?;
    gateway.abort();
    refresh.abort();
    let _ = fs::remove_file(PathBuf::from(state::data_dir()).join("node.pid"));
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
