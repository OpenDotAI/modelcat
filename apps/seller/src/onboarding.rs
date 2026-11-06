//! Buyer setup touches only ModelCat-owned profiles and launchers, never vendor logins.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let tmp = parent.join(format!(".modelcat-{}", uuid::Uuid::new_v4()));
    fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    }
    if path.exists() {
        fs::copy(path, path.with_extension("modelcat-backup"))?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}
fn read_key(path: Option<&Path>, stored: &Path) -> Result<String> {
    let key = if let Some(path) = path {
        fs::read_to_string(path)?
    } else if stored.exists() {
        fs::read_to_string(stored)?
    } else {
        eprint!("请输入网站买家工作台创建的 API Key（输入不会显示）：");
        io::stderr().flush()?;
        let hidden = Command::new("stty").arg("-echo").status()?.success();
        ensure!(hidden, "Please run in Terminal, or pass --key-file");
        let mut value = String::new();
        let result = io::stdin().read_line(&mut value);
        let _ = Command::new("stty").arg("echo").status();
        eprintln!();
        result?;
        value
    };
    let key = key.trim().to_owned();
    ensure!(
        !key.is_empty() && key.len() <= 512 && key.bytes().all(|b| b.is_ascii_graphic()),
        "Invalid API key"
    );
    Ok(key)
}

pub async fn models(base: &str, key: &str) -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client
        .get(format!("{}/models", base.trim_end_matches('/')))
        .bearer_auth(key)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Gateway model listing failed (HTTP {}); verify the gateway URL and API key",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 2 * 1024 * 1024,
            "Model listing too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let data: Value =
        serde_json::from_slice(&bytes).context("Gateway did not return a JSON model list")?;
    let items = data
        .get("data")
        .and_then(Value::as_array)
        .context("Gateway model list has no data array")?;
    let mut ids = BTreeSet::new();
    for item in items {
        if let Some(id) = item["id"].as_str() {
            ensure!(
                !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic()),
                "Invalid model ID in gateway response"
            );
            ids.insert(id.to_owned());
        }
    }
    ensure!(
        !ids.is_empty() && ids.len() <= 2000,
        "No available models; contact the gateway administrator"
    );
    Ok(ids.into_iter().collect())
}

pub async fn setup(
    client: &str,
    gateway: &str,
    key_file: Option<&Path>,
    no_launch: bool,
) -> Result<()> {
    ensure!(
        ["codex", "claude", "dsh"].contains(&client),
        "Unsupported client"
    );
    let mut url = reqwest::Url::parse(gateway)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Gateway URL must not contain credentials, query or fragment"
    );
    let host = url.host_str().context("Gateway host required")?.to_owned();
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && ["localhost", "127.0.0.1", "[::1]", "::1"].contains(&host.as_str())),
        "Use HTTPS except for a loopback development gateway"
    );
    let mut path = url.path().trim_end_matches('/').to_owned();
    if !path.ends_with("/v1") {
        path.push_str("/v1");
    }
    url.set_path(&path);
    let base = url.as_str().trim_end_matches('/').to_owned();
    let origin = base.strip_suffix("/v1").context("Invalid API base")?;
    let home = crate::state::home_dir().context("Home directory unavailable")?;
    use sha2::{Digest, Sha256};
    let id = format!("{:x}", Sha256::digest(base.as_bytes()))[..12].to_owned();
    let root = std::env::var_os("MODELCAT_BUYER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Library/Application Support/ModelCat/Buyers"))
        .join(&id)
        .join(client);
    let stored_key = root.join("gateway.key");
    let key = read_key(key_file, &stored_key)?;
    let mut ids = models(&base, &key).await?;
    if client == "claude" {
        ids.retain(|s| s.to_ascii_lowercase().contains("claude"));
        ensure!(!ids.is_empty(), "No Claude models are available; Claude Desktop requires an Anthropic Messages-compatible Claude model");
    }
    write(&stored_key, key.as_bytes(), 0o600)?;
    write(
        &root.join("models.json"),
        &serde_json::to_vec_pretty(
            &json!({"data":ids.iter().map(|s|json!({"id":s})).collect::<Vec<_>>()}),
        )?,
        0o600,
    )?;
    let profile = root.join("profile");
    let desktop = if client == "dsh" {
        None
    } else {
        Some(crate::desktop_install::application(client, no_launch).await?)
    };
    let node_bin = if client == "dsh" {
        crate::desktop_install::node(no_launch).await?
    } else {
        None
    };
    let launcher = match client {
        "codex" => {
            let catalog = profile.join("model-catalog.json");
            let entries: Vec<Value> = ids.iter().enumerate().map(|(i,id)| json!({"slug":id,"display_name":id,"description":"ModelCat gateway model","visibility":"list","supported_in_api":true,"priority":i,"default_reasoning_level":"medium","supported_reasoning_levels":[{"effort":"medium","description":"Default"}],"shell_type":"shell_command","base_instructions":"You are a helpful coding assistant.","supports_reasoning_summaries":false,"support_verbosity":false,"default_verbosity":null,"apply_patch_tool_type":null,"truncation_policy":{"mode":"tokens","limit":10000},"supports_parallel_tool_calls":true,"context_window":128000,"effective_context_window_percent":95,"experimental_supported_tools":[],"input_modalities":["text"],"prefer_websockets":false})).collect();
            write(
                &catalog,
                &serde_json::to_vec_pretty(&json!({"models":entries}))?,
                0o600,
            )?;
            let config = format!("model = {}\nmodel_provider = \"modelcat\"\nmodel_catalog_json = {}\n\n[model_providers.modelcat]\nname = \"ModelCat\"\nbase_url = {}\nenv_key = \"MODELCAT_GATEWAY_KEY\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",json!(ids[0]),json!(catalog.to_string_lossy()),json!(base));
            write(&profile.join("config.toml"), config.as_bytes(), 0o600)?;
            format!("#!/bin/sh\nset -eu\nexport CODEX_HOME={}\nexport MODELCAT_GATEWAY_KEY=$(cat {})\nexec {} --user-data-dir={}\n",shell(&profile.to_string_lossy()),shell(&stored_key.to_string_lossy()),shell(&desktop.as_ref().unwrap().to_string_lossy()),shell(&root.join("desktop").to_string_lossy()))
        }
        "claude" => {
            let config_id = "cac00000-0000-4000-8000-000000000001";
            let lib = profile.join("configLibrary");
            let config = json!({"inferenceProvider":"gateway","inferenceGatewayBaseUrl":origin,"inferenceGatewayApiKey":key,"inferenceGatewayAuthScheme":"bearer","inferenceModels":ids.iter().map(|s|json!({"name":s,"displayName":s})).collect::<Vec<_>>(),"deploymentDisplayName":"ModelCat"});
            write(
                &lib.join(format!("{config_id}.json")),
                &serde_json::to_vec_pretty(&config)?,
                0o600,
            )?;
            write(
                &lib.join("_meta.json"),
                &serde_json::to_vec_pretty(
                    &json!({"appliedId":config_id,"entries":[{"id":config_id,"name":"ModelCat","provider":"gateway"}]}),
                )?,
                0o600,
            )?;
            write(
                &profile.join("claude_desktop_config.json"),
                b"{\"deploymentMode\":\"3p\"}",
                0o600,
            )?;
            format!(
                "#!/bin/sh\nset -eu\nexport CLAUDE_USER_DATA_DIR={}\nexec {} --user-data-dir={}\n",
                shell(&profile.to_string_lossy()),
                shell(&desktop.as_ref().unwrap().to_string_lossy()),
                shell(&profile.to_string_lossy())
            )
        }
        _ => {
            let patch = json!([{"id":"llm-pi-ai","config":{"providers":{"modelcat":{"displayName":"ModelCat","apiKeyEnv":"MODELCAT_GATEWAY_KEY","api":"openai-completions","baseURL":base,"models":ids.iter().map(|id|json!({"id":id,"name":id,"input":["text"]})).collect::<Vec<_>>()}}}},{"id":"agent-default-model","config":{"provider":"modelcat","model":ids[0]}}]);
            write(
                &profile.join("cordis.patch.yml"),
                &serde_json::to_vec_pretty(&patch)?,
                0o600,
            )?;
            format!("#!/bin/sh\nset -eu\nexport DSH_HOME={}\nexport MODELCAT_GATEWAY_KEY=$(cat {})\nexport PATH={}\":/opt/homebrew/bin:/usr/local/bin:$PATH\"\ncd {}\nexec npx --yes @deepseek-ai/dsh web\n",shell(&profile.to_string_lossy()),shell(&stored_key.to_string_lossy()),shell(&node_bin.as_ref().map(|p|p.to_string_lossy().to_string()).unwrap_or_default()),shell(&home.to_string_lossy()))
        }
    };
    let launch_path = root.join("launch.sh");
    write(&launch_path, launcher.as_bytes(), 0o700)?;
    let app_name = format!(
        "ModelCat {} {}.app",
        match client {
            "codex" => "Codex",
            "claude" => "Claude",
            _ => "DSH",
        },
        id
    );
    let applications = std::env::var_os("MODELCAT_APPLICATIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Applications"));
    let app = applications.join(app_name);
    let info = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>ai.opendot.modelcat.{client}.{id}</string><key>CFBundleName</key><string>ModelCat {client}</string><key>CFBundleExecutable</key><string>launcher</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>");
    write(&app.join("Contents/Info.plist"), info.as_bytes(), 0o600)?;
    write(
        &app.join("Contents/MacOS/launcher"),
        format!(
            "#!/bin/sh\nexec {}\n",
            shell(&launch_path.to_string_lossy())
        )
        .as_bytes(),
        0o700,
    )?;
    println!(
        "ModelCat {client}: 已更新 {} 个模型。入口：{}",
        ids.len(),
        app.display()
    );
    if !no_launch {
        #[cfg(target_os = "macos")]
        {
            ensure!(
                Command::new("open").arg(&app).status()?.success(),
                "Could not open app"
            );
        }
    }
    Ok(())
}
