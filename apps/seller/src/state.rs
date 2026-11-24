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
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_label: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_at: Option<FailureAt>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub gateway: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureAt {
    ModelRoute,
    Model,
}
#[derive(Default, Clone, Serialize)]
pub struct Runtime {
    pub connection: String,
    pub last_error: String,
    pub connected_at: i64,
    pub connecting_since: i64,
    pub retry_at: i64,
    pub active: usize,
    pub gw_bytes_in: u64,
    pub gw_bytes_out: u64,
}
pub struct App {
    pub vm_workspace: bool,
    pub(crate) pricing: RwLock<crate::pricing::Catalog>,
    pub store: Arc<LocalStore>,
    pub config: RwLock<Config>,
    pub accounts: RwLock<Vec<Account>>,
    pub ollama_models: RwLock<Vec<crate::ollama::Model>>,
    pub pool: Arc<Mutex<AccountPool>>,
    pub runtime: RwLock<Runtime>,
    pub traffic: RwLock<crate::traffic::History>,
    pub(crate) codex_device: tokio::sync::Mutex<Option<crate::connections::codex::Pending>>,
    pub(crate) quote_submission: tokio::sync::Mutex<()>,
    pub(crate) connection_setup: tokio::sync::Mutex<()>,
    pub(crate) connection_requests: Mutex<std::collections::HashMap<String, crate::connections::ActiveRequest>>,
    pub token: String,
    pub api_token: String,
    pub flows: RwLock<std::collections::HashMap<String, serde_json::Value>>,
    pub submitters: RwLock<std::collections::HashMap<String, crate::oauth::CodeSubmitter>>,
    pub binding: RwLock<Option<Binding>>,
    pub seen: Mutex<std::collections::HashMap<String, i64>>,
    pub records: Mutex<Vec<LocalRecord>>,
    pub mutation: tokio::sync::Mutex<()>,
    pub claude_setup: tokio::sync::Mutex<()>,
    #[cfg(feature = "oauth-preview")]
    pub(crate) claude_oauth_preview: tokio::sync::Mutex<Option<crate::claude_oauth_preview::Session>>,
    pub stopping: std::sync::atomic::AtomicBool,
    pub shutdown_requested: tokio::sync::Notify,
    pub network_stop: tokio::sync::Notify,
}
impl App {
    pub async fn open() -> Result<Arc<Self>> {
        Self::open_for_mode(std::env::var_os("MODELCAT_VM_WORKSPACE").is_some()).await
    }

    pub(crate) async fn open_for_mode(vm_workspace: bool) -> Result<Arc<Self>> {
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
        let ollama_models = store
            .get_setting("node.ollama_models")
            .await?
            .map(|value| serde_json::from_str(&value))
            .transpose()?
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
        let records: Vec<LocalRecord> = store
            .get_setting("node.records")
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        crate::token_usage::initialize(&store.pool, &records).await?;
        let traffic = crate::traffic::History::load(&store.pool, now()).await?;
        let (gw_bytes_in, gw_bytes_out) = traffic.totals();
        let app = Arc::new(Self {
            vm_workspace,
            pricing: Default::default(),
            store,
            config: RwLock::new(config),
            accounts: RwLock::new(accounts),
            ollama_models: RwLock::new(ollama_models),
            pool: Arc::new(Mutex::new(AccountPool::new(Strategy::RoundRobin))),
            runtime: RwLock::new(Runtime { gw_bytes_in, gw_bytes_out, ..Runtime::default() }),
            traffic: RwLock::new(traffic),
            codex_device: Default::default(),
            quote_submission: Default::default(),
            connection_setup: Default::default(),
            connection_requests: Default::default(),
            token,
            api_token,
            flows: Default::default(),
            submitters: Default::default(),
            binding: RwLock::new(binding),
            seen: Default::default(),
            records: Mutex::new(records),
            mutation: Default::default(),
            claude_setup: Default::default(),
            #[cfg(feature = "oauth-preview")]
            claude_oauth_preview: Default::default(),
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
        for model in self.ollama_models.read().await.iter() {
            let id = model.id();
            anyhow::ensure!(
                !cfg.offers.iter().any(|offer| crate::ollama::conflicts(offer, &id)),
                "offer conflicts with registered model {id}"
            );
        }
        if self.config.read().await.claude_api_proxy_setting() != cfg.claude_api_proxy_setting() {
            let _setup = self.claude_setup.lock().await;
            crate::claude_api::require_recheck()?;
            return self.persist_config(cfg).await;
        }
        self.persist_config(cfg).await
    }
    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let mut cfg = self.config.read().await.clone();
        cfg.paused = paused;
        self.persist_config(cfg).await
    }
    async fn persist_config(&self, cfg: Config) -> Result<()> {
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
            .filter(|_| !self.vm_workspace)
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
        let mut recent = self.records.lock().unwrap_or_else(|e| e.into_inner()).clone();
        recent.push(record.clone());
        let excess = recent.len().saturating_sub(1000);
        recent.drain(..excess);
        let encoded = serde_json::to_string(&recent).expect("serializable request records");
        match crate::token_usage::save(&self.store.pool, &record, &encoded).await {
            Ok(()) => *self.records.lock().unwrap_or_else(|e| e.into_inner()) = recent,
            Err(e) => tracing::error!("record persistence failed: {e}"),
        }
    }
}
