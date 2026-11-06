use crate::{
    accounts::Tokens,
    state::{now, App, LocalRecord},
};
use anyhow::{ensure, Result};
use asale_client_core::{
    executor::{self, RecordSink},
    protocol::{Envelope, HttpRequestPayload, Usage},
    security::QuotaVerifier,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

#[derive(serde::Deserialize)]
pub struct SignedJob {
    pub request_b64: String,
    pub signature: String,
    pub public_model: String,
}

pub async fn verify_job(
    app: &App,
    id: &str,
    job: SignedJob,
) -> Result<(HttpRequestPayload, QuotaVerifier)> {
    let binding = app
        .binding
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("node not bound"))?;
    let public: [u8; 32] = B64
        .decode(&binding.gateway_public_key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid public key"))?;
    let key = VerifyingKey::from_bytes(&public)?;
    let request = B64.decode(&job.request_b64)?;
    key.verify(
        &request,
        &Signature::from_slice(&B64.decode(&job.signature)?)?,
    )?;
    let payload: HttpRequestPayload = serde_json::from_slice(&request)?;
    ensure!(
        payload.task_id == id
            && payload.id == id
            && payload.exp >= now()
            && payload.exp <= now() + 300,
        "invalid task identity or expiry"
    );
    ensure!(
        payload.budget_tokens > 0 && payload.budget_tokens <= 32768,
        "invalid budget"
    );
    let cfg = app.config.read().await;
    ensure!(!cfg.paused, "node paused");
    ensure!(
        cfg.offers.iter().any(|o| o.enabled
            && o.model == job.public_model
            && o.provider == payload.upstream.provider
            && o.upstream_model == payload.model),
        "task is not an enabled model offer"
    );
    drop(cfg);
    // SQLite survives restarts; checking only in-memory IDs would permit a replay after restart.
    let _guard = app.mutation.lock().await;
    let mut accepted: std::collections::HashMap<String, i64> = app
        .store
        .get_setting("node.accepted")
        .await?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    accepted.retain(|_, exp| *exp >= now());
    ensure!(
        !accepted.contains_key(&payload.task_id),
        "task already accepted"
    );
    accepted.insert(payload.task_id.clone(), payload.exp);
    app.store
        .set_setting("node.accepted", &serde_json::to_string(&accepted)?)
        .await?;
    Ok((
        payload,
        QuotaVerifier::from_pubkey_b64(&binding.gateway_public_key)?,
    ))
}

struct Sink {
    app: Arc<App>,
    model: String,
    bytes_out: u64,
}
#[async_trait]
impl RecordSink for Sink {
    async fn record(
        &self,
        id: &str,
        _provider: &str,
        _account: &str,
        _model: &str,
        usage: &Usage,
        status: &str,
    ) {
        self.app
            .add_record(LocalRecord {
                id: id.into(),
                model: self.model.clone(),
                status: status.into(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_tokens: usage.cache_read_tokens + usage.cache_write_tokens,
                at: now(),
                upstream_bytes_out: self.bytes_out,
                ..Default::default()
            })
            .await;
    }
}

pub async fn execute(
    app: Arc<App>,
    payload: HttpRequestPayload,
    verifier: QuotaVerifier,
    out: mpsc::Sender<Envelope>,
    cancel: oneshot::Receiver<()>,
) {
    let id = payload.task_id.clone();
    let model = payload.model.clone();
    let body_len = B64
        .decode(&payload.upstream.body_b64)
        .map(|b| b.len())
        .unwrap_or(0) as u64;
    let capacity = app.config.read().await.concurrency;
    {
        let mut runtime = app.runtime.write().await;
        if runtime.active >= capacity || app.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            drop(runtime);
            let _ = out
                .send(Envelope::with_id(
                    &id,
                    "error",
                    json!({"code":"JOB_REJECTED","message":"node at capacity"}),
                ))
                .await;
            return;
        }
        runtime.active += 1;
    }
    let tokens = Arc::new(Tokens {
        pool: app.pool.clone(),
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = Arc::new(Sink {
        app: app.clone(),
        model,
        bytes_out: body_len,
    });
    let worker = tokio::spawn(async move {
        if payload.upstream.provider == "kiro" {
            crate::kiro::execute(
                tokens.as_ref(),
                payload,
                &tx,
                sink.as_ref(),
                &verifier,
                cancel,
            )
            .await;
            return;
        }
        executor::execute(
            &asale_client_core::http::upstream(),
            tokens.as_ref(),
            payload,
            &tx,
            Some(sink.as_ref()),
            &verifier,
            cancel,
        )
        .await;
    });
    let mut had_terminal = false;
    let mut bytes_in = 0u64;
    let mut usage_known = false;
    let mut tracker = Vec::new();
    while let Some(mut event) = rx.recv().await {
        if event.msg_type == "stream_chunk" {
            if let Some(raw) = event.payload["data_b64"]
                .as_str()
                .and_then(|s| B64.decode(s).ok())
            {
                bytes_in += raw.len() as u64;
                // Inspect usage across chunk boundaries without retaining a conversation.
                tracker.extend_from_slice(&raw);
                if tracker.windows(7).any(|w| w == b"\"usage\"") {
                    usage_known = true;
                }
                if tracker.len() > 32 {
                    let from = tracker.len() - 32;
                    tracker = tracker[from..].to_vec();
                }
            }
        }
        if event.msg_type == "http_response" {
            if let Some(raw) = event.payload["body_b64"]
                .as_str()
                .and_then(|s| B64.decode(s).ok())
            {
                bytes_in += raw.len() as u64;
                usage_known = serde_json::from_slice::<Value>(&raw)
                    .ok()
                    .is_some_and(|v| v.get("usage").is_some());
            }
        }
        if matches!(event.msg_type.as_str(), "stream_end" | "http_response") {
            event.payload["usage_known"] = json!(usage_known);
            had_terminal = true;
        }
        if out.send(event).await.is_err() {
            break;
        }
    }
    let _ = worker.await;
    if !had_terminal {
        // The core records partial usage on cancellation but emits no stream_end.
        // Reconcile from that record, or explicitly leave the GW reservation for review.
        let record = app
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .find(|r| r.id == id)
            .cloned();
        if let Some(r) = record {
            let event = Envelope::with_id(
                &id,
                "completion",
                json!({"usage":{"input_tokens":r.input_tokens,"output_tokens":r.output_tokens,"cache_read_tokens":r.cache_tokens,"cache_write_tokens":0},"usage_known":usage_known}),
            );
            let _ = out.send(event).await;
        }
    }
    {
        let mut records = app.records.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = records.iter_mut().rev().find(|r| r.id == id) {
            r.upstream_bytes_in = bytes_in;
        }
    }
    let encoded = {
        let records = app.records.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::to_string(&*records).ok()
    };
    if let Some(raw) = encoded {
        let _ = app.store.set_setting("node.records", &raw).await;
    }
    let mut runtime = app.runtime.write().await;
    runtime.active = runtime.active.saturating_sub(1);
}
