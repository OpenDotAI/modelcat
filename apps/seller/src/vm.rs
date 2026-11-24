use anyhow::{ensure, Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{Emitter, Manager};

const CONFIG: &str = include_str!("../vm.yaml");
const CONFIG_OVERRIDE: &str = include_str!("../vm-override.yaml");
const START_NODE: &str = include_str!("../vm-start-node.sh");
const TOOL: &str = include_str!("../vm-tool.sh");
const PREPARE_TOOL: &str = include_str!("../../../scripts/prepare-vm-tools.sh");
const PLATFORM_GATEWAY: &str = "https://token.opendot.pro";
const WORKSPACE_MARKER: &str = "modelcat-vm.marker";
const PLATFORM_VERIFIED: &str = "platform-verified";
const WORKSPACE_DIRECTORY: &str = "workspace-directory.json";
const DATA_DIRECTORY: &str = "ModelCatVMData";
const KEEP_WORKSPACE_RUNNING: &str = "keep-workspace-running";
const RESOURCE_FILE: &str = "vm-resources.json";

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct VmResources {
    cpus: u32,
    memory_gib: u32,
    disk_gib: u32,
}

fn saved_resources(root: &Path) -> Result<Option<VmResources>> {
    match std::fs::read(root.join(RESOURCE_FILE)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[tauri::command]
pub async fn vm_resources(window: tauri::WebviewWindow, state: tauri::State<'_, Arc<VmState>>,
    limits: Option<VmResources>) -> std::result::Result<serde_json::Value, String> {
    if window.label() != "main" { return Err("请在工具管理中设置虚拟机资源".into()); }
    let operation = state.lifecycle.clone().lock_owned().await;
    tauri::async_runtime::spawn_blocking(move || -> Result<serde_json::Value> {
        let _operation = operation;
        let root = workspace_root(None)?;
        let current = if root.join("lima/modelcat/lima.yaml").is_file() {
            let driver = Driver { root: root.clone(), runtime: runtime()? };
            let output = driver.call(&["list", "--json", "modelcat"])?;
            #[derive(serde::Deserialize)]
            struct Instance { cpus: u32, memory: u64, disk: u64 }
            let instance: Instance = serde_json::from_slice(&output.stdout)?;
            VmResources { cpus: instance.cpus, memory_gib: instance.memory.div_ceil(1<<30) as u32,
                disk_gib: instance.disk.div_ceil(1<<30) as u32 }
        } else { VmResources { cpus: 2, memory_gib: 2, disk_gib: 8 } };
        let file = root.join("lima/modelcat/disk");
        let min_disk = if file.exists() { std::fs::metadata(file)?.len().div_ceil(1<<30).max(1) } else { 8 };
        let max_cpu = std::thread::available_parallelism()?.get() as u32;
        let mem = Command::new("/usr/sbin/sysctl").args(["-n","hw.memsize"]).output()?;
        let max_memory = std::str::from_utf8(&mem.stdout)?.trim().parse::<u64>()? >> 30;
        let configured = if let Some(value) = limits {
            ensure!((1..=max_cpu).contains(&value.cpus), "CPU 超出本机范围");
            ensure!((1..=max_memory).contains(&u64::from(value.memory_gib)), "内存超出本机范围");
            ensure!(u64::from(value.disk_gib) >= min_disk, "虚拟磁盘不能小于现有容量（{min_disk} GiB）");
            prepare_root(&root)?;
            let pending = root.join(format!(".resources-{}", uuid::Uuid::new_v4()));
            std::fs::write(&pending, serde_json::to_vec(&value)?)?;
            std::fs::rename(pending, root.join(RESOURCE_FILE))?;
            value
        } else { saved_resources(&root)?.unwrap_or(current) };
        Ok(serde_json::json!({"limits":configured,"current":current,"min_disk":min_disk,
            "max_cpu":max_cpu,"max_memory":max_memory}))
    }).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn vm_usage(window: tauri::WebviewWindow, seconds: u32) -> std::result::Result<serde_json::Value, String> {
    if window.label() != "main" { return Err("请在工具管理中查看用量".into()); }
    let result: Result<_> = async {
        let driver = Driver { root: workspace_root(None)?, runtime: runtime()? };
        ensure!([0, 300, 3600, 86400].contains(&seconds), "无效的监控时段");
        let mut cmd = tokio::process::Command::from(driver.command());
        cmd.args(["shell", "modelcat", "--", "python3", "-c", include_str!("../vm-monitor.py"), &seconds.to_string()]).kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await??;
        ensure!(output.status.success(), "虚拟机采样失败");
        Ok(serde_json::from_slice(&output.stdout)?)
    }.await;
    result.map_err(|e| format!("{e:#}"))
}

#[derive(Default)]
pub struct VmState {
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    exit_requested: AtomicBool,
    workspace: Mutex<Option<Workspace>>,
}

struct Workspace {
    driver: Driver,
    _lock: std::fs::File,
    management_url: Option<tauri::Url>,
}
#[derive(Clone)]
struct Driver {
    root: PathBuf,
    runtime: PathBuf,
}

#[derive(Default, serde::Serialize)]
struct RuntimeHealth {
    vm_running: Option<bool>,
    docker_ready: Option<bool>,
    agent_running: Option<bool>,
    node_running: Option<bool>,
    issue: Option<&'static str>,
}

impl VmState {
    pub(crate) fn authorization_profile_id(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let workspace = self.workspace.lock().unwrap();
        let root = &workspace.as_ref().context("请先开启工作区")?.driver.root;
        Ok(format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes())))
    }

    fn stop_on_exit(&self, on_stopping: impl FnOnce()) -> Result<()> {
        if self.workspace.lock().unwrap().is_none() {
            return Ok(());
        }
        let enabled = read_stop_on_exit().unwrap_or_else(|error| {
            eprintln!("无法读取关闭偏好，按默认设置停止工作区：{error:#}");
            true
        });
        if enabled {
            on_stopping();
            self.stop(false)?;
        }
        Ok(())
    }

    fn stop(&self, include_saved_workspace: bool) -> Result<()> {
        let mut workspace = self.workspace.lock().unwrap();
        if workspace.is_none() && !include_saved_workspace {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        if let Some(workspace) = workspace.as_ref() {
            runtime.block_on(workspace.driver.shutdown())?;
        } else if include_saved_workspace {
            let root = workspace_root(None)?;
            if root.join(WORKSPACE_MARKER).is_file() {
                runtime.block_on(stop_saved_workspace(root))?;
            }
        }
        *workspace = None;
        Ok(())
    }
}
impl Driver {
    fn command(&self) -> Command {
        let mut command = Command::new(self.runtime.join("lima/bin/limactl"));
        command
            // Finder-launched Lima resolves its current directory before reading LIMA_HOME.
            .current_dir("/")
            .env("HOME", self.root.join("host-home"))
            .env("LIMA_HOME", self.root.join("lima"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("TMPDIR", self.root.join("tmp"))
            .env_remove("SSH_AUTH_SOCK")
            .env_remove("LIMA_AUTOSTART_IDENTIFIER");
        command
    }
    fn call(&self, args: &[&str]) -> Result<Output> {
        let output = self.command().args(args).output().context("无法运行内置虚拟机管理程序")?;
        self.check_output(args[0], output)
    }
    fn check_output(&self, action: &str, output: Output) -> Result<Output> {
        if !output.status.success() {
            std::fs::write(self.root.join("vm-error.log"), &output.stderr)?;
            anyhow::bail!(
                "虚拟机操作 {} 失败：{}。详细信息：{}",
                action,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(1200)
                    .collect::<String>(),
                self.root.join("vm-error.log").display()
            );
        }
        Ok(output)
    }
    fn start_monitor(&self) {
        let args = ["shell", "modelcat", "--", "sh", "-c", include_str!("../vm-usage.sh"), "modelcat-monitor", include_str!("../vm-monitor.py")];
        if let Err(error) = self.call(&args) {
            eprintln!("无法启动资源监控：{error:#}");
        }
    }
    fn start_node(&self, stage: &str) -> Result<()> {
        // Loopback in the node is the VM. Resolve the Mac endpoint inside Lima.
        let mut proxy = asale_client_core::http::auto_proxy().unwrap_or_default();
        if let Ok(mut url) = reqwest::Url::parse(&proxy) {
            let local = match url.host_str() {
                Some(host) => host.eq_ignore_ascii_case("localhost") || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()),
                None => false,
            };
            if local {
                let hosts = self.call(&["shell", "modelcat", "--", "getent", "ahostsv4", "host.lima.internal"])?;
                let hosts = std::str::from_utf8(&hosts.stdout)?;
                let ip: std::net::Ipv4Addr = hosts.split_whitespace().next()
                    .context("无法解析工作区的 Mac 代理地址")?.parse()?;
                url.set_ip_host(ip.into()).map_err(|_| anyhow::anyhow!("无法设置工作区代理"))?;
                proxy = url.into();
            }
        }
        let mut node = self.command()
            .args(["shell", "modelcat", "--", "sh", "-c", START_NODE, "modelcat-start-node", stage])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        // A proxy URL may contain credentials. Keep it out of command arguments and logs.
        let write = writeln!(node.stdin.take().context("无法传递代理设置")?, "{proxy}");
        self.check_output("shell", node.wait_with_output()?)?;
        write?;
        Ok(())
    }
    fn start(&self, was_running: bool, report: impl Fn(&str)) -> Result<tauri::Url> {
        let has_disk = self.root.join("lima/modelcat/disk").is_file();
        report(if has_disk {
            "正在启动虚拟机…"
        } else {
            "正在下载首次启动所需的工作区镜像…"
        });
        let file = self.root.join("modelcat.yaml");
        let config = CONFIG.replace("hostPort: 19701", &format!("hostPort: {}", workspace_host_port()));
        std::fs::write(&file, config)?;
        // Apply the bootstrap network policy to existing workspaces on restart as well.
        let config_dir = self.root.join("lima/_config");
        std::fs::create_dir_all(&config_dir)?;
        let mut overrides = CONFIG_OVERRIDE.to_owned();
        if let Some(limits) = saved_resources(&self.root)? {
            if !was_running {
                overrides.push_str(&format!("\ncpus: {}\nmemory: {}GiB\ndisk: {}GiB\n",
                    limits.cpus, limits.memory_gib, limits.disk_gib));
            } else if let Ok(previous) = std::fs::read_to_string(config_dir.join("override.yaml")) {
                for line in previous.lines().filter(|l| ["cpus:", "memory:", "disk:"].iter().any(|k| l.starts_with(k))) {
                    overrides.push_str(&format!("\n{line}\n"));
                }
            }
        }
        std::fs::write(config_dir.join("override.yaml"), overrides)?;
        let exists = self.root.join("lima/modelcat/lima.yaml").exists();
        let timeout = if has_disk { "--timeout=3m" } else { "--timeout=15m" };
        if exists {
            self.call(&["start", "--tty=false", timeout, "modelcat"])?;
        } else {
            self.call(&[
                "start",
                "--tty=false",
                timeout,
                "--name=modelcat",
                file.to_str().context("目录名称无法编码")?,
            ])?;
        }
        report("正在载入模型节点…");
        let stage = self.call(&[
            "shell", "modelcat", "--", "mktemp", "-d", "/tmp/modelcat.XXXXXXXXXX",
        ])?;
        let stage = String::from_utf8(stage.stdout)?;
        let stage = stage.trim();
        ensure!(
            stage.strip_prefix("/tmp/modelcat.").is_some_and(|suffix| {
                suffix.len() == 10 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            }),
            "虚拟机返回了异常临时目录"
        );
        let binary = if cfg!(debug_assertions) && std::env::var_os("MODELCAT_OAUTH_PREVIEW").is_some() {
            self.runtime.join("oauth-preview/modelcat")
        } else {
            self.runtime.join("guest/modelcat")
        };
        self.call(&[
            "copy",
            binary.to_str().context("程序路径无法编码")?,
            &format!("modelcat:{stage}/modelcat"),
        ])?;
        report("正在准备 Docker 和模型节点…");
        self.start_node(stage)?;
        self.start_monitor();
        if !was_running {
            report("正在检查自启动工具…");
            self.autostart_tools();
        }
        report("正在验证本地连接…");
        let url = self.management_url()?;
        let token = management_token(&url)?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let http = reqwest::Client::builder().no_proxy().build()?;
            let health = url.join("api/status")?;
            for _ in 0..60 {
                if http
                    .get(health.clone())
                    .bearer_auth(token)
                    .timeout(std::time::Duration::from_secs(1))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            anyhow::bail!("虚拟机已启动，但管理接口尚未连接。请检查 {port} 端口是否被占用", port = workspace_host_port())
        })?;
        Ok(url)
    }
    fn management_url(&self) -> Result<tauri::Url> {
        let output = self.call(&[
            "shell", "modelcat", "--", "sudo", "-n", "docker", "exec", "modelcat-node", "modelcat",
            "url",
        ])?;
        let guest: tauri::Url = String::from_utf8(output.stdout)?.trim().parse()?;
        ensure!(
            guest.scheme() == "http" && guest.host_str() == Some("127.0.0.1")
                && guest.port() == Some(19700)
                && guest.username().is_empty() && guest.password().is_none()
                && guest.path() == "/" && guest.query().is_none(),
            "虚拟机返回了异常管理地址"
        );
        let mut url = guest;
        let port = workspace_host_port();
        url.set_port(Some(port))
            .map_err(|_| anyhow::anyhow!("无法配置本地连接端口"))?;
        Ok(url)
    }
    fn open_workspace(&self, default: &Path, was_running: bool, report: impl Fn(&str)) -> Result<tauri::Url> {
        let url = self.start(was_running, report)?;
        remember_workspace_root(default, &self.root)?;
        Ok(url)
    }
    fn tool(&self, action: &str, tool: &str, extra: &[&str]) -> Result<String> {
        ensure!(channel_node::tool_config::TOOLS.contains(&tool), "unknown tool {tool}");
        let mut args = vec!["shell", "modelcat", "--", "sh", "-c", TOOL, "modelcat-tool", action, tool];
        args.extend(extra);
        Ok(String::from_utf8(self.call(&args)?.stdout)?)
    }
    fn launch_tool(&self, tool: &str, action: &str) -> Result<()> {
        self.launch_tool_progress(tool, action, |_| {})
    }
    fn launch_tool_progress(&self, tool: &str, action: &str, report: impl Fn(&str)) -> Result<()> {
        report("正在检查工作目录和可用资源");
        let stage = self.tool("stage", tool, &[])?;
        let stage = stage.trim();
        if stage.is_empty() {
            report("正在启动容器并检查工具版本");
            self.tool(action, tool, &[])?;
            return Ok(());
        }
        ensure!(
            stage.strip_prefix("/tmp/modelcat-tool.").is_some_and(|suffix| {
                suffix.len() == 10 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            }),
            "invalid tool staging directory"
        );
        let result = (|| {
            report("正在准备安装文件（首次使用需要下载）");
            let payload = self.tool_assets(tool)?;
            report("正在将安装文件传入虚拟机");
            let program_kib = payload.join(tool).metadata()?.len().div_ceil(1024);
            let base_file = payload.join("ubuntu-base.tar.gz");
            // Our pinned Ubuntu base is a single gzip member below 4 GiB.
            use std::io::{Read, Seek, SeekFrom};
            let mut archive = std::fs::File::open(&base_file)?;
            archive.seek(SeekFrom::End(-4))?;
            let mut length = [0u8; 4];
            archive.read_exact(&mut length)?;
            let base_kib = (archive.metadata()?.len() + u64::from(u32::from_le_bytes(length))).div_ceil(1024);
            let base_needed = self.tool("install-space", tool, &[&program_kib.to_string(), &base_kib.to_string()])?;
            let mut files = vec![(tool, "program")];
            if base_needed.trim() == "base" { files.push(("ubuntu-base.tar.gz", "ubuntu-base.tar.gz")); }
            for (source, name) in files {
                let source = payload.join(source);
                self.call(&["copy", source.to_str().context("invalid tool path")?, &format!("modelcat:{stage}/{name}")])?;
            }
            report("正在安装并检查工具版本");
            self.tool(action, tool, &[stage])?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.call(&["shell", "modelcat", "--", "rm", "-rf", "--", stage]);
        }
        result
    }
    fn tool_assets(&self, tool: &str) -> Result<PathBuf> {
        let prepared = self.runtime.join("tools");
        if prepared.join(tool).is_file() && prepared.join("ubuntu-base.tar.gz").is_file() {
            return Ok(prepared);
        }
        let directory = self.root.join("tool-assets");
        let output = Command::new("/bin/sh")
            .args(["-c", PREPARE_TOOL, "modelcat-prepare-vm-tools", tool])
            .env("MODELCAT_TOOL_DATA", &directory)
            .env("HOME", self.root.join("host-home"))
            .env("TMPDIR", self.root.join("tmp"))
            .output()
            .context("无法准备工具运行文件")?;
        ensure!(
            output.status.success(),
            "准备 {tool} 运行文件失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(directory)
    }
    fn autostart_tools(&self) {
        for tool in channel_node::tool_config::TOOLS {
            let result = (|| -> Result<()> {
                let status = serde_json::from_str::<serde_json::Value>(&self.tool("status", tool, &[])?)?;
                if status["autostart"] == true {
                    self.launch_tool(tool, "start")?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Could not start {tool}: {error}");
            }
        }
    }
    fn is_running(&self) -> Result<bool> {
        let output = self.call(&["list", "--format", "{{.Status}}", "modelcat"])?;
        Ok(String::from_utf8(output.stdout)?.trim() == "Running")
    }

    async fn bounded_call(&self, args: &[&str], seconds: u64, timeout_message: &str) -> Result<Output> {
        let mut command = tokio::process::Command::from(self.command());
        command.args(args).kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(seconds), command.output())
            .await
            .map_err(|_| anyhow::anyhow!("{timeout_message}"))?
            .context("无法运行内置虚拟机管理程序")?;
        self.check_output(args[0], output)
    }

    async fn probe(&self, args: &[&str]) -> Result<Output> {
        self.bounded_call(args, 12, "内置 Lima 状态检查超时，请重试").await
    }

    async fn instance_status(&self) -> Result<String> {
        let format = r#"{{if eq .Name "modelcat"}}{{.Status}}{{end}}"#;
        let output = self.probe(&["list", "--format", format]).await?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    async fn inspect_running(&self) -> Result<bool> {
        Ok(self.instance_status().await? == "Running")
    }

    async fn inspect_health(&self) -> RuntimeHealth {
        let mut health = RuntimeHealth::default();
        match self.inspect_running().await {
            Ok(false) => {
                health.vm_running = Some(false);
                health.agent_running = Some(false);
                health.docker_ready = Some(false);
                health.node_running = Some(false);
                return health;
            }
            Ok(true) => health.vm_running = Some(true),
            Err(_) => {
                health.issue = Some("无法读取 Lima 状态，请检查工作区目录访问权限");
                return health;
            }
        }

        let check = r#"
if ! sudo -n docker info --format '{{.ServerVersion}}' >/dev/null 2>&1; then
    printf '%s\n' docker-unavailable
    exit 0
fi
sudo -n docker ps --format '{{.Names}}'
"#;
        match self.probe(&["shell", "modelcat", "--", "sh", "-c", check]).await {
            Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
                "docker-unavailable" => {
                    health.docker_ready = Some(false);
                    health.issue = Some("虚拟机中的 Docker 引擎无响应");
                }
                names => {
                    health.docker_ready = Some(true);
                    health.node_running = Some(names.lines().any(|name| name == "modelcat-node"));
                    health.agent_running = Some(names.lines().any(|name| matches!(name, "modelcat-tool-claude" | "modelcat-tool-codex")));
                }
            },
            Err(_) => health.issue = Some("无法连接虚拟机检查 Docker 和节点"),
        }
        health
    }

    async fn shutdown(&self) -> Result<()> {
        if self.root.join("lima/modelcat/lima.yaml").is_file() && self.inspect_running().await? {
            self.bounded_call(
                &["shell", "modelcat", "--", "sh", "-c", r#"
set -eu
if ! command -v docker >/dev/null || ! sudo -n systemctl is-active --quiet docker; then
    exit 0
fi
node=$(sudo -n docker ps --quiet --filter 'name=^/modelcat-node$')
if [ -n "$node" ]; then
    sudo -n docker stop --timeout -1 "$node" >/dev/null
    exit_code=$(sudo -n docker inspect --format '{{.State.ExitCode}}' "$node")
    if [ "$exit_code" -ne 0 ]; then
        printf 'ModelCat node exited with code %s during shutdown\n' "$exit_code" >&2
        exit 1
    fi
fi
"#],
                90,
                "节点仍在处理请求。工作区仍在运行，请稍后重试停止",
            ).await?;
        }
        self.stop_if_running().await
    }

    async fn stop_if_running(&self) -> Result<()> {
        if self.root.join("lima/modelcat/lima.yaml").is_file()
            && self.instance_status().await? != "Stopped"
        {
            self.bounded_call(&["stop", "--tty=false", "modelcat"], 90, "等待 Lima 停止虚拟机超时").await?;
            let status = self.instance_status().await?;
            ensure!(status == "Stopped", "Lima 未确认虚拟机已停止，当前状态：{status}");
        }
        Ok(())
    }

    #[cfg(test)]
    fn node_started_at(&self) -> Result<Vec<u8>> {
        let output = self.call(&[
            "shell", "modelcat", "--", "sudo", "-n", "docker", "inspect", "--format",
            "{{.State.StartedAt}}", "modelcat-node",
        ])?;
        Ok(output.stdout)
    }
}

pub async fn monitor_workspace(app: tauri::AppHandle) {
    let state = app.state::<Arc<VmState>>().inner().clone();
    while let Some(window) = app.get_webview_window("main") {
        let driver = state.workspace.try_lock().ok().and_then(|workspace| {
            workspace.as_ref().map(|workspace| workspace.driver.clone())
        });
        if let Some(driver) = driver {
            let health = driver.inspect_health().await;
            if let Ok(payload) = serde_json::to_string(&health) {
                // Publish only status data to the desktop shell or a standalone node page.
                let _ = window.eval(format!(
                    "if (location.protocol === 'tauri:' || location.origin === 'http://127.0.0.1:{}') {{
                        window.dispatchEvent(new CustomEvent('modelcat-runtime', {{detail: {payload}}}));
                    }}",
                    workspace_host_port()
                ));
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn stop_saved_workspace(root: PathBuf) -> Result<()> {
    let _lock = lock_workspace(&root)
        .context("close the ModelCat window, or choose Stop Workspace from its menu")?;
    Driver { root, runtime: runtime()? }.shutdown().await
}

pub async fn stop_workspace_command(directory: Option<&Path>) -> Result<()> {
    let root = workspace_root(directory)?;
    ensure!(root.join(WORKSPACE_MARKER).is_file(), "open the ModelCat workspace first");
    stop_saved_workspace(root).await?;
    println!("ModelCat workspace stopped");
    Ok(())
}

fn lock_workspace(root: &Path) -> Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("app.lock"))?;
    lock.try_lock()?;
    Ok(lock)
}

pub fn run_tool_command(directory: Option<&Path>, action: &crate::ToolAction) -> Result<()> {
    use crate::ToolAction;
    let root = workspace_root(directory)?;
    ensure!(root.join(WORKSPACE_MARKER).is_file(), "open the ModelCat workspace first");
    let driver = Driver { root, runtime: runtime()? };
    ensure!(driver.is_running()?, "the workspace VM is stopped");
    let (command, tool, extra): (&str, &str, &[&str]) = match action {
        ToolAction::Start { tool } => return driver.launch_tool(tool, "start"),
        ToolAction::Restart { tool } => return driver.launch_tool(tool, "restart"),
        ToolAction::Authorize { tool } => ("authorize", tool, &[]),
        ToolAction::Status { tool } => ("status", tool, &[]),
        ToolAction::Stop { tool } => ("stop", tool, &[]),
        ToolAction::Logs { tool } => ("logs", tool, &[]),
        ToolAction::Autostart { tool, state } => ("autostart", tool, &[state]),
        ToolAction::Run { tool, args } => {
            use std::io::IsTerminal;
            let mut command = driver.command();
            command.args(["shell", "modelcat", "--", "sh", "-c", TOOL, "modelcat-tool", "run", tool]);
            command.arg(if std::io::stdin().is_terminal() { "tty" } else { "stdin" });
            let result = command.args(args).status()?;
            ensure!(result.success(), "{tool} exited with {result}");
            return Ok(());
        }
    };
    print!("{}", driver.tool(command, tool, extra)?);
    Ok(())
}

pub(crate) fn management_token(url: &tauri::Url) -> Result<&str> {
    url.fragment()
        .and_then(|fragment| fragment.strip_prefix("token="))
        .filter(|token| !token.is_empty())
        .context("缺少管理密钥")
}

pub fn workspace_connection(directory: Option<&Path>) -> Result<Option<(String, String)>> {
    if directory.is_some_and(|path| !path.join(WORKSPACE_MARKER).is_file()) {
        return Ok(None);
    }
    let root = workspace_root(directory)?;
    if !root.join(WORKSPACE_MARKER).is_file() {
        return Ok(None);
    }
    let driver = Driver { root, runtime: runtime()? };
    ensure!(driver.is_running()?, "the workspace VM is stopped");
    let url = driver.management_url()?;
    Ok(Some((url.origin().ascii_serialization(), management_token(&url)?.to_owned())))
}

fn runtime() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MODELCAT_VM_RUNTIME") {
        return Ok(path.into());
    }
    Ok(std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("Resources/vm-runtime"))
}
fn check_root(root: &Path) -> Result<PathBuf> {
    ensure!(
        !root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "存储路径不能包含上级目录跳转"
    );
    ensure!(root.is_absolute(), "请选择绝对路径");
    // Resolve the nearest existing ancestor before creating anything, including symlink aliases.
    let mut ancestor = root;
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("存储路径无效")?;
    }
    let resolved = ancestor.canonicalize()?.join(root.strip_prefix(ancestor)?);
    if let Some(home) = std::env::var_os("HOME") {
        ensure!(
            !resolved.starts_with(Path::new(&home).canonicalize()?)
                || resolved.file_name().is_some_and(|name| name == DATA_DIRECTORY),
            "主目录内请使用独立的 ModelCatVMData 目录"
        );
    }
    ensure!(
        resolved.to_string_lossy().len() < 65,
        "目录路径过长，请选择更靠近磁盘根目录的位置"
    );
    ensure!(
        !resolved
            .components()
            .any(|c| c.as_os_str().to_string_lossy().ends_with(".app")),
        "不能把虚拟磁盘放进 App 包内"
    );
    Ok(resolved)
}

fn dev_data_directory() -> Option<PathBuf> {
    if !cfg!(any(debug_assertions, feature = "desktop-preview")) {
        return None;
    }
    let path = PathBuf::from(std::env::var_os("MODELCAT_VM_DEV_DATA")?);
    if path.file_name()?.to_str()? != DATA_DIRECTORY {
        return None;
    }
    Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?))
}

#[tauri::command]
pub fn vm_default_directory() -> String {
    if let Some(directory) = dev_data_directory() {
        return directory.display().to_string();
    }
    let parent = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("/Applications"));
    parent.join(DATA_DIRECTORY).display().to_string()
}

fn workspace_root(directory: Option<&Path>) -> Result<PathBuf> {
    if let Some(directory) = directory {
        return check_root(directory);
    }
    selected_workspace_root(Path::new(&vm_default_directory()))
}

pub(crate) fn codex_profile(create: bool) -> Result<PathBuf> {
    let root = workspace_root(None)?;
    if create {
        prepare_root(&root)?;
    }
    crate::codex_login::profile(&root, create)
}

fn selected_workspace_root(default: &Path) -> Result<PathBuf> {
    let default = check_root(default)?;
    match std::fs::read(default.join(WORKSPACE_DIRECTORY)) {
        Ok(data) => check_root(&serde_json::from_slice::<PathBuf>(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn remember_workspace_root(default: &Path, root: &Path) -> Result<()> {
    if selected_workspace_root(default).as_deref().is_ok_and(|saved| saved == root) {
        return Ok(());
    }
    let pending = default.join(format!(".workspace-{}.json", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        std::fs::write(&pending, serde_json::to_vec(root)?)?;
        std::fs::rename(&pending, default.join(WORKSPACE_DIRECTORY))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(pending);
    }
    result.context("无法保存工作区位置")
}

#[tauri::command]
pub fn vm_workspace_directory() -> std::result::Result<String, String> {
    workspace_root(None).map(|root| root.display().to_string()).map_err(|error| format!("{error:#}"))
}
#[tauri::command]
pub async fn vm_choose_directory(
    directory: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<VmState>>,
) -> std::result::Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let (selected, result) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let _ = selected.send(choose_macos_directory(&directory));
        }).map_err(|e| e.to_string())?;
        let selected = result.await.map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
        if let Some(path) = selected.as_deref() {
            let _operation = state.lifecycle.try_lock()
                .map_err(|_| "请等待当前工作区操作完成")?;
            if state.workspace.lock().unwrap().is_some() {
                return Err("请先停止工作区，再更改存储位置".into());
            }
            let root = Path::new(path);
            let default = check_root(Path::new(&vm_default_directory())).map_err(|e| format!("{e:#}"))?;
            prepare_root(root).and_then(|_| remember_workspace_root(&default, root))
                .map_err(|e| format!("{e:#}"))?;
        }
        Ok(selected)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (directory, app, state);
        Err("此平台不支持选择工作区目录".into())
    }
}

#[cfg(target_os = "macos")]
fn choose_macos_directory(current: &str) -> Result<Option<String>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSString, NSURL};

    let main_thread = MainThreadMarker::new().context("无法在主线程打开目录选择器")?;
    let panel = NSOpenPanel::openPanel(main_thread);
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    panel.setPrompt(Some(&NSString::from_str("选择")));
    panel.setMessage(Some(&NSString::from_str("选择现有的 ModelCatVMData，或选择要创建工作区的上一级目录。")));
    if let Some(parent) = Path::new(current).parent().and_then(Path::to_str) {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath_isDirectory(&NSString::from_str(parent), true)));
    }
    let response = panel.runModal();
    if response == NSModalResponseCancel {
        return Ok(None);
    }
    ensure!(response == NSModalResponseOK, "无法打开目录选择器");
    let path = panel.URL().and_then(|url| url.path()).context("无法读取所选目录")?.to_string();
    let path = PathBuf::from(path);
    let root = if path.file_name().is_some_and(|name| name == DATA_DIRECTORY) {
        path
    } else {
        path.join(DATA_DIRECTORY)
    };
    Ok(Some(check_root(&root)?.display().to_string()))
}
#[tauri::command]
pub fn vm_platform_gateway() -> &'static str {
    PLATFORM_GATEWAY
}

#[tauri::command]
pub fn vm_platform_verified() -> bool {
    check_root(Path::new(&vm_default_directory()))
        .is_ok_and(|root| root.join(PLATFORM_VERIFIED).is_file())
}

fn prepare_root(root: &Path) -> Result<()> {
    if root.exists() && !root.join(WORKSPACE_MARKER).is_file() {
        // Earlier previews created the account profile before initializing the workspace.
        for entry in root.read_dir()? {
            let entry = entry?;
            ensure!(
                entry.file_name() == crate::codex_login::PROFILE_DIRECTORY
                    && entry.file_type()?.is_dir(),
                "目录包含其他文件，请选择新的存放位置"
            );
        }
    }
    std::fs::create_dir_all(root).context("无法创建工作区目录")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(root.join(WORKSPACE_MARKER), "ModelCat VM experiment 2\n")?;
    Ok(())
}

#[tauri::command]
pub async fn vm_verify_platform(key: String) -> std::result::Result<(), String> {
    channel_node::onboarding::verify_platform(PLATFORM_GATEWAY, &key)
        .await
        .map_err(|error| error.to_string())?;
    let result = (|| -> Result<()> {
        let root = check_root(Path::new(&vm_default_directory()))?;
        prepare_root(&root)?;
        let receipt = root.join(PLATFORM_VERIFIED);
        if !receipt.is_file() {
            std::fs::OpenOptions::new().write(true).create_new(true).open(receipt)?;
        }
        Ok(())
    })();
    result.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn vm_logout_platform(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<VmState>>,
) -> std::result::Result<(), String> {
    if window.label() != "main" {
        return Err("请在 ModelCat 主窗口退出登录".into());
    }
    let worker = state.inner().clone();
    let operation = worker.lifecycle.clone().lock_owned().await;
    tauri::async_runtime::spawn_blocking(move || -> Result<()> {
        let _operation = operation;
        let root = check_root(Path::new(&vm_default_directory()))?;
        worker.stop(true)?;
        match std::fs::remove_file(root.join(PLATFORM_VERIFIED)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("无法清除本机平台登录状态"),
        }
    }).await.map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn vm_attach_workspace(
    directory: String,
    state: tauri::State<'_, Arc<VmState>>,
) -> std::result::Result<bool, String> {
    let result: Result<bool> = async {
        let root = check_root(Path::new(&directory))?;
        if !root.join(WORKSPACE_MARKER).is_file() || !root.join("lima/modelcat/lima.yaml").is_file() {
            return Ok(false);
        }
        let _operation = state.lifecycle.lock().await;
        if state.exit_requested.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if state.workspace.lock().unwrap().is_some() {
            return Ok(true);
        }
        let driver = Driver { root, runtime: runtime()? };
        let lock = lock_workspace(&driver.root).context("此工作区已在另一个 ModelCat 窗口中打开")?;
        if !driver.inspect_running().await? {
            return Ok(false);
        }
        let monitor = driver.clone();
        tauri::async_runtime::spawn_blocking(move || monitor.start_monitor());
        *state.workspace.lock().unwrap() = Some(Workspace {
            driver,
            _lock: lock,
            management_url: None,
        });
        Ok(true)
    }.await;
    result.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn vm_tool(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<VmState>>,
    tool: String,
    action: String,
) -> std::result::Result<String, String> {
    if window.label() != "main" || !channel_node::tool_config::TOOLS.contains(&tool.as_str()) {
        return Err("工具操作无效".into());
    }
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        let workspace = state.workspace.lock().unwrap();
        let driver = &workspace.as_ref().context("请先打开工作区")?.driver;
        match action.as_str() {
            "start" | "restart" => driver.launch_tool_progress(&tool, &action, |message| {
                let _ = window.emit("tool-start-progress", serde_json::json!({"tool": tool, "message": message}));
            })?,
            "status" | "logs" | "authorize" | "stop" | "online" | "offline" => return driver.tool(&action, &tool, &[]),
            "enable" | "disable" => return driver.tool("autostart", &tool, &[&action]),
            _ => anyhow::bail!("工具操作无效"),
        }
        Ok(String::new())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

fn stop_on_exit(root: &Path, enabled: Option<bool>) -> Result<bool> {
    let preference = root.join(KEEP_WORKSPACE_RUNNING);
    match enabled {
        Some(false) => {
            prepare_root(root)?;
            std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&preference)?;
        }
        Some(true) => match std::fs::remove_file(&preference) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        },
        None => {},
    }
    Ok(!preference.try_exists()?)
}

fn read_stop_on_exit() -> Result<bool> {
    stop_on_exit(&check_root(Path::new(&vm_default_directory()))?, None)
}

pub fn stop_workspace_on_system_exit(app: &tauri::AppHandle) {
    // AppKit cannot defer this event. Use a separate thread to avoid nesting Tokio runtimes.
    let state = app.state::<Arc<VmState>>().inner().clone();
    let result = std::thread::spawn(move || state.stop_on_exit(|| {})).join()
        .map_err(|_| anyhow::anyhow!("工作区关闭线程异常结束"))
        .and_then(|result| result);
    if let Err(error) = result {
        eprintln!("停止工作区失败：{error:#}");
    }
}

#[tauri::command]
pub fn vm_stop_on_exit(window: tauri::WebviewWindow, enabled: Option<bool>) -> std::result::Result<bool, String> {
    if window.label() != "main" {
        return Err("请在 ModelCat 设置中修改此选项".into());
    }
    check_root(Path::new(&vm_default_directory()))
        .and_then(|root| stop_on_exit(&root, enabled))
        .map_err(|error| format!("无法访问关闭偏好。{error:#}"))
}

pub fn stop_workspace(app: &tauri::AppHandle, exit: bool) {
    let state = app.state::<Arc<VmState>>().inner().clone();
    if exit && state.exit_requested.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _operation = state.lifecycle.lock().await;
        if !exit && state.exit_requested.load(Ordering::SeqCst) {
            return;
        }
        let window = app.get_webview_window("main");
        let original_title = window.as_ref().and_then(|window| window.title().ok());
        let progress_window = window.clone();
        let worker = state.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let show_stopping = || {
                if let Some(window) = progress_window {
                    let _ = window.emit_to("main", "workspace-stopping", ());
                    let _ = window.set_title("ModelCat · 正在停止工作区…");
                }
            };
            if exit {
                worker.stop_on_exit(show_stopping)
            } else {
                show_stopping();
                worker.stop(true)
            }
        }).await.map_err(anyhow::Error::from).and_then(|result| result);
        let message = match result {
            Ok(()) if exit => {
                app.exit(0);
                return;
            }
            Ok(()) => None,
            Err(error) => Some(format!("停止工作区失败：{error:#}")),
        };
        if exit {
            state.exit_requested.store(false, Ordering::SeqCst);
        }
        if let Some(window) = window {
            if let Some(title) = original_title {
                let _ = window.set_title(&title);
            }
            let mut url = tauri::Url::parse("tauri://localhost/vm.html").unwrap();
            if let Some(message) = message {
                url.query_pairs_mut().append_pair("status", &message);
            }
            let _ = window.navigate(url);
            let _ = window.set_focus();
        }
    });
}

#[tauri::command]
pub fn vm_stop(app: tauri::AppHandle) {
    stop_workspace(&app, false);
}

#[tauri::command]
pub async fn vm_open(
    state: tauri::State<'_, Arc<VmState>>,
) -> std::result::Result<Option<String>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>> {
        let mut workspace = state.workspace.lock().unwrap();
        let Some(workspace) = workspace.as_mut() else {
            return Ok(None);
        };
        if workspace.management_url.is_none() {
            let url = workspace.driver.management_url()?;
            management_token(&url)?;
            workspace.management_url = Some(url);
        }
        Ok(workspace.management_url.as_ref().map(|url| url.as_str().to_owned()))
    }).await.map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn vm_start(
    directory: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<VmState>>,
) -> std::result::Result<String, String> {
    if !vm_platform_verified() {
        return Err("请先验证平台 API Key".into());
    }
    let state = state.inner().clone();
    let operation = state.lifecycle.clone().try_lock_owned()
        .map_err(|_| "请等待当前工作区操作完成")?;
    if state.exit_requested.load(Ordering::SeqCst) {
        return Err("ModelCat 正在退出".into());
    }
    let worker = state.clone();
    let progress = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        let _operation = operation;
        let root = check_root(Path::new(&directory))?;
        let driver = Driver {
            root,
            runtime: runtime()?,
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        if driver.root.join(WORKSPACE_MARKER).is_file() {
            let _ = progress.emit_to("main", "workspace-start-progress", "正在检查工作区…");
            runtime.block_on(driver.inspect_running())?;
        }
        prepare_root(&driver.root)?;
        let lock = lock_workspace(&driver.root).context("无法锁定工作区，请检查是否已在其他窗口打开")?;
        for sub in ["host-home", "cache", "tmp"] {
            std::fs::create_dir_all(driver.root.join(sub))?;
        }
        let was_running = runtime.block_on(driver.inspect_running())?;
        let default = check_root(Path::new(&vm_default_directory()))?;
        let result = driver.open_workspace(&default, was_running, |message| {
            let _ = progress.emit_to("main", "workspace-start-progress", message);
        });
        if result.is_ok() || (!was_running && runtime.block_on(driver.stop_if_running()).is_err()) {
            *worker.workspace.lock().unwrap() = Some(Workspace {
                driver,
                _lock: lock,
                management_url: result.as_ref().ok().cloned(),
            });
        }
        result.map(Into::into)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| format!("{e:#}")))
}

pub(crate) fn workspace_host_port() -> u16 {
    if cfg!(any(debug_assertions, feature = "desktop-preview")) {
        return std::env::var("MODELCAT_VM_PORT")
            .or_else(|_| std::env::var("MODELCAT_VM_TEST_PORT"))
            .ok()
            .and_then(|port| port.parse().ok())
            .filter(|port| *port >= 1024)
            .unwrap_or(19701);
    }
    19701
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_home_app_bundle_and_parent_traversal() {
        let home = std::env::var_os("HOME").unwrap();
        assert!(check_root(&PathBuf::from(home).join("modelcat-new-data")).is_err());
        assert!(check_root(Path::new("/Applications/Test.app/Contents/new-data")).is_err());
        assert!(check_root(Path::new("/Applications/not-created/../../Users/data")).is_err());
        assert!(check_root(Path::new("relative-data")).is_err());
        assert!(check_root(Path::new("/Applications/ModelCatVMData")).is_ok());
    }
    #[test]
    fn codex_profile_does_not_block_first_workspace_start() -> Result<()> {
        let scratch = std::env::temp_dir().join(format!("modelcat-codex-{}", uuid::Uuid::new_v4()));
        let root = scratch.join(DATA_DIRECTORY);
        let result = (|| -> Result<()> {
            let profile = crate::codex_login::profile(&root, false)?;
            ensure!(!root.exists(), "reading login status created a workspace");

            std::fs::create_dir_all(&profile)?;
            let prior_data = profile.join("existing");
            std::fs::write(&prior_data, b"legacy profile")?;
            let other = root.join("unrelated");
            std::fs::write(&other, b"unrelated data")?;
            ensure!(prepare_root(&root).is_err(), "adopted unrelated directory");
            std::fs::remove_file(other)?;

            prepare_root(&root)?;
            ensure!(root.join(WORKSPACE_MARKER).is_file(), "missing workspace marker");
            ensure!(std::fs::read(prior_data)? == b"legacy profile", "lost existing profile");
            Ok(())
        })();
        let cleanup = std::fs::remove_dir_all(scratch);
        result?;
        Ok(cleanup?)
    }
    #[test]
    fn failed_start_preserves_the_saved_workspace_directory() -> Result<()> {
        let scratch = std::env::temp_dir().join(format!("modelcat-start-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&scratch)?;
        let result = (|| -> Result<()> {
            let candidate = scratch.join("candidate");
            std::fs::create_dir(&candidate)?;
            let saved = scratch.join(WORKSPACE_DIRECTORY);
            let original = serde_json::to_vec(&scratch.join("current"))?;
            std::fs::write(&saved, &original)?;
            let driver = Driver { root: candidate, runtime: scratch.join("missing-runtime") };

            ensure!(driver.open_workspace(&scratch, false, |_| {}).is_err(), "expected Lima startup to fail");
            ensure!(std::fs::read(saved)? == original, "failed startup changed the saved workspace");
            Ok(())
        })();
        let cleanup = std::fs::remove_dir_all(scratch);
        result?;
        Ok(cleanup?)
    }
    #[test]
    #[ignore = "requires native macOS virtualization and an explicitly selected disposable directory"]
    fn native_vm_backend_smoke() {
        let root = PathBuf::from(
            std::env::var_os("MODELCAT_VM_TEST_ROOT").expect("dedicated test directory required"),
        );
        for sub in ["host-home", "cache", "tmp"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let driver = Driver {
            root: root.clone(),
            runtime: runtime().unwrap(),
        };
        // The test directory is task-owned. Recreate its VM configuration on the next start.
        let outcome = (|| -> Result<()> {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let url = driver.start(runtime.block_on(driver.inspect_running())?, |_| {})?;
            ensure!(
                url.as_str().starts_with(&format!("http://127.0.0.1:{}/#token=", workspace_host_port())),
                "invalid local endpoint"
            );
            let result = driver.call(&[
                "shell", "modelcat", "--", "sh", "-c",
                r#"set -eu
test "$(uname -s)" = Linux
test -f ~/.modelcat/control.token
if grep -Eq ' - (virtiofs|9p) ' /proc/self/mountinfo; then exit 1; fi
test "$(sudo -n docker inspect --format '{{.State.Running}}' modelcat-node)" = true"#,
            ])?;
            ensure!(
                result.status.success(),
                "unexpected guest mounts, missing guest profile, or stopped container"
            );
            let started_at = driver.node_started_at()?;
            ensure!(
                driver.start(true, |_| {})? == url && driver.node_started_at()? == started_at,
                "connecting to a running workspace restarted the node"
            );
            let verify_tools = std::env::var_os("MODELCAT_VM_VERIFY_TOOLS").is_some();
            if verify_tools {
                for tool in channel_node::tool_config::TOOLS {
                    driver.tool("authorize", tool, &[])?;
                    driver.launch_tool(tool, "start")?;
                    let info = driver.tool("status", tool, &[])?;
                    let status: serde_json::Value = serde_json::from_str(&info)?;
                    ensure!(status["state"] == "running", "{tool} did not start");
                    ensure!(!driver.tool("logs", tool, &[])?.is_empty(), "{tool} logs are empty");
                    driver.tool("stop", tool, &[])?;
                    driver.tool("autostart", tool, &[if *tool == "claude" { "enable" } else { "disable" }])?;
                }
                driver.start(true, |_| {})?;
                let status: serde_json::Value = serde_json::from_str(&driver.tool("status", "claude", &[])?)?;
                ensure!(status["state"] == "exited", "reopening the window restarted a manually stopped tool");
            }
            runtime.block_on(driver.shutdown())?;
            ensure!(
                driver.start(false, |_| {})? == url,
                "workspace lost its management identity after restart"
            );
            if verify_tools {
                for (tool, expected) in [("claude", "running"), ("codex", "exited")] {
                    let status: serde_json::Value = serde_json::from_str(&driver.tool("status", tool, &[])?)?;
                    ensure!(status["state"] == expected, "unexpected {tool} state after workspace restart");
                }
                driver.tool("autostart", "claude", &["disable"])?;
                driver.tool("stop", "claude", &[])?;
            }
            Ok(())
        })();
        let stopped = driver.call(&["stop", "modelcat"]);
        outcome.unwrap();
        stopped.unwrap();
    }
}
