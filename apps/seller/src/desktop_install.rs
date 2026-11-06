//! Official application/runtime downloads. No signature or operating-system protection is disabled.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

async fn download(url: &str, destination: &Path, max: usize) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut response = client.get(url).send().await?;
    ensure!(
        response.url().scheme() == "https" && response.status().is_success(),
        "Official download failed; use the vendor's download page and rerun setup"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= max,
            "Download exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    fs::write(destination, bytes)?;
    Ok(())
}

pub async fn application(client: &str, no_install: bool) -> Result<PathBuf> {
    let name = if client == "codex" { "Codex" } else { "Claude" };
    let home = crate::state::home_dir().context("Home unavailable")?;
    let global = PathBuf::from(format!("/Applications/{name}.app"));
    let local = home.join(format!("Applications/{name}.app"));
    for app in [&global, &local] {
        let exe = app.join(format!("Contents/MacOS/{name}"));
        if exe.is_file() {
            return Ok(exe);
        }
    }
    if no_install {
        return Ok(global.join(format!("Contents/MacOS/{name}")));
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Desktop installation requires macOS");
    #[cfg(target_os = "macos")]
    {
        let root = home
            .join("Library/Application Support/ModelCat/Downloads")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&root)?;
        let dmg = root.join("installer.dmg");
        let mount = root.join("mount");
        fs::create_dir(&mount)?;
        let url = if client == "codex" {
            "https://persistent.oaistatic.com/codex-app-prod/Codex.dmg"
        } else {
            "https://claude.ai/api/desktop/darwin/universal/dmg/latest/redirect"
        };
        eprintln!("正在从厂商官方下载 {name}，安装到 ~/Applications …");
        download(url, &dmg, 768 * 1024 * 1024).await?;
        ensure!(
            Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-readonly", "-quiet", "-mountpoint"])
                .arg(&mount)
                .arg(&dmg)
                .status()?
                .success(),
            "Cannot mount vendor installer"
        );
        let source = mount.join(format!("{name}.app"));
        let result = (|| -> Result<()> {
            ensure!(
                source.is_dir(),
                "Vendor installer layout changed; install from its official website and rerun"
            );
            ensure!(
                Command::new("codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(&source)
                    .status()?
                    .success(),
                "Vendor app signature verification failed"
            );
            ensure!(Command::new("spctl").args(["--assess","--type","execute"]).arg(&source).status()?.success(),"macOS did not accept the vendor signature; install using the official vendor process");
            fs::create_dir_all(local.parent().unwrap())?;
            ensure!(!local.exists(), "Vendor app appeared during setup; rerun");
            ensure!(
                Command::new("ditto")
                    .arg(&source)
                    .arg(&local)
                    .status()?
                    .success(),
                "Could not install vendor application"
            );
            Ok(())
        })();
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet"])
            .arg(&mount)
            .status();
        let _ = fs::remove_dir_all(&root);
        result?;
        Ok(local.join(format!("Contents/MacOS/{name}")))
    }
}

pub async fn node(no_install: bool) -> Result<Option<PathBuf>> {
    let home = crate::state::home_dir().context("Home unavailable")?;
    let root = home.join("Library/Application Support/ModelCat/Runtime");
    let cache = root.join("node/bin");
    for bin in [Some(cache.clone()), None] {
        let exe = bin
            .as_ref()
            .map(|p| p.join("node"))
            .unwrap_or_else(|| PathBuf::from("node"));
        if let Ok(output) = Command::new(&exe).arg("--version").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut parts = text.trim().trim_start_matches('v').split('.');
            let major = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            let minor = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            if output.status.success() && (major > 24 || (major == 24 && minor >= 9)) {
                return Ok(bin);
            }
        }
    }
    if no_install {
        return Ok(None);
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Install Node.js >=24.9 before running DSH");
    #[cfg(target_os = "macos")]
    {
        fs::create_dir_all(&root)?;
        let work = root.join(format!("download-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&work)?;
        let index = work.join("index.json");
        download(
            "https://nodejs.org/dist/index.json",
            &index,
            5 * 1024 * 1024,
        )
        .await?;
        let releases: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&index)?)?;
        let version = releases
            .iter()
            .filter_map(|v| v["version"].as_str())
            .find(|v| v.starts_with("v24."))
            .context("Node 24 release not found")?;
        ensure!(
            version
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == 'v'),
            "Invalid Node release version"
        );
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        let name = format!("node-{version}-darwin-{arch}");
        let asset = format!("{name}.tar.gz");
        let base = format!("https://nodejs.org/dist/{version}");
        let checks = work.join("SHASUMS256.txt");
        download(&format!("{base}/SHASUMS256.txt"), &checks, 1024 * 1024).await?;
        let archive = work.join(&asset);
        eprintln!("正在安装 DSH 所需的独立 Node.js {version} …");
        download(&format!("{base}/{asset}"), &archive, 100 * 1024 * 1024).await?;
        let checks = fs::read_to_string(checks)?;
        let expected = checks
            .lines()
            .find_map(|l| {
                let p: Vec<_> = l.split_whitespace().collect();
                if p.len() == 2 && p[1] == asset {
                    Some(p[0])
                } else {
                    None
                }
            })
            .context("Node checksum missing")?;
        ensure!(
            format!("{:x}", Sha256::digest(fs::read(&archive)?)) == expected,
            "Node checksum does not match"
        );
        ensure!(
            Command::new("tar")
                .arg("-xzf")
                .arg(&archive)
                .arg("-C")
                .arg(&work)
                .status()?
                .success(),
            "Node extraction failed"
        );
        ensure!(!root.join("node").exists(),"A previous unsupported private Node runtime exists; remove it before installing a new one");
        fs::rename(work.join(name), root.join("node"))?;
        fs::remove_dir_all(work)?;
        Ok(Some(cache))
    }
}
