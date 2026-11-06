pub mod accounts;
pub mod config;
pub mod control;
pub mod desktop_install;
pub mod discovery;
pub mod gateway;
pub mod kiro;
pub mod onboarding;
pub mod runner;
pub mod state;

#[cfg(test)]
mod tests;

// Reuse the upstream implementations directly. Their source and license are pinned
// under vendor/asale; no Asale market login, wallet or auto-updater is started.
#[path = "../../../vendor/asale/daemon/src/cli_scan.rs"]
pub mod cli_scan;
#[path = "../../../vendor/asale/daemon/src/keychain.rs"]
pub mod keychain;
#[path = "../../../vendor/asale/daemon/src/oauth.rs"]
pub mod oauth;

pub mod tool_config {
    pub const TOOLS: &[&str] = &["claude", "codex"];
}

#[cfg(test)]
pub mod codex_catalog {
    pub static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
#[cfg(test)]
pub mod testenv {
    pub fn with_temp_data_dir<T>(prefix: &str, f: impl FnOnce() -> T) -> T {
        let _lock = crate::codex_catalog::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        let original = std::env::var_os("CHANNEL_NODE_DATA");
        std::env::set_var("CHANNEL_NODE_DATA", &path);
        let result = f();
        if let Some(value) = original {
            std::env::set_var("CHANNEL_NODE_DATA", value);
        } else {
            std::env::remove_var("CHANNEL_NODE_DATA");
        }
        let _ = std::fs::remove_dir_all(path);
        result
    }
}
