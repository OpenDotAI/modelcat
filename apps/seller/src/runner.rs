use crate::{
    accounts::Tokens,
    connections::InFlight,
    state::{now, App, FailureAt, LocalRecord},
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
    let mut payload: HttpRequestPayload = serde_json::from_slice(&request)?;
    ensure!(
        payload.task_id == id
            && payload.id == id
            && payload.exp >= now()
            && payload.exp <= now() + 300,
        "invalid task identity or expiry"
    );
    ensure!(
        payload.budget_tokens > 0 && payload.budget_tokens <= 1048576,
        "invalid budget"
    );
    let _guard = app.mutation.lock().await;
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
    ensure!(
        app.offer_source_allowed(&payload.upstream.provider, &payload.model).await,
        "offer is unavailable in this workspace"
    );
    if crate::ollama::sale_model(app, &payload.upstream.provider, &payload.model).await.is_some() {
        crate::ollama::prepare_sale(&mut payload)?;
    }
    if app.vm_workspace && payload.upstream.provider == "claude" && !crate::connections::is_model_id(&payload.model) {
        crate::claude_api::prepare_sale(&mut payload)?;
    }
    // SQLite survives restarts; checking only in-memory IDs would permit a replay after restart.
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
    route: LocalRecord,
    claude_revision: Option<String>,
    started: std::time::Instant,
}
#[async_trait]
impl RecordSink for Sink {
    async fn record(
        &self,
        id: &str,
        provider: &str,
        _account: &str,
        _model: &str,
        usage: &Usage,
        status: &str,
    ) {
        if let Some(revision) = self.claude_revision.as_deref().filter(|_| {
            matches!(status, "upstream_error" | "upstream_redirect" | "stream_error")
        }) {
            if let Err(error) = crate::claude_api::pause_source(&self.app, revision).await {
                tracing::error!("could not persist Claude network pause: {error}");
            }
        }
        self.app
            .add_record(LocalRecord {
                id: id.into(),
                status: status.into(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_tokens: usage.cache_read_tokens + usage.cache_write_tokens,
                at: now(),
                source_label: self.route.source_label.clone().or_else(|| Some(provider.into())),
                duration_ms: Some(self.started.elapsed().as_millis() as u64),
                ..self.route.clone()
            })
            .await;
    }
}

pub async fn execute(
    app: Arc<App>,
    mut payload: HttpRequestPayload,
    verifier: QuotaVerifier,
    out: mpsc::Sender<Envelope>,
    cancel: oneshot::Receiver<()>,
    gateway: bool,
) {
    let id = payload.task_id.clone();
    let model = payload.model.clone();
    if !app.offer_source_allowed(&payload.upstream.provider, &model).await {
        let _ = out.send(Envelope::with_id(&id, "error",
            json!({"code":"JOB_REJECTED","message":"offer is unavailable in this workspace"}))).await;
        return;
    }
    let body_len = B64
        .decode(&payload.upstream.body_b64)
        .map(|b| b.len())
        .unwrap_or(0) as u64;
    let connection = if crate::connections::is_model_id(&model) {
        match crate::connections::sale_route(&app, &mut payload).await {
            Ok(route) => Some(route),
            Err(error) => {
                let _ = out.send(Envelope::with_id(&id, "error",
                    json!({"code":"JOB_REJECTED","message":error.to_string()}))).await;
                return;
            }
        }
    } else { None };
    let claude = if app.vm_workspace && payload.upstream.provider == "claude" && !crate::connections::is_model_id(&payload.model) {
        match crate::claude_api::sale_route(&app, &mut payload).await {
            Ok(route) => Some(route),
            Err(error) => {
                let _ = out.send(Envelope::with_id(&id, "error",
                    json!({"code":"JOB_REJECTED","message":error.to_string()}))).await;
                return;
            }
        }
    } else {
        None
    };
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
    let ollama = crate::ollama::sale_model(&app, &payload.upstream.provider, &payload.model).await;
    let route = LocalRecord {
        id: id.clone(),
        model: connection.as_ref().map_or(model, |route| route.model.clone()),
        upstream_bytes_out: body_len,
        gateway,
        source_id: connection.as_ref().map(|route| route.id().to_owned()),
        source_label: connection.as_ref().map(|route| route.label().to_owned())
            .or_else(|| ollama.as_ref().map(|_| "Ollama".into()))
            .or_else(|| claude.as_ref().map(|_| "Claude API".into())),
        endpoint: connection.as_ref().and_then(|route| route.endpoint())
            .or_else(|| ollama.as_ref().and_then(|model| crate::ollama::origin(&model.origin).ok()).map(|url| url.authority().to_owned()))
            .or_else(|| claude.as_ref().map(|route| route.endpoint.clone())),
        ..Default::default()
    };
    let flight = (gateway && route.endpoint.is_some()).then(|| InFlight::start(&app, &route, None));
    let sink = Arc::new(Sink {
        app: app.clone(),
        route,
        claude_revision: claude.as_ref().map(|route| route.revision.clone()),
        started: std::time::Instant::now(),
    });
    let worker = tokio::spawn(async move {
        if let Some(route) = connection {
            executor::execute(
                &route.http, &route, payload, &tx, Some(sink.as_ref()), &verifier, cancel,
            ).await;
            return;
        }
        if let Some(route) = claude {
            executor::execute(
                &route.http, &route, payload, &tx, Some(sink.as_ref()), &verifier, cancel,
            ).await;
            return;
        }
        if let Some(model) = ollama {
            if let Err(error) = crate::ollama::prepare_sale(&mut payload) {
                let _ = tx.send(Envelope::with_id(&payload.task_id, "error",
                    json!({"code":"JOB_REJECTED","message":error.to_string()})));
                return;
            }
            executor::execute(
                crate::ollama::client(), &crate::ollama::SaleToken(model), payload, &tx,
                Some(sink.as_ref()), &verifier, cancel,
            ).await;
            return;
        }
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
    let mut http_status = None;
    let mut usage_known = false;
    let mut tracker = Vec::new();
    while let Some(mut event) = rx.recv().await {
        if matches!(event.msg_type.as_str(), "http_response" | "stream_start") {
            http_status = event.payload["status"].as_u64().and_then(|value| u16::try_from(value).ok());
            if let Some(flight) = &flight {
                flight.receiving();
            }
        }
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
        let _guard = app.mutation.lock().await;
        let encoded = {
            let mut records = app.records.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(record) = records.iter_mut().rev().find(|record| record.id == id) {
                record.upstream_bytes_in = bytes_in;
                record.http_status = http_status;
                record.failure_at = match (record.status.as_str(), http_status) {
                    ("stream_error", _) => Some(FailureAt::ModelRoute),
                    ("upstream_error", Some(400..=599)) => Some(FailureAt::Model),
                    _ => None,
                };
                let detail = record.clone();
                serde_json::to_string(&*records).ok().map(|raw| (raw, detail))
            } else {
                None
            }
        };
        if let Some((raw, detail)) = encoded {
            if let Err(error) = crate::token_usage::update_detail(&app.store.pool, &detail, &raw).await {
                tracing::error!("record persistence failed: {error}");
            }
        }
    }
    drop(flight);
    let mut runtime = app.runtime.write().await;
    runtime.active = runtime.active.saturating_sub(1);
}
