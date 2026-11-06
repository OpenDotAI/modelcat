use crate::{
    accounts::Account,
    config::{Binding, Config},
    keychain,
};
use anyhow::Result;
use asale_client_core::{store::LocalStore, AccountPool, Strategy};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
pub fn data_dir() -> String {
    std::env::var("CHANNEL_NODE_DATA")
        .or_else(|_| std::env::var("MODELCAT_DATA"))
        .unwrap_or_else(|_| {
            let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
            let legacy = home.join(".channel-node");
            (if legacy.exists() {
                legacy
            } else {
                home.join(".modelcat")
            })
            .display()
            .to_string()
        })
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct LocalRecord {
    pub id: String,
    pub model: String,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_tokens: i64,
    pub at: i64,
    pub upstream_bytes_in: u64,
    pub upstream_bytes_out: u64,
    pub gw_bytes_in: u64,
    pub gw_bytes_out: u64,
}
#[derive(Default, Clone, Serialize)]
pub struct Runtime {
    pub connection: String,
    pub last_error: String,
    pub connected_at: i64,
    pub active: usize,
    pub gw_bytes_in: u64,
    pub gw_bytes_out: u64,
}
pub struct App {
    pub store: Arc<LocalStore>,
    pub config: RwLock<Config>,
    pub accounts: RwLock<Vec<Account>>,
    pub pool: Arc<Mutex<AccountPool>>,
    pub runtime: RwLock<Runtime>,
    pub token: String,
    pub api_token: String,
    pub flows: RwLock<std::collections::HashMap<String, serde_json::Value>>,
    pub submitters: RwLock<std::collections::HashMap<String, crate::oauth::CodeSubmitter>>,
    pub binding: RwLock<Option<Binding>>,
    pub seen: Mutex<std::collections::HashMap<String, i64>>,
    pub records: Mutex<Vec<LocalRecord>>,
    pub mutation: tokio::sync::Mutex<()>,
    pub stopping: std::sync::atomic::AtomicBool,
    pub shutdown_requested: tokio::sync::Notify,
    pub network_stop: tokio::sync::Notify,
}
impl App {
    pub async fn open() -> Result<Arc<Self>> {
        let dir = PathBuf::from(data_dir());
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let store = Arc::new(LocalStore::open(&dir.join("node.db").display().to_string()).await?);
        let config: Config = store
            .get_setting("node.config")
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        config.validate()?;
        config.apply_proxy();
        let accounts = store
            .get_setting("node.accounts")
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let binding =
            keychain::get("node.binding")?.and_then(|s| serde_json::from_str::<Binding>(&s).ok());
        if let Some(b) = &binding {
            b.validate()?;
        }
        let token = match keychain::get("node.local_token")? {
            Some(s) => s,
            None => {
                let s = uuid::Uuid::new_v4().simple().to_string()
                    + &uuid::Uuid::new_v4().simple().to_string();
                keychain::set("node.local_token", &s)?;
                s
            }
        };
        let api_token = match keychain::get("node.api_token")? {
            Some(s) => s,
            None => {
                let s = uuid::Uuid::new_v4().simple().to_string()
                    + &uuid::Uuid::new_v4().simple().to_string();
                keychain::set("node.api_token", &s)?;
                s
            }
        };
        // CLI reads this protected file without loading or printing any subscription credential.
        std::fs::write(dir.join("control.token"), &token)?;
        std::fs::write(dir.join("local-api.token"), &api_token)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("control.token"),
                std::fs::Permissions::from_mode(0o600),
            )?;
            std::fs::set_permissions(
                dir.join("local-api.token"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        let records = store
            .get_setting("node.records")
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let app = Arc::new(Self {
            store,
            config: RwLock::new(config),
            accounts: RwLock::new(accounts),
            pool: Arc::new(Mutex::new(AccountPool::new(Strategy::RoundRobin))),
            runtime: RwLock::new(Runtime::default()),
            token,
            api_token,
            flows: Default::default(),
            submitters: Default::default(),
            binding: RwLock::new(binding),
            seen: Default::default(),
            records: Mutex::new(records),
            mutation: Default::default(),
            stopping: Default::default(),
            shutdown_requested: Default::default(),
            network_stop: Default::default(),
        });
        app.rebuild().await;
        Ok(app)
    }
    pub async fn save_config(&self, cfg: Config) -> Result<()> {
        cfg.validate()?;
        let _guard = self.mutation.lock().await;
        self.store
            .set_setting("node.config", &serde_json::to_string(&cfg)?)
            .await?;
        cfg.apply_proxy();
        *self.config.write().await = cfg;
        self.rebuild().await;
        Ok(())
    }
    pub async fn drain(&self) {
        {
            let _runtime = self.runtime.write().await;
            self.stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.config.write().await.paused = true;
        self.network_stop.notify_one();
        loop {
            let runtime = self.runtime.read().await;
            if runtime.active == 0 && matches!(runtime.connection.as_str(), "drained" | "unbound") {
                break;
            }
            drop(runtime);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    pub async fn save_accounts(&self, accounts: Vec<Account>) -> Result<()> {
        self.store
            .set_setting("node.accounts", &serde_json::to_string(&accounts)?)
            .await?;
        *self.accounts.write().await = accounts;
        self.rebuild().await;
        Ok(())
    }
    pub async fn rebuild(&self) {
        let cfg = self.config.read().await;
        let accounts = self.accounts.read().await;
        let runtime = accounts
            .iter()
            .map(|a| {
                let models: Vec<_> = cfg
                    .offers
                    .iter()
                    .filter(|o| {
                        o.enabled
                            && o.provider == a.provider
                            && (a.models_checked_at.is_none()
                                || a.models.contains(&o.upstream_model))
                    })
                    .map(|o| o.upstream_model.clone())
                    .collect();
                let mut r =
                    asale_client_core::pool::AccountRuntime::new(&a.provider, &a.id, &a.token_ref)
                        .with_models(&models);
                r.sell_enabled = a.enabled;
                r.sell_models = models;
                r.expires_at = a.expires_at;
                r.upstream_account_id = a.upstream_account_id.clone();
                r.upstream_base = a.upstream_base.clone();
                r.upstream_wire = Some(asale_protocol::ids::Wire::Responses);
                r.upstream_responses = true;
                r.concurrency_max = cfg.concurrency as u32;
                r
            })
            .collect();
        self.pool
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_accounts(runtime);
    }
    pub async fn add_record(&self, record: LocalRecord) {
        let _guard = self.mutation.lock().await;
        let encoded = {
            let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
            records.push(record);
            if records.len() > 1000 {
                let excess = records.len() - 1000;
                records.drain(..excess);
            }
            serde_json::to_string(&*records).unwrap_or_default()
        };
        if let Err(e) = self.store.set_setting("node.records", &encoded).await {
            tracing::error!("record persistence failed: {e}");
        }
    }
}
