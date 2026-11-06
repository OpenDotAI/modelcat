//! Read the provider's model catalogue using the imported account, never a static list.
use crate::{
    accounts::Account,
    keychain,
    state::{now, App},
};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
impl App {
    pub async fn discover_models(&self) -> Result<Value> {
        let snapshot = self.accounts.read().await.clone();
        let mut results = vec![];
        for a in snapshot.iter().filter(|a| a.enabled) {
            let discovered = discover(a).await;
            let _guard = self.mutation.lock().await;
            let mut accounts = self.accounts.read().await.clone();
            if let Some(row) = accounts
                .iter_mut()
                .find(|r| r.id == a.id && r.provider == a.provider)
            {
                match discovered {
                    Ok(mut models) => {
                        models.sort();
                        models.dedup();
                        row.models = models;
                        row.models_checked_at = Some(now());
                        row.discovery_error.clear()
                    }
                    Err(_) => {
                        row.discovery_error = "模型清单读取失败，请检查账号与网络后重试".into()
                    }
                }
                results.push(json!({"account_id":row.id,"provider":row.provider,"models":row.models,"checked_at":row.models_checked_at,"error":row.discovery_error}));
            }
            self.save_accounts(accounts).await?;
        }
        Ok(json!({"accounts":results}))
    }
}
async fn discover(a: &Account) -> Result<Vec<String>> {
    let token =
        keychain::get(&a.token_ref)?.ok_or_else(|| anyhow::anyhow!("credential unavailable"))?;
    if a.provider == "codex" {
        return asale_client_core::discovery::codex_servable_models(
            &token,
            a.upstream_account_id.as_deref().unwrap_or(""),
        )
        .await;
    }
    if a.provider == "kiro" {
        return crate::kiro::models(&token, &a.token_ref).await;
    }
    if a.provider == "custom" {
        return asale_client_core::discovery::custom_endpoint_models(
            a.upstream_base.as_deref().unwrap_or(""),
            &token,
            asale_protocol::ids::Wire::Responses,
        )
        .await;
    }
    ensure!(a.provider == "claude", "unknown provider");
    let http = asale_client_core::http::upstream();
    let mut result = vec![];
    let mut after = String::new();
    for _ in 0..20 {
        let mut req = http
            .get("https://api.anthropic.com/v1/models")
            .bearer_auth(&token)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "oauth-2025-04-20")
            .query(&[("limit", "100")])
            .timeout(std::time::Duration::from_secs(20));
        if !after.is_empty() {
            req = req.query(&[("after_id", &after)])
        }
        let resp = req.send().await?;
        ensure!(resp.status().is_success(), "model discovery rejected");
        let v: Value = resp.json().await?;
        let rows = v["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("model list missing"))?;
        result.extend(
            rows.iter()
                .filter_map(|r| r["id"].as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
        if v["has_more"] != true {
            break;
        }
        let next = v["last_id"].as_str().unwrap_or("");
        ensure!(
            !next.is_empty() && next != after,
            "invalid model pagination"
        );
        after = next.into();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovered_ids_gate_accounts_and_survive_restart() {
        crate::testenv::with_temp_data_dir("model-discovery", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async{
 use tokio::{net::TcpListener,io::{AsyncReadExt,AsyncWriteExt}};
 let server=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=server.local_addr().unwrap();
 let task=tokio::spawn(async move{let(mut socket,_)=server.accept().await.unwrap();let mut buf=[0;4096];let n=socket.read(&mut buf).await.unwrap();let req=String::from_utf8_lossy(&buf[..n]);assert!(req.starts_with("GET /v1/models"));assert!(req.contains("Bearer fixture-only"));let body=r#"{"data":[{"id":"fixture-a"},{"id":"fixture-b"}]}"#;socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap()});
 let app=App::open().await.unwrap();app.persist_credential(asale_client_core::cli_import::CliCred{provider:"custom".into(),access_token:"fixture-only".into(),refresh_token:None,expires_at:None,account_hint:Some("fixture".into()),plan:None,upstream_account_id:None},"fixture",Some(format!("http://{address}/v1"))).await.unwrap();
 app.discover_models().await.unwrap();task.await.unwrap();
 let accounts=app.accounts.read().await.clone();assert_eq!(accounts[0].models,vec!["fixture-a","fixture-b"]);
 let mut cfg=app.config.read().await.clone();cfg.paused=false;for model in ["fixture-a","absent"]{cfg.offers.push(crate::config::Offer{model:model.into(),provider:"custom".into(),upstream_model:model.into(),input_price_per_m:"1".into(),output_price_per_m:"2".into(),enabled:true})};app.save_config(cfg).await.unwrap();assert_eq!(app.available_models().await,vec!["fixture-a"]);
 let restarted=App::open().await.unwrap();assert_eq!(restarted.accounts.read().await[0].models,vec!["fixture-a","fixture-b"]);
 })
        });
    }
}
