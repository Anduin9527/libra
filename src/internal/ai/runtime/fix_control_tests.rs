use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{Json, Router, extract::State, routing::post};

use super::*;

#[test]
fn interaction_id_is_encoded_as_one_path_segment() {
    let base = Url::parse("http://127.0.0.1:8080/control").expect("test loopback URL must parse");
    let endpoint = interaction_endpoint(&base, "approval/../next?decision=deny")
        .expect("HTTP URL accepts path segments");
    assert_eq!(
        endpoint.as_str(),
        "http://127.0.0.1:8080/control/api/code/interactions/approval%2F..%2Fnext%3Fdecision=deny"
    );
}

#[derive(Clone)]
struct RenewalState {
    calls: Arc<AtomicUsize>,
    token: &'static str,
}

async fn renew_controller_handler(State(state): State<RenewalState>) -> Json<serde_json::Value> {
    state.calls.fetch_add(1, Ordering::Relaxed);
    Json(serde_json::json!({ "controllerToken": state.token }))
}

async fn renewal_client(token: &'static str) -> (ReviewFixControlClient, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/api/code/controller/attach",
            post(renew_controller_handler),
        )
        .with_state(RenewalState {
            calls: Arc::clone(&calls),
            token,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind renewal server");
    let address = listener.local_addr().expect("renewal server address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("renewal server remains healthy");
    });
    (
        ReviewFixControlClient {
            client: Client::builder().no_proxy().build().expect("test client"),
            base_url: Url::parse(&format!("http://{address}")).expect("test control URL parses"),
            control_token: "control-token".to_string(),
            controller_token: RwLock::new("controller-token".to_string()),
            client_id: "review-fix-test".to_string(),
        },
        calls,
    )
}

#[tokio::test]
async fn renewal_preserves_the_existing_controller_token() {
    let (client, calls) = renewal_client("controller-token").await;
    client
        .renew_controller()
        .await
        .expect("same-client lease renewal succeeds");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(client.controller_token().await, "controller-token");
}

#[tokio::test]
async fn renewal_fails_closed_after_an_unexpected_token_replacement() {
    let (client, calls) = renewal_client("replacement-token").await;
    let error = client
        .renew_controller()
        .await
        .expect_err("replacement token means the old lease expired");
    assert_eq!(error, ReviewFixBridgeError::ExecutionFailed);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(client.controller_token().await, "replacement-token");
}
