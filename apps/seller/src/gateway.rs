use crate::{
    config::Binding,
    runner::{self, SignedJob},
    state::{now, App},
};
use anyhow::{ensure, Result};
use asale_client_core::protocol::Envelope;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

pub async fn supervise(app: Arc<App>) {
    let mut backoff = 1;
    loop {
        if app.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            app.runtime.write().await.connection = "drained".into();
            return;
        }
        let binding = app.binding.read().await.clone();
        let Some(binding) = binding else {
            app.runtime.write().await.connection = "unbound".into();
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        app.runtime.write().await.connection = "connecting".into();
        let result = connect(app.clone(), &binding).await;
        if app.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            app.runtime.write().await.connection = "drained".into();
            return;
        }
        {
            let mut rt = app.runtime.write().await;
            rt.connection = "reconnecting".into();
            if result.is_err() {
                rt.last_error =
                    "GW connection closed or unavailable; check binding and network".into();
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(15);
        if result.is_ok() {
            backoff = 1;
        }
    }
}
async fn connect(app: Arc<App>, binding: &Binding) -> Result<()> {
    binding.validate()?;
    let mut url = reqwest::Url::parse(&binding.gateway_url)?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| anyhow::anyhow!("invalid GW scheme"))?;
    url.set_path("/market/node/connect");
    url.query_pairs_mut()
        .append_pair("node_id", &binding.node_id);
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", binding.node_secret))?,
    );
    let (socket, _) =
        tokio::time::timeout(Duration::from_secs(15), connect_async(request)).await??;
    let (mut writer, mut reader) = socket.split();
    let public_ip = discover_public_ip().await;
    {
        let mut rt = app.runtime.write().await;
        rt.connection = "online".into();
        rt.connected_at = now();
        rt.last_error.clear();
    }
    let (out, mut rx) = mpsc::channel::<Envelope>(64);
    let (completed, mut done) = mpsc::channel::<String>(64);
    let mut active: HashMap<String, Option<oneshot::Sender<()>>> = HashMap::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let result:Result<()>=async {
        loop{tokio::select!{
            _=app.network_stop.notified()=>{}
            _=heartbeat.tick()=>{
                let current=app.binding.read().await.clone();
                ensure!(current.as_ref().is_some_and(|b|b.node_id==binding.node_id&&b.gateway_url==binding.gateway_url&&b.node_secret==binding.node_secret),"binding changed");
                let cfg=app.config.read().await.clone();
                let data=json!({"id":"","type":"heartbeat","payload":{"models":app.available_models().await,"paused":cfg.paused,"version":option_env!("MODELCAT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),"public_ip":public_ip,"output_budget_ceiling":1048576}}).to_string();
                app.runtime.write().await.gw_bytes_out+=data.len()as u64;
                writer.send(Message::Text(data)).await?;
            }
            Some(id)=done.recv()=>{active.remove(&id);}
            Some(event)=rx.recv()=>{
                let data=serde_json::to_string(&event)?;
                app.runtime.write().await.gw_bytes_out+=data.len()as u64;
                writer.send(Message::Text(data)).await?;
            }
            message=reader.next()=>{
                let message=message.ok_or_else(||anyhow::anyhow!("GW disconnected"))??;
                match message {
                    Message::Text(data)=>{
                        ensure!(data.len()<=16<<20,"GW frame too large");
                        app.runtime.write().await.gw_bytes_in+=data.len()as u64;
                        let frame:Value=serde_json::from_str(&data)?;
                        let id=frame["id"].as_str().unwrap_or("").to_string();
                        match frame["type"].as_str().unwrap_or(""){
                            "job"=>{
                                if active.len()>=app.config.read().await.concurrency||active.contains_key(&id){
                                    let _=out.send(Envelope::with_id(&id,"error",json!({"code":"JOB_REJECTED","message":"node busy"}))).await;continue
                                }
                                let job:SignedJob=serde_json::from_value(frame["payload"].clone())?;
                                match runner::verify_job(&app,&id,job).await{
                                    Ok((payload,verifier))=>{
                                        let (cancel,cancel_rx)=oneshot::channel();active.insert(id.clone(),Some(cancel));
                                        let app=app.clone();let tx=out.clone();let done=completed.clone();
                                        tokio::spawn(async move{runner::execute(app,payload,verifier,tx,cancel_rx).await;let _=done.send(id).await;});
                                    }
                                    Err(_)=>{let _=out.send(Envelope::with_id(&id,"error",json!({"code":"JOB_REJECTED","message":"invalid, replayed or unavailable job"}))).await;}
                                }
                            }
                            "cancel"=>{if let Some(cancel)=active.get_mut(&id).and_then(Option::take){let _=cancel.send(());}}
                            "heartbeat.ack"=>{}
                            _=>{}
                        }
                    }
                    Message::Ping(data)=>writer.send(Message::Pong(data)).await?,
                    Message::Close(_)=>break,
                    _=>{}
                }
            }
        }
        if app.stopping.load(std::sync::atomic::Ordering::SeqCst)&&active.is_empty()&&rx.is_empty(){writer.close().await?;break}
        }
        Ok(())
    }.await;
    for (_, cancel) in active {
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
    }
    result
}

pub async fn submit_quote(app: &App, offer: &crate::config::Offer) -> Result<Value> {
    offer.validate()?;
    let b = app
        .binding
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("bind node first"))?;
    let resp=asale_client_core::http::plain().post(format!("{}/market/api/node/quotes",b.gateway_url.trim_end_matches('/')))
        .bearer_auth(&b.node_secret).header("X-Node-ID",&b.node_id).timeout(Duration::from_secs(15))
        .json(&json!({"model":offer.model,"provider":offer.provider,"upstream_model":offer.upstream_model,"input_price_per_m":offer.input_price_per_m,"output_price_per_m":offer.output_price_per_m})).send().await?;
    ensure!(
        resp.status().is_success(),
        "GW rejected quote; check node binding and price"
    );
    Ok(resp.json().await?)
}

pub async fn account(app: &App) -> Result<Value> {
    let b = app
        .binding
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("bind node first"))?;
    let resp = asale_client_core::http::plain()
        .get(format!(
            "{}/market/api/node/account",
            b.gateway_url.trim_end_matches('/')
        ))
        .bearer_auth(&b.node_secret)
        .header("X-Node-ID", &b.node_id)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    ensure!(resp.status().is_success(), "GW account unavailable");
    Ok(resp.json().await?)
}

// No provider credentials are sent. Fixture runs never contact an external service.
async fn discover_public_ip() -> Option<String> {
    if let Ok(raw) = std::env::var("MODELCAT_PUBLIC_IP") {
        if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
            if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast() {
                return Some(ip.to_string());
            }
        }
    }
    if std::env::var("MODELCAT_DISABLE_IP_DISCOVERY").is_ok_and(|s| s == "1") {
        return None;
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let response = client.get("https://api.ipify.org").send().await.ok()?;
    if !response.status().is_success() || response.content_length().is_some_and(|n| n > 64) {
        return None;
    }
    let text = response.text().await.ok()?;
    let ip: std::net::IpAddr = text.trim().parse().ok()?;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return None;
    }
    Some(ip.to_string())
}
