use crate::{
    config::{Binding, Config, Offer},
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
