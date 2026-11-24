use crate::{
    config::{Binding, Config, Offer, ProxyMode},
    runner::{verify_job, SignedJob},
    state::App,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

#[test]
fn config_rejects_unsafe_gateway_and_invalid_quotes() {
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    let mut b = Binding {
        gateway_url: "http://remote.example".into(),
        node_id: "n1".into(),
        node_secret: "secret".into(),
        gateway_public_key: B64.encode(key.verifying_key().to_bytes()),
        currency: "USDT".into(),
    };
    assert!(b.validate().is_err());
    b.gateway_url = "http://127.0.0.1:3000".into();
    assert!(b.validate().is_ok());
    b.gateway_url = "https://user:password@remote.example".into();
    assert!(b.validate().is_err());
    let mut cfg = Config::default();
    cfg.offers.push(Offer {
        model: "m".into(),
        upstream_model: "vendor".into(),
        provider: "codex".into(),
        input_price_per_m: "1.0000001".into(),
        output_price_per_m: "2".into(),
        enabled: true,
    });
    assert!(cfg.validate().is_err());
    cfg.offers[0].input_price_per_m = "1.5".into();
    assert!(cfg.validate().is_ok());
    cfg.offers.push(cfg.offers[0].clone());
    assert!(cfg.validate().is_err());
}

#[test]
fn seller_quote_uses_persisted_binding_and_selected_offer() {
    use axum::{body::{to_bytes, Body}, http::{HeaderMap, Request, StatusCode}, routing::{get, post}, Json, Router};
    use serde_json::Value;
    use tower::ServiceExt;

    crate::testenv::with_temp_data_dir("seller-quote", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let gateway = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", gateway.local_addr().unwrap());
            let fixture = Router::new().route("/market/api/node/quotes", post(
                |headers: HeaderMap, Json(quote): Json<Value>| async move {
                    assert_eq!(headers["authorization"], "Bearer fixture-node-secret");
                    assert_eq!(headers["x-node-id"], "fixture-node");
                    if quote["model"] == "rejected-model" {
                        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error":"rejected"})));
                    }
                    assert_eq!(quote, json!({
                        "model": "listed-model",
                        "provider": "custom",
                        "upstream_model": "ollama/fixture-model",
                        "input_price_per_m": "1.25",
                        "output_price_per_m": "5.75"
                    }));
                    (StatusCode::CREATED, Json(json!({"id":"fixture-quote","status":"pending"})))
                }
            )).route("/market/api/node/account", get(|| async { Json(json!({"quotes":[]})) }))
                .route("/api/tags", get(|| async { Json(json!({"models":[{"name":"fixture-model"}]})) }));
            let server = tokio::spawn(async move { axum::serve(gateway, fixture).await.unwrap() });
            let app = App::open_for_mode(true).await.unwrap();
            let post_json = |path: &str, token: &str, body: Value| {
                Request::post(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap()
            };
            let signer = SigningKey::generate(&mut rand::rngs::OsRng);
            let response = crate::control::router(app.clone()).oneshot(post_json("/api/bind", &app.token, json!({
                "gateway_url": &origin,
                "node_id": "fixture-node",
                "node_secret": "fixture-node-secret",
                "gateway_public_key": B64.encode(signer.verifying_key().to_bytes()),
                "currency": "USD"
            }))).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let registered = crate::control::router(app.clone()).oneshot(post_json(
                "/api/ollama/register", &app.token, json!({"origin":origin,"model":"fixture-model"}),
            )).await.unwrap();
            assert_eq!(registered.status(), StatusCode::OK);
            let offer = Offer {
                model: "listed-model".into(), provider: "custom".into(),
                upstream_model: "ollama/fixture-model".into(), input_price_per_m: "1.25".into(),
                output_price_per_m: "5.75".into(), enabled: true,
            };
            let other = Offer { model: "unselected-model".into(), ..offer.clone() };
            let disabled = Offer { model: "disabled-model".into(), enabled: false, ..offer.clone() };
            let rejected = Offer { model: "rejected-model".into(), ..offer.clone() };
            let legacy = Offer { model: "legacy-claude".into(), provider: "claude".into(),
                upstream_model: "retired-cloud-model".into(), ..offer.clone() };
            let config = Config { offers: vec![offer, other, disabled, rejected, legacy], ..Config::default() };
            let response = crate::control::router(app.clone()).oneshot(post_json(
                "/api/config", &app.token, serde_json::to_value(config).unwrap(),
            )).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            drop(app);
            let restarted = App::open_for_mode(true).await.unwrap();
            let router = crate::control::router(restarted.clone());
            let selection = json!({"models":["listed-model","disabled-model","legacy-claude"]});
            let unauthorized = router.clone().oneshot(post_json(
                "/api/quotes/submit", &restarted.api_token, selection.clone(),
            )).await.unwrap();
            assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
            let response = router.clone().oneshot(post_json(
                "/api/quotes/submit", &restarted.token, selection,
            )).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({
                "quotes":[{"id":"fixture-quote","status":"pending"}],"skipped":0,"pending_models":[]
            }));
            let response = router.oneshot(post_json(
                "/api/quotes/submit", &restarted.token,
                json!({"models":["listed-model","rejected-model"]}),
            )).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let result: Value = serde_json::from_slice(&body).unwrap();
            assert!(result["error"].as_str().unwrap().contains("rejected-model 提交失败（已提交 1 项）"));
            assert!(restarted.config.read().await.paused);
            server.abort();
        });
    });
}

#[test]
fn vm_workspace_keeps_legacy_cloud_accounts_inactive() {
    use axum::{body::{to_bytes, Body}, http::{Request, StatusCode}};
    use tower::ServiceExt;

    crate::testenv::with_temp_data_dir("vm-legacy-cloud", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let legacy = App::open_for_mode(false).await.unwrap();
            legacy.persist_credential(
                asale_client_core::cli_import::CliCred {
                    provider: "claude".into(), access_token: "fixture-legacy-access".into(),
                    refresh_token: Some("fixture-legacy-refresh".into()),
                    expires_at: Some(crate::state::now() + 120), account_hint: Some("fixture".into()),
                    plan: None, upstream_account_id: None,
                },
                "fixture", None,
            ).await.unwrap();
            legacy.save_config(Config {
                paused: false,
                upstream_proxy: "http://127.0.0.1:1".into(),
                offers: vec![Offer {
                    model: "fixture-legacy-model".into(), provider: "claude".into(),
                    upstream_model: "fixture-legacy-model".into(), input_price_per_m: "1".into(),
                    output_price_per_m: "2".into(), enabled: true,
                }],
                ..Config::default()
            }).await.unwrap();
            assert_eq!(legacy.available_models().await, ["fixture-legacy-model"]);
            drop(legacy);

            let app = App::open_for_mode(true).await.unwrap();
            let router = crate::control::router(app.clone());
            assert!(app.available_models().await.is_empty());
            assert!(app.local_models().await.is_empty());
            assert_eq!(app.discover_models().await.unwrap(), json!({"accounts":[]}));
            app.refresh_accounts().await;
            let account = app.accounts.read().await[0].clone();
            assert!(account.last_error.is_empty());
            assert_eq!(crate::keychain::get(&account.refresh_ref).unwrap().as_deref(), Some("fixture-legacy-refresh"));

            let mut operations = vec![
                ("/api/import/kiro", &app.token, json!({"accessToken":"fixture-ignored"}), StatusCode::BAD_REQUEST),
                ("/v1/messages", &app.api_token, json!({"model":"fixture-legacy-model"}), StatusCode::FORBIDDEN),
            ];
            for provider in ["claude", "codex", "kiro", "custom"] {
                for path in ["/api/oauth/start", "/api/import", "/api/accounts/state"] {
                    operations.push((path, &app.token, json!({"provider":provider,"id":"fixture","enabled":true}), StatusCode::BAD_REQUEST));
                }
            }
            for (path, token, body, expected) in operations {
                let response = router.clone().oneshot(Request::post(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string())).unwrap()).await.unwrap();
                assert_eq!(response.status(), expected);
                let data = to_bytes(response.into_body(), 4096).await.unwrap();
                assert!(String::from_utf8_lossy(&data).contains("Claude API Key"));
            }
        });
    });
}

#[test]
fn signed_job_is_bound_to_payload_model_and_persistent_replay_guard() {
    crate::testenv::with_temp_data_dir("channel-signature", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async{
            let app=App::open().await.unwrap();let key=SigningKey::generate(&mut rand::rngs::OsRng);
            let binding=Binding{gateway_url:"http://127.0.0.1:3000".into(),node_id:"n1".into(),node_secret:"secret".into(),gateway_public_key:B64.encode(key.verifying_key().to_bytes()),currency:"USDT".into()};
            crate::keychain::set("node.binding",&serde_json::to_string(&binding).unwrap()).unwrap();*app.binding.write().await=Some(binding);
            let mut cfg=Config::default();cfg.paused=false;cfg.offers.push(Offer{model:"public".into(),provider:"codex".into(),upstream_model:"vendor".into(),input_price_per_m:"1".into(),output_price_per_m:"4".into(),enabled:true});app.save_config(cfg).await.unwrap();
            let payload=json!({"id":"task1","task_id":"task1","quota_sig":"not-used-by-outer-verifier","model":"vendor","exp":crate::state::now()+120,"budget_tokens":100,"stream":true,"upstream":{"provider":"codex","method":"POST","url":"https://chatgpt.com/backend-api/codex/responses","headers":{},"body_b64":B64.encode(b"{}")}});
            let raw=serde_json::to_vec(&payload).unwrap();let signature=B64.encode(key.sign(&raw).to_bytes());
            let job=||SignedJob{request_b64:B64.encode(&raw),signature:signature.clone(),public_model:"public".into()};
            let mut tampered=job();tampered.request_b64=B64.encode(b"{}");assert!(verify_job(&app,"task1",tampered).await.is_err());
            assert!(verify_job(&app,"task1",job()).await.is_ok());assert!(verify_job(&app,"task1",job()).await.is_err());
            let restarted=App::open().await.unwrap();assert!(verify_job(&restarted,"task1",job()).await.is_err());
            assert!(verify_job(&restarted,"other-task",job()).await.is_err());
            let mut paused = restarted.config.read().await.clone();
            paused.paused = true;
            restarted.save_config(paused).await.unwrap();
            let error = verify_job(&restarted, "task1", job()).await.unwrap_err();
            assert_eq!(error.to_string(), "node paused");
        });
    });
}

#[test]
fn local_inference_remains_available_while_platform_orders_are_paused() {
    crate::testenv::with_temp_data_dir("local-inference-paused", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use axum::{body::Body, http::Request, routing::post, Json, Router};
            use tower::ServiceExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let upstream = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new().route(
                        "/v1/messages",
                        post(|Json(body): Json<serde_json::Value>| async move {
                            assert_eq!(body["model"], "fixture-model");
                            Json(json!({
                                "id": "msg_fixture", "type": "message", "role": "assistant",
                                "model": "fixture-model", "stop_reason": "end_turn",
                                "content": [{"type": "text", "text": "fixture reply"}],
                                "usage": {"input_tokens": 2, "output_tokens": 2}
                            }))
                        }),
                    ),
                ).await.unwrap();
            });
            let app = App::open().await.unwrap();
            app.persist_credential(
                asale_client_core::cli_import::CliCred {
                    provider: "custom".into(),
                    access_token: "fixture-upstream-key".into(),
                    refresh_token: None,
                    expires_at: None,
                    account_hint: Some("fixture".into()),
                    plan: None,
                    upstream_account_id: None,
                },
                "fixture",
                Some(format!("http://{address}/v1")),
            ).await.unwrap();
            let cfg = Config {
                paused: true,
                offers: vec![Offer {
                    model: "local-fixture".into(),
                    provider: "custom".into(),
                    upstream_model: "fixture-model".into(),
                    input_price_per_m: "0".into(),
                    output_price_per_m: "0".into(),
                    enabled: true,
                }],
                ..Config::default()
            };
            app.save_config(cfg).await.unwrap();
            assert_eq!(app.local_models().await, ["local-fixture"]);
            assert!(app.available_models().await.is_empty());
            let response = crate::control::router(app.clone())
                .oneshot(Request::post("/v1/messages")
                    .header("x-api-key", &app.api_token)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({
                        "model": "local-fixture",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 32
                    }).to_string())).unwrap())
                .await.unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            upstream.abort();
            assert!(status.is_success(), "{status}: {}", String::from_utf8_lossy(&body));
            let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(result["content"][0]["text"], "fixture reply");
        });
    });
}

#[test]
fn pausing_rejects_new_gateway_jobs_and_preserves_the_inflight_response() {
    assert_gateway_pause_and_drain("legacy");
}

#[test]
fn ollama_gateway_jobs_use_registered_origin_and_obey_pause() {
    assert_gateway_pause_and_drain("ollama");
}

#[test]
fn saved_connection_gateway_jobs_record_the_selected_route() {
    assert_gateway_pause_and_drain("connection");
}

fn assert_gateway_pause_and_drain(kind: &str) {
    use axum::{body::Body, http::{HeaderMap, Request}, routing::{get, post}, Json, Router};
    use futures_util::{SinkExt, StreamExt};
    use std::{sync::Arc, time::Duration};
    use tokio::{net::TcpListener, sync::{Notify, Semaphore}, time::timeout};
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{handshake::server::{Request as Handshake, Response as Upgrade}, Message},
    };
    use tower::ServiceExt;

    crate::testenv::with_temp_data_dir("gateway-pause-drain", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            timeout(Duration::from_secs(10), async {
                let ollama = kind != "legacy";
                let connection = kind == "connection";
                let path = if connection { "/v1/chat/completions" } else { "/v1/messages" };
                let arrived = Arc::new(Notify::new());
                let release = Arc::new(Semaphore::new(0));
                let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let upstream_address = upstream.local_addr().unwrap().to_string();
                let origin = format!("http://{upstream_address}");
                let fixture = Router::new().route(path, post({
                    let arrived = arrived.clone();
                    let release = release.clone();
                    move |headers: HeaderMap, Json(request): Json<serde_json::Value>| {
                        let arrived = arrived.clone();
                        let release = release.clone();
                        async move {
                            assert_eq!(request["model"], "fixture-model");
                            if ollama {
                                assert!(!headers.contains_key("authorization"));
                                assert!(!headers.contains_key("x-api-key"));
                                if !connection {
                                    assert_eq!(headers["anthropic-version"], "2023-06-01");
                                }
                            }
                            arrived.notify_one();
                            release.acquire().await.unwrap().forget();
                            if connection {
                                return Json(json!({
                                    "id":"chat_fixture","object":"chat.completion","model":"fixture-model",
                                    "choices":[{"message":{"role":"assistant","content":"completed"}}],
                                    "usage":{"prompt_tokens":2,"completion_tokens":1}
                                }));
                            }
                            Json(json!({
                                "id":"msg_fixture","type":"message","role":"assistant",
                                "model":"fixture-model","stop_reason":"end_turn",
                                "content":[{"type":"text","text":"completed"}],
                                "usage":{"input_tokens":2,"output_tokens":1}
                            }))
                        }
                    }
                })).route("/api/tags", get(|| async { Json(json!({"models":[{"name":"fixture-model"}]})) }));
                let upstream_task = tokio::spawn(async move { axum::serve(upstream, fixture).await.unwrap() });

                let app = App::open_for_mode(ollama).await.unwrap();
                let upstream_model = if connection {
                    let response = crate::control::router(app.clone())
                        .oneshot(Request::post("/api/connections")
                            .header("authorization", format!("Bearer {}", app.token))
                            .header("content-type", "application/json")
                            .body(Body::from(json!({"label":"Fixture connection","base":origin,"key":"",
                                "protocol":"ollama"}).to_string())).unwrap())
                        .await.unwrap();
                    assert_eq!(response.status(), axum::http::StatusCode::OK);
                    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
                    let connection: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    connection["models"][0]["id"].as_str().unwrap().to_owned()
                } else if ollama {
                    crate::ollama::register(&app, crate::ollama::Model {
                        origin, name: "fixture-model".into(),
                    }).await.unwrap();
                    "ollama/fixture-model".to_string()
                } else {
                    app.persist_credential(
                    asale_client_core::cli_import::CliCred {
                        provider: "custom".into(), access_token: "fixture-key".into(),
                        refresh_token: None, expires_at: None, account_hint: Some("fixture".into()),
                        plan: None, upstream_account_id: None,
                    },
                    "fixture",
                    Some(format!("{origin}/v1")),
                    ).await.unwrap();
                    "fixture-model".to_string()
                };
                app.save_config(Config {
                    paused: false,
                    upstream_proxy: if ollama { "http://127.0.0.1:1" } else { "auto" }.into(),
                    proxy_mode: if ollama { ProxyMode::Global } else { ProxyMode::Rules },
                    offers: vec![Offer {
                        model: "public-fixture".into(), upstream_model: upstream_model.clone(),
                        provider: "custom".into(), input_price_per_m: "1".into(),
                        output_price_per_m: "1".into(), enabled: true,
                    }],
                    ..Config::default()
                }).await.unwrap();
                assert_eq!(app.available_models().await, ["public-fixture"]);

                let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let signer = SigningKey::generate(&mut rand::rngs::OsRng);
                let router = crate::control::router(app.clone());
                let post = |path: &str, body: serde_json::Value| {
                    router.clone().oneshot(Request::post(path)
                        .header("authorization", format!("Bearer {}", app.token))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string())).unwrap())
                };
                assert!(post("/api/bind", json!({
                    "gateway_url": format!("http://{}", gateway.local_addr().unwrap()),
                    "node_id":"fixture-node", "node_secret":"fixture-node-secret",
                    "gateway_public_key":B64.encode(signer.verifying_key().to_bytes()), "currency":"USDT"
                })).await.unwrap().status().is_success());
                let gateway_task = tokio::spawn(crate::gateway::supervise(app.clone()));
                let (tcp, _) = gateway.accept().await.unwrap();
                let mut peer = accept_hdr_async(tcp, |request: &Handshake, response: Upgrade| {
                    assert_eq!(request.uri().path_and_query().unwrap().as_str(), "/market/node/connect?node_id=fixture-node");
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-node-secret");
                    Ok(response)
                }).await.unwrap();
                let job = |id: &str| {
                    let expiry = crate::state::now() + 60;
                    let quota = signer.sign(format!("{id}|{upstream_model}|32|{expiry}").as_bytes());
                    let headers = if ollama {
                        json!({"authorization":"Bearer gateway-fixture","x-api-key":"gateway-fixture","content-type":"application/json"})
                    } else {
                        json!({"content-type":"application/json"})
                    };
                    let body = json!({"model":upstream_model,"messages":[{"role":"user","content":"hi"}],"max_tokens":32});
                    let request = json!({
                        "id":id, "task_id":id, "quota_sig":B64.encode(quota.to_bytes()),
                        "model":upstream_model, "exp":expiry, "budget_tokens":32, "stream":false,
                        "upstream":{"provider":"custom","method":"POST","url":format!("https://placeholder.invalid{path}"),
                            "headers":headers,"body_b64":B64.encode(body.to_string())}
                    });
                    let bytes = serde_json::to_vec(&request).unwrap();
                    Message::Text(json!({"id":id,"type":"job","payload":{
                        "request_b64":B64.encode(&bytes), "signature":B64.encode(signer.sign(&bytes).to_bytes()),
                        "public_model":"public-fixture"
                    }}).to_string())
                };
                let next_job = async |peer: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>| {
                    loop {
                        let frame = peer.next().await.unwrap().unwrap();
                        if let Message::Text(source) = frame {
                            let value: serde_json::Value = serde_json::from_str(&source).unwrap();
                            if value["id"].as_str().is_some_and(|id| !id.is_empty()) {
                                return value;
                            }
                        }
                    }
                };

                peer.send(job("accepted-before-pause")).await.unwrap();
                arrived.notified().await;
                assert_eq!(app.runtime.read().await.active, 1);
                if connection {
                    let response = crate::control::router(app.clone())
                        .oneshot(Request::get("/api/activity")
                            .header("authorization", format!("Bearer {}", app.token))
                            .body(Body::empty()).unwrap())
                        .await.unwrap();
                    let bytes = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
                    let activity: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(activity["active"], 1);
                    let trace = &activity["requests"][0];
                    assert_eq!(trace["id"], "accepted-before-pause");
                    assert_eq!(trace["phase"], "sending");
                    assert_eq!(trace["gateway"], true);
                    assert_eq!(trace["model"], "fixture-model");
                    assert_eq!(trace["endpoint"], upstream_address.as_str());
                }
                assert!(post("/api/pause", json!({"paused":true})).await.unwrap().status().is_success());
                peer.send(job("arrived-after-pause")).await.unwrap();
                let rejected = next_job(&mut peer).await;
                assert_eq!(rejected["id"], "arrived-after-pause");
                assert_eq!(rejected["payload"]["code"], "JOB_REJECTED");

                release.add_permits(1);
                let completed = next_job(&mut peer).await;
                assert_eq!(completed["id"], "accepted-before-pause");
                assert_eq!(completed["type"], "http_response");
                let body = B64.decode(completed["payload"]["body_b64"].as_str().unwrap()).unwrap();
                let body = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
                if connection {
                    assert_eq!(body["choices"][0]["message"]["content"], "completed");
                    while app.runtime.read().await.active > 0 {
                        tokio::task::yield_now().await;
                    }
                    let records = app.records.lock().unwrap();
                    let record = records.iter().find(|record| record.id == "accepted-before-pause").unwrap();
                    assert!(record.gateway);
                    assert_eq!(record.model, "fixture-model");
                    assert_eq!(record.source_label.as_deref(), Some("Fixture connection"));
                    assert_eq!(record.endpoint.as_deref(), Some(upstream_address.as_str()));
                    assert_eq!((record.input_tokens, record.output_tokens), (2, 1));
                    assert_eq!(record.http_status, Some(200));
                } else {
                    assert_eq!(body["content"][0]["text"], "completed");
                }

                assert!(post("/api/pause", json!({"paused":false})).await.unwrap().status().is_success());
                peer.send(job("accepted-after-resume")).await.unwrap();
                arrived.notified().await;
                release.add_permits(1);
                let resumed = next_job(&mut peer).await;
                assert_eq!(resumed["id"], "accepted-after-resume");
                assert_eq!(resumed["type"], "http_response");
                gateway_task.abort();
                upstream_task.abort();
            }).await.expect("local gateway integration did not finish");
        });
    });
}

#[test]
fn ollama_discovery_registration_and_stream_use_the_local_route() {
    crate::testenv::with_temp_data_dir("ollama-local-route", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use axum::{body::Body, http::{HeaderMap, Request, StatusCode}, routing::{get, post}, Json, Router};
            use tower::ServiceExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let upstream = tokio::spawn(async move {
                axum::serve(listener, Router::new()
                    .route("/api/tags", get(|| async { Json(json!({"models":[{"name":"fixture:latest"},{"name":"second:latest"}]})) }))
                    .route("/v1/messages", post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                        assert!(headers.get("authorization").is_none());
                        assert!(headers.get("x-api-key").is_none());
                        assert_eq!(body["model"], "fixture:latest");
                        assert_eq!(body["stream"], true);
                        ([("content-type", "text/event-stream")], "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
                    })))
                    .await.unwrap();
            });
            let app = App::open().await.unwrap();
            let router = crate::control::router(app.clone());
            let discovery = router.clone().oneshot(Request::post("/api/ollama/discover")
                .header("authorization", format!("Bearer {}", app.token))
                .header("content-type", "application/json")
                .body(Body::from(json!({"origin":origin}).to_string())).unwrap()).await.unwrap();
            assert_eq!(discovery.status(), StatusCode::OK);
            let body = axum::body::to_bytes(discovery.into_body(), 4096).await.unwrap();
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["models"], json!(["fixture:latest", "second:latest"]));
            let registration = router.oneshot(Request::post("/api/ollama/register")
                .header("authorization", format!("Bearer {}", app.token))
                .header("content-type", "application/json")
                .body(Body::from(json!({"origin":origin,"model":"fixture:latest"}).to_string())).unwrap()).await.unwrap();
            assert_eq!(registration.status(), StatusCode::OK);
            let restarted = App::open().await.unwrap();
            assert_eq!(restarted.local_models().await, ["ollama/fixture:latest"]);
            assert!(restarted.config.read().await.offers.is_empty());
            crate::keychain::set("node.claude_api", "invalid source record").unwrap();
            assert!(crate::claude_api::source().is_err());
            let response = crate::control::router(restarted.clone()).oneshot(Request::post("/v1/messages")
                .header("x-api-key", &restarted.api_token)
                .header("content-type", "application/json")
                .body(Body::from(json!({"model":"ollama/fixture:latest","stream":true}).to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-type"], "text/event-stream");
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(String::from_utf8_lossy(&body).contains("event: message_stop"));
            upstream.abort();
        });
    });
}

#[test]
fn local_model_key_cannot_manage_node_and_foreign_origin_is_rejected() {
    crate::testenv::with_temp_data_dir("channel-auth", || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use axum::{
                body::Body,
                http::{Request, StatusCode},
            };
            use tower::ServiceExt;
            let app = App::open().await.unwrap();
            let router = crate::control::router(app.clone());
            for (path, key, status) in [
                (
                    "/api/status",
                    app.api_token.as_str(),
                    StatusCode::UNAUTHORIZED,
                ),
                ("/v1/models", app.token.as_str(), StatusCode::UNAUTHORIZED),
                ("/v1/models", app.api_token.as_str(), StatusCode::OK),
                ("/api/status", app.token.as_str(), StatusCode::OK),
                ("/api/local-key", app.api_token.as_str(), StatusCode::UNAUTHORIZED),
                ("/api/local-key", app.token.as_str(), StatusCode::OK),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("authorization", format!("Bearer {key}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), status);
            }
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/api/status")
                        .header("authorization", format!("Bearer {}", app.token))
                        .header("host", "127.0.0.1:19700")
                        .header("origin", "https://foreign.example")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        });
    });
}
