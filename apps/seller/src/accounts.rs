use crate::{
    keychain, oauth,
    state::{now, App},
};
use anyhow::{ensure, Result};
use asale_client_core::{
    cli_import,
    executor::{LeasedToken, TaskOutcome, TokenProvider},
    pool::{AccountPool, UpstreamErrorKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub token_ref: String,
    pub refresh_ref: String,
    pub enabled: bool,
    pub expires_at: Option<i64>,
    pub upstream_account_id: Option<String>,
    pub upstream_base: Option<String>,
    pub origin: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_checked_at: Option<i64>,
    #[serde(default)]
    pub discovery_error: String,
}

impl App {
    pub async fn import_cli(&self, provider: &str) -> Result<usize> {
        if provider == "kiro" {
            return crate::kiro::import_local(self).await;
        }
        ensure!(
            matches!(provider, "claude" | "codex"),
            "unsupported subscription provider"
        );
        let provider = provider.to_string();
        let found =
            tokio::task::spawn_blocking(move || crate::cli_scan::load_all(&provider)).await?;
        ensure!(
            !found.is_empty(),
            "no CLI login found; sign in with the vendor CLI or use OAuth"
        );
        let mut count = 0;
        for item in found {
            self.persist_credential(item.cred, "imported", None).await?;
            count += 1;
        }
        Ok(count)
    }
    pub async fn persist_credential(
        &self,
        cred: cli_import::CliCred,
        origin: &str,
        base: Option<String>,
    ) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let id = cred.account_hint.clone().unwrap_or_else(|| {
            format!("{}-{}", cred.provider, cli_import::token_fingerprint(&cred))
        });
        let reference = keychain::token_ref(&cred.provider, &id);
        let refresh_ref = keychain::refresh_ref(&cred.provider, &id);
        keychain::set(&reference, &cred.access_token)?;
        if let Some(refresh) = &cred.refresh_token {
            keychain::set(&refresh_ref, refresh)?;
        }
        let account = Account {
            id: id.clone(),
            provider: cred.provider,
            label: id,
            token_ref: reference,
            refresh_ref,
            enabled: true,
            expires_at: cred.expires_at,
            upstream_account_id: cred.upstream_account_id,
            upstream_base: base,
            origin: origin.into(),
            last_error: String::new(),
            models: vec![],
            models_checked_at: None,
            discovery_error: String::new(),
        };
        let mut accounts = self.accounts.read().await.clone();
        if let Some(old) = accounts
            .iter_mut()
            .find(|a| a.id == account.id && a.provider == account.provider)
        {
            *old = account;
        } else {
            accounts.push(account);
        }
        self.save_accounts(accounts).await
    }
    pub async fn begin_oauth(self: &Arc<Self>, provider: &str) -> Result<Value> {
        ensure!(
            matches!(provider, "claude" | "codex"),
            "unsupported provider"
        );
        ensure!(self.flows.read().await.len() < 8, "too many active flows");
        let p =
            oauth::provider(provider).ok_or_else(|| anyhow::anyhow!("provider not configured"))?;
        let (url, future) = oauth::begin(&p).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.submitters
            .write()
            .await
            .insert(id.clone(), future.submitter());
        self.flows
            .write()
            .await
            .insert(id.clone(), json!({"status":"pending"}));
        let app = self.clone();
        let flow = id.clone();
        let provider = provider.to_string();
        tokio::spawn(async move {
            let result: Result<()> = async {
                let code = tokio::time::timeout(std::time::Duration::from_secs(300), future.wait())
                    .await??;
                let tokens = oauth::exchange(&p, &code).await?;
                let access = tokens["access_token"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("provider did not return an access token"))?;
                let metadata =
                    cli_import::jwt_claims(tokens["id_token"].as_str().unwrap_or(access))
                        .unwrap_or_default();
                let cred = cli_import::CliCred {
                    provider: provider.clone(),
                    access_token: access.into(),
                    refresh_token: tokens["refresh_token"].as_str().map(String::from),
                    expires_at: tokens["expires_in"].as_i64().map(|v| now() + v),
                    account_hint: metadata["email"].as_str().map(String::from),
                    plan: None,
                    upstream_account_id: metadata["https://api.openai.com/auth"]
                        ["chatgpt_account_id"]
                        .as_str()
                        .map(String::from),
                };
                app.persist_credential(cred, "oauth", None).await
            }
            .await;
            app.submitters.write().await.remove(&flow);
            let value = match result {
                Ok(()) => json!({"status":"ok"}),
                Err(e) => {
                    tracing::warn!("provider login failed");
                    json!({"status":"error","error":e.to_string()})
                }
            };
            app.flows.write().await.insert(flow, value);
        });
        Ok(json!({"flow_id":id,"auth_url":url}))
    }
    pub async fn set_account_enabled(&self, provider: &str, id: &str, enabled: bool) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let mut accounts = self.accounts.read().await.clone();
        let a = accounts
            .iter_mut()
            .find(|a| a.id == id && a.provider == provider)
            .ok_or_else(|| anyhow::anyhow!("unknown account"))?;
        a.enabled = enabled;
        self.save_accounts(accounts).await
    }
    pub async fn refresh_accounts(&self) {
        let accounts = self.accounts.read().await.clone();
        for a in accounts {
            if a.provider == "custom"
                || a.provider == "kiro"
                || a.expires_at.is_none_or(|exp| exp > now() + 300)
            {
                continue;
            }
            let result:Result<()>=async {
                let refresh=keychain::get(&a.refresh_ref)?.ok_or_else(||anyhow::anyhow!("refresh token unavailable; reconnect account"))?;
                let p=oauth::provider(&a.provider).ok_or_else(||anyhow::anyhow!("unknown provider"))?;
                let http=asale_client_core::http::upstream();
                let params=json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":p.client_id});
                let req=http.post(p.token_url).timeout(std::time::Duration::from_secs(30));
                let res=if p.anthropic_style{req.json(&params).send().await?}else{req.form(&params).send().await?};
                ensure!(res.status().is_success(),"provider rejected token refresh");
                let tokens:Value=res.json().await?;
                let access=tokens["access_token"].as_str().filter(|s|!s.is_empty()).ok_or_else(||anyhow::anyhow!("refresh returned no access token"))?;
                let _guard=self.mutation.lock().await;
                keychain::set(&a.token_ref,access)?;
                if let Some(r)=tokens["refresh_token"].as_str(){keychain::set(&a.refresh_ref,r)?;}
                let mut current=self.accounts.read().await.clone();
                if let Some(row)=current.iter_mut().find(|x|x.id==a.id&&x.provider==a.provider){row.expires_at=Some(now()+tokens["expires_in"].as_i64().unwrap_or(3600));row.last_error.clear();}
                self.save_accounts(current).await
            }.await;
            if result.is_err() {
                let _guard = self.mutation.lock().await;
                let mut current = self.accounts.read().await.clone();
                if let Some(row) = current
                    .iter_mut()
                    .find(|x| x.id == a.id && x.provider == a.provider)
                {
                    row.last_error =
                        "Token refresh failed; check network or reconnect the account".into();
                }
                let _ = self.save_accounts(current).await;
            }
        }
    }
    pub async fn available_models(&self) -> Vec<String> {
        let cfg = self.config.read().await;
        if cfg.paused {
            return vec![];
        }
        let pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        cfg.offers
            .iter()
            .filter(|o| o.enabled && pool.any_sellable(&o.provider, &o.upstream_model, now()))
            .map(|o| o.model.clone())
            .collect()
    }
}

pub struct Tokens {
    pub pool: Arc<Mutex<AccountPool>>,
}
impl TokenProvider for Tokens {
    fn acquire(&self, provider: &str, model: &str) -> Option<LeasedToken> {
        let mut pool = self.pool.lock().ok()?;
        let account = pool.pick_for_sale(provider, model, None, now())?;
        let token = match keychain::get(&account.keychain_ref).ok().flatten() {
            Some(t) => t,
            None => {
                pool.on_error(
                    provider,
                    &account.account_id,
                    model,
                    UpstreamErrorKind::AuthFailed,
                    "credential unavailable",
                    now(),
                );
                return None;
            }
        };
        let session_key = format!("session:{}:{}", provider, account.account_id);
        let session = keychain::get(&session_key)
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                let s = uuid::Uuid::new_v4().to_string();
                let _ = keychain::set(&session_key, &s);
                s
            });
        Some(LeasedToken {
            token,
            account_id: account.account_id,
            session_id: Some(session),
            upstream_account_id: account.upstream_account_id,
            upstream_base: account.upstream_base,
            upstream_wire: account.upstream_wire,
            upstream_model: account.upstream_model,
        })
    }
    fn saturated(&self, provider: &str, model: &str) -> bool {
        self.pool
            .lock()
            .is_ok_and(|p| p.lane_saturated(provider, model, None, now()))
    }
    fn has_alternate(&self, provider: &str, model: &str, except: &str) -> bool {
        self.pool
            .lock()
            .is_ok_and(|p| p.alternate_available(provider, model, except, now()))
    }
    fn report(&self, provider: &str, id: &str, model: &str, outcome: TaskOutcome) {
        let Ok(mut pool) = self.pool.lock() else {
            return;
        };
        match outcome {
            TaskOutcome::Success { tokens_used } => {
                pool.on_success(provider, id, model, tokens_used)
            }
            TaskOutcome::HandedOff => pool.on_handoff(provider, id, model, now()),
            other => {
                let kind = match other {
                    TaskOutcome::AuthFailed => UpstreamErrorKind::AuthFailed,
                    TaskOutcome::Blocked => UpstreamErrorKind::Blocked,
                    TaskOutcome::Unsupported => UpstreamErrorKind::Unsupported,
                    TaskOutcome::ClientUnsupported => UpstreamErrorKind::ClientUnsupported,
                    TaskOutcome::RateLimited { reset_at }
                    | TaskOutcome::QuotaExhausted { reset_at } => {
                        UpstreamErrorKind::RateLimited { reset_at }
                    }
                    _ => UpstreamErrorKind::ServerError,
                };
                pool.on_error(provider, id, model, kind, "upstream request failed", now());
            }
        }
    }
}

pub fn validate_custom_base(base: &str, allow_loopback: bool) -> Result<()> {
    let u = reqwest::Url::parse(base)?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    ensure!(
        u.scheme() == "https" || (allow_loopback && local && u.scheme() == "http"),
        "custom upstream requires HTTPS (loopback HTTP requires explicit development option)"
    );
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "invalid upstream URL"
    );
    Ok(())
}
