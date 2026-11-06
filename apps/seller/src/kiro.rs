//! Kiro sessions stay in the local encrypted credential store. Only offers cross the GW bridge.
use crate::{
    accounts::Tokens,
    keychain,
    state::{now, App},
};
use anyhow::{ensure, Result};
use asale_client_core::{
    cli_import::CliCred,
    executor::TokenProvider,
    protocol::{Envelope, HttpRequestPayload},
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default = "region")]
    region: String,
    #[serde(default)]
    profile_arn: Option<String>,
    #[serde(default)]
    auth_method: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    expires_at: Option<Value>,
}
fn region() -> String {
    "us-east-1".into()
}
impl Session {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.access_token.is_empty() && self.access_token.len() < 32000,
            "Kiro accessToken required"
        );
        ensure!(
            matches!(self.region.as_str(), "us-east-1" | "eu-central-1"),
            "Kiro region must be us-east-1 or eu-central-1"
        );
        Ok(())
    }
    fn host(&self) -> String {
        let service = if self.provider.eq_ignore_ascii_case("enterprise")
            || self.auth_method.eq_ignore_ascii_case("external_idp")
        {
            "codewhisperer"
        } else {
            "q"
        };
        format!("https://{service}.{}.amazonaws.com", self.region)
    }
    async fn refresh(&mut self) -> Result<()> {
        ensure!(
            !self.refresh_token.is_empty(),
            "Kiro session expired; import again"
        );
        let http = asale_client_core::http::upstream();
        let idc = !self.client_id.is_empty();
        let (url, body) = if idc {
            (
                format!("https://oidc.{}.amazonaws.com/token", self.region),
                json!({"clientId":self.client_id,"clientSecret":self.client_secret,"refreshToken":self.refresh_token,"grantType":"refresh_token"}),
            )
        } else {
            (
                format!(
                    "https://prod.{}.auth.desktop.kiro.dev/refreshToken",
                    self.region
                ),
                json!({"refreshToken":self.refresh_token}),
            )
        };
        let response = http
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "Kiro refresh failed; import session again"
        );
        let v: Value = response.json().await?;
        self.access_token = v["accessToken"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Kiro refresh response missing token"))?
            .into();
        if let Some(r) = v["refreshToken"].as_str() {
            self.refresh_token = r.into()
        };
        self.expires_at = Some(json!(now() + v["expiresIn"].as_i64().unwrap_or(3600)));
        Ok(())
    }
}
pub async fn import(app: &App, v: Value) -> Result<()> {
    let session: Session = serde_json::from_value(v)
        .map_err(|_| anyhow::anyhow!("invalid Kiro session JSON; accessToken is required"))?;
    session.validate()?;
    let cred = CliCred {
        provider: "kiro".into(),
        access_token: serde_json::to_string(&session)?,
        refresh_token: None,
        expires_at: None,
        account_hint: None,
        plan: None,
        upstream_account_id: None,
    };
    app.persist_credential(cred, "kiro-session", None).await
}
pub async fn import_local(app: &App) -> Result<usize> {
    let home = crate::state::home_dir().ok_or_else(|| anyhow::anyhow!("home unavailable"))?;
    let path = home.join(".aws/sso/cache/kiro-auth-token.json");
    let raw = tokio::fs::read(path)
        .await
        .map_err(|_| anyhow::anyhow!("Kiro session not found; select an exported session JSON"))?;
    ensure!(raw.len() < 128 * 1024, "session file too large");
    let v: Value =
        serde_json::from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid Kiro session file"))?;
    import(app, v).await?;
    Ok(1)
}
async fn load(raw: &str) -> Result<Session> {
    let mut s: Session =
        serde_json::from_str(raw).map_err(|_| anyhow::anyhow!("invalid saved Kiro session"))?;
    s.validate()?;
    let expiry = s.expires_at.as_ref().and_then(|v| {
        v.as_i64().or_else(|| {
            v.as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp())
        })
    });
    if expiry.is_some_and(|t| t < now() + 60) {
        s.refresh().await?
    }
    Ok(s)
}
pub async fn models(raw: &str, reference: &str) -> Result<Vec<String>> {
    let s = load(raw).await?;
    keychain::set(reference, &serde_json::to_string(&s)?)?;
    let http = asale_client_core::http::upstream();
    let mut ids = vec![];
    let mut next = String::new();
    for _ in 0..20 {
        let mut body = json!({"origin":"AI_EDITOR","maxResults":100});
        if let Some(arn) = &s.profile_arn {
            body["profileArn"] = json!(arn)
        };
        if !next.is_empty() {
            body["nextToken"] = json!(next)
        };
        let r = http
            .post(format!(
                "https://q.{}.amazonaws.com/ListAvailableModels",
                s.region
            ))
            .bearer_auth(&s.access_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await?;
        ensure!(r.status().is_success(), "Kiro model discovery failed");
        let v: Value = r.json().await?;
        let list = v["models"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Kiro model list missing"))?;
        ids.extend(
            list.iter()
                .filter_map(|m| m["modelId"].as_str())
                .filter(|id| !id.is_empty())
                .map(String::from),
        );
        let token = v["nextToken"].as_str().unwrap_or("");
        if token.is_empty() {
            break;
        };
        ensure!(token != next, "invalid Kiro pagination");
        next = token.into();
    }
    Ok(ids)
}
fn text(v: &Value) -> Result<String> {
    if let Some(s) = v.as_str() {
        return Ok(s.into());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Kiro content must be text"))?;
    let mut result = String::new();
    for item in arr {
        ensure!(
            matches!(
                item["type"].as_str(),
                Some("input_text" | "output_text" | "text")
            ),
            "Kiro adapter currently accepts text content only"
        );
        result.push_str(item["text"].as_str().unwrap_or(""))
    }
    Ok(result)
}
fn request(body: &Value, model: &str, session: &Session) -> Result<Value> {
    ensure!(
        body["tools"].as_array().is_none_or(Vec::is_empty),
        "Kiro tool calls require a native adapter; text requests are supported"
    );
    let mut messages = vec![];
    if let Some(input) = body.get("input") {
        if input.is_string() {
            messages.push(json!({"role":"user","content":input}))
        } else {
            messages = input
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid input"))?
                .clone()
        }
    } else if let Some(arr) = body["messages"].as_array() {
        messages = arr.clone()
    }
    ensure!(!messages.is_empty(), "Kiro input required");
    let mut history = vec![];
    let mut current = None;
    let instruction = body["instructions"].as_str().unwrap_or("");
    for (i, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("user");
        let mut content = text(&m["content"])?;
        if i == 0 && !instruction.is_empty() {
            content = format!("{instruction}\n\n{content}")
        };
        if role == "assistant" {
            ensure!(
                i + 1 < messages.len(),
                "Kiro request must end with a user message"
            );
            history.push(json!({"assistantResponseMessage":{"content":content}}))
        } else {
            ensure!(
                matches!(role, "user" | "system" | "developer"),
                "unsupported Kiro message type"
            );
            let user = json!({"content":content,"modelId":model,"origin":"AI_EDITOR","userInputMessageContext":{}});
            if i + 1 == messages.len() {
                current = Some(user)
            } else {
                history.push(json!({"userInputMessage":user}))
            }
        }
    }
    let mut v = json!({"conversationState":{"conversationId":uuid::Uuid::new_v4().to_string(),"chatTriggerType":"MANUAL","currentMessage":{"userInputMessage":current.ok_or_else(||anyhow::anyhow!("user message required"))?},"history":history}});
    if let Some(arn) = &session.profile_arn {
        v["profileArn"] = json!(arn)
    };
    Ok(v)
}
// AWS EventStream frames: prelude, typed headers, JSON payload, CRC. Never bill inferred usage.
#[derive(Default)]
struct Events {
    pending: Vec<u8>,
    text: String,
    usage: Option<Value>,
}
impl Events {
    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        ensure!(self.pending.len() <= 20 << 20, "Kiro event buffer exceeded");
        while self.pending.len() >= 12 {
            let size = u32::from_be_bytes(self.pending[0..4].try_into()?) as usize;
            let headers = u32::from_be_bytes(self.pending[4..8].try_into()?) as usize;
            ensure!(
                (16..=8 << 20).contains(&size) && headers <= size - 16,
                "invalid Kiro event frame"
            );
            if self.pending.len() < size {
                break;
            };
            let f = &self.pending[..size];
            ensure!(
                crc32fast::hash(&f[..8]) == u32::from_be_bytes(f[8..12].try_into()?)
                    && crc32fast::hash(&f[..size - 4])
                        == u32::from_be_bytes(f[size - 4..size].try_into()?),
                "invalid Kiro event checksum"
            );
            let mut pos = 12;
            let mut kind = String::new();
            while pos < 12 + headers {
                let len = f[pos] as usize;
                pos += 1;
                ensure!(pos + len + 3 <= 12 + headers, "invalid Kiro event header");
                let name = &f[pos..pos + len];
                pos += len;
                ensure!(f[pos] == 7, "unsupported Kiro header type");
                pos += 1;
                let n = u16::from_be_bytes(f[pos..pos + 2].try_into()?) as usize;
                pos += 2;
                ensure!(pos + n <= 12 + headers, "invalid Kiro header length");
                if name == b":event-type" {
                    kind = String::from_utf8_lossy(&f[pos..pos + n]).into_owned()
                };
                if name == b":message-type" {
                    ensure!(
                        &f[pos..pos + n] != b"exception" && &f[pos..pos + n] != b"error",
                        "Kiro returned an error event"
                    )
                };
                pos += n;
            }
            let v: Value = serde_json::from_slice(&f[12 + headers..size - 4])?;
            if kind == "assistantResponseEvent" {
                if let Some(t) = v["content"].as_str() {
                    self.text.push_str(t)
                }
            }
            if let Some(u) = v
                .get("tokenUsage")
                .or_else(|| v["messageMetadataEvent"].get("tokenUsage"))
                .or_else(|| v["metadataEvent"].get("tokenUsage"))
            {
                let output = u["outputTokens"].as_i64();
                let input = u["uncachedInputTokens"]
                    .as_i64()
                    .map(|n| {
                        n + u["cacheReadInputTokens"].as_i64().unwrap_or(0)
                            + u["cacheWriteInputTokens"].as_i64().unwrap_or(0)
                    })
                    .or_else(|| u["totalTokens"].as_i64().zip(output).map(|(t, o)| t - o));
                if let (Some(i), Some(o)) = (input, output) {
                    ensure!(i >= 0 && o >= 0, "invalid Kiro token usage");
                    self.usage =
                        Some(json!({"input_tokens":i,"output_tokens":o,"total_tokens":i+o}))
                }
            }
            ensure!(self.text.len() < 16 << 20, "Kiro response too large");
            self.pending.drain(..size);
        }
        Ok(())
    }
}
pub async fn execute(
    tokens: &Tokens,
    payload: HttpRequestPayload,
    tx: &mpsc::UnboundedSender<Envelope>,
    records: &dyn asale_client_core::executor::RecordSink,
    verifier: &asale_client_core::security::QuotaVerifier,
    mut cancel: oneshot::Receiver<()>,
) {
    let id = payload.task_id.clone();
    if verifier
        .verify(
            &id,
            &payload.model,
            payload.budget_tokens,
            payload.exp,
            &payload.quota_sig,
            now(),
        )
        .is_err()
    {
        let _ = tx.send(Envelope::with_id(
            &id,
            "error",
            json!({"code":"QUOTA_SIG_INVALID","message":"invalid quota signature"}),
        ));
        return;
    }
    let work = async {
        let lease = tokens
            .acquire("kiro", &payload.model)
            .ok_or_else(|| anyhow::anyhow!("Kiro account unavailable"))?;
        let upstream_work = async {
            let session = load(&lease.token).await?;
            keychain::set(
                &keychain::token_ref("kiro", &lease.account_id),
                &serde_json::to_string(&session)?,
            )?;
            let input: Value = serde_json::from_slice(&B64.decode(&payload.upstream.body_b64)?)?;
            let body = request(&input, &payload.model, &session)?;
            let r = asale_client_core::http::upstream()
                .post(format!("{}/generateAssistantResponse", session.host()))
                .bearer_auth(&session.access_token)
                .header("x-amzn-codewhisperer-optout", "true")
                .json(&body)
                .timeout(std::time::Duration::from_secs(300))
                .send()
                .await?;
            ensure!(r.status().is_success(), "Kiro request rejected");
            if payload.stream {
                let _ = tx.send(Envelope::with_id(
                    &id,
                    "stream_start",
                    json!({"status":200,"content_type":"text/event-stream"}),
                ));
            }
            use futures_util::StreamExt;
            let mut stream = r.bytes_stream();
            let mut events = Events::default();
            while let Some(chunk) = stream.next().await {
                let old = events.text.len();
                events.feed(&chunk?)?;
                if payload.stream && events.text.len() > old {
                    let data = format!(
                        "event: response.output_text.delta\ndata: {}\n\n",
                        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":&events.text[old..]})
                    );
                    ensure!(
                        tx.send(Envelope::with_id(
                            &id,
                            "stream_chunk",
                            json!({"data_b64":B64.encode(data)})
                        ))
                        .is_ok(),
                        "receiver closed"
                    );
                }
            }
            ensure!(events.pending.is_empty(), "truncated Kiro response");
            let usage = events.usage.clone();
            let normalized = asale_client_core::protocol::Usage {
                input_tokens: usage
                    .as_ref()
                    .and_then(|u| u["input_tokens"].as_i64())
                    .unwrap_or(0),
                output_tokens: usage
                    .as_ref()
                    .and_then(|u| u["output_tokens"].as_i64())
                    .unwrap_or(0),
                ..Default::default()
            };
            records
                .record(
                    &id,
                    "kiro",
                    &lease.account_id,
                    &payload.model,
                    &normalized,
                    if usage.is_some() {
                        "ok"
                    } else {
                        "usage_unknown"
                    },
                )
                .await;
            let mut response = json!({"id":id,"object":"response","status":"completed","model":payload.model,"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":events.text}]}]});
            if let Some(u) = &usage {
                response["usage"] = u.clone()
            }
            if payload.stream {
                let data = format!(
                    "event: response.completed\ndata: {}\n\n",
                    json!({"type":"response.completed","response":response})
                );
                let _ = tx.send(Envelope::with_id(
                    &id,
                    "stream_chunk",
                    json!({"data_b64":B64.encode(data)}),
                ));
                let _=tx.send(Envelope::with_id(&id,"stream_end",json!({"usage":usage.clone().unwrap_or(json!({})),"usage_known":usage.is_some()})));
            } else {
                let _=tx.send(Envelope::with_id(&id,"http_response",json!({"status":200,"content_type":"application/json","body_b64":B64.encode(serde_json::to_vec(&response)?),"usage":usage.clone().unwrap_or(json!({})),"usage_known":usage.is_some()})));
            }
            Ok(())
        };
        let outcome: Result<()> = tokio::select! {r=upstream_work=>r,_=&mut cancel=>Err(anyhow::anyhow!("cancelled; usage unknown"))};
        tokens.report(
            "kiro",
            &lease.account_id,
            &payload.model,
            asale_client_core::executor::TaskOutcome::HandedOff,
        );
        outcome
    };
    let result: Result<()> = work.await;
    if result.is_err() {
        let _=tx.send(Envelope::with_id(&id,"error",json!({"code":"UPSTREAM_5XX","message":"Kiro request failed; check session, supported text input and connection"})));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame(kind: &str, payload: Value) -> Vec<u8> {
        let mut headers = vec![];
        let name = b":event-type";
        headers.push(name.len() as u8);
        headers.extend(name);
        headers.push(7);
        headers.extend((kind.len() as u16).to_be_bytes());
        headers.extend(kind.as_bytes());
        let body = serde_json::to_vec(&payload).unwrap();
        let size = 16 + headers.len() + body.len();
        let mut out = vec![];
        out.extend((size as u32).to_be_bytes());
        out.extend((headers.len() as u32).to_be_bytes());
        out.extend(crc32fast::hash(&out).to_be_bytes());
        out.extend(headers);
        out.extend(body);
        out.extend(crc32fast::hash(&out).to_be_bytes());
        out
    }
    #[test]
    fn fragmented_eventstream_requires_real_usage_and_valid_checksum() {
        let mut events = Events::default();
        let text = frame("assistantResponseEvent", json!({"content":"hello"}));
        for chunk in text.chunks(3) {
            events.feed(chunk).unwrap()
        }
        assert_eq!(events.text, "hello");
        assert!(events.usage.is_none());
        let meta = frame(
            "messageMetadataEvent",
            json!({"tokenUsage":{"uncachedInputTokens":10,"cacheReadInputTokens":5,"cacheWriteInputTokens":2,"outputTokens":3}}),
        );
        events.feed(&meta).unwrap();
        assert_eq!(events.usage.unwrap()["input_tokens"], 17);
        let mut corrupt = text;
        corrupt[15] ^= 1;
        assert!(Events::default().feed(&corrupt).is_err());
        assert!(Events::default().feed(&[0; 16]).is_err());
    }
    #[test]
    fn imported_session_never_appears_in_account_metadata() {
        crate::testenv::with_temp_data_dir("kiro-credential", || {
            tokio::runtime::Runtime::new().unwrap().block_on(async{
            let app=App::open().await.unwrap();import(&app,json!({"accessToken":"fixture-kiro-secret","refreshToken":"fixture-kiro-refresh","region":"us-east-1"})).await.unwrap();let accounts=app.accounts.read().await.clone();let metadata=serde_json::to_string(&accounts).unwrap();assert!(!metadata.contains("fixture-kiro-secret"));assert!(!metadata.contains("fixture-kiro-refresh"));let saved=keychain::get(&accounts[0].token_ref).unwrap().unwrap();assert!(saved.contains("fixture-kiro-secret"));
        })
        });
    }
    #[test]
    fn session_validation_rejects_arbitrary_hosts() {
        let v = json!({"accessToken":"fixture","region":"attacker.example"});
        let s: Session = serde_json::from_value(v).unwrap();
        assert!(s.validate().is_err())
    }
    #[test]
    fn text_conversion_preserves_history() {
        let s: Session = serde_json::from_value(json!({"accessToken":"fixture"})).unwrap();
        let v=request(&json!({"input":[{"role":"user","content":[{"type":"input_text","text":"one"}]},{"role":"assistant","content":"two"},{"role":"user","content":"three"}]}),"model-id",&s).unwrap();
        assert_eq!(
            v["conversationState"]["history"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            v["conversationState"]["currentMessage"]["userInputMessage"]["modelId"],
            "model-id"
        );
        assert!(request(&json!({"input":"hi","tools":[{}]}), "m", &s).is_err())
    }
}
