//! plan-20260912 MB-01: mega2 `/api/v1/tree` transport integration.
//!
//! End-to-end (mock server) checks: query encoding, no Authorization header,
//! valid listing, HTTP failure mapping, and zero local filesystem writes.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use libra::{
    internal::protocol::mega2_tree::{ContentType, Mega2TreeClient},
    utils::test::ChangeDirGuard,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Debug, Default, Deserialize)]
struct TreeQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    refs: String,
}

#[derive(Default)]
struct MockState {
    queries: Vec<(String, String)>,
    saw_authorization: bool,
}

/// Spawns an axum mock of the mega2 tree route; returns the bound address,
/// the captured request log, and a handle that stops on drop.
struct MockMega2Tree {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
    _handle: JoinHandle<()>,
}

impl MockMega2Tree {
    async fn start(status: u16, tree_items: serde_json::Value) -> Self {
        let state = Arc::new(Mutex::new(MockState::default()));
        let app_state = Arc::clone(&state);
        let app = Router::new()
            .route(
                "/api/v1/tree",
                get(move |Query(q): Query<TreeQuery>, headers: HeaderMap| {
                    let s = Arc::clone(&app_state);
                    async move {
                        {
                            let mut s = s.lock().expect("mock state poisoned");
                            s.queries.push((q.path.clone(), q.refs.clone()));
                            if headers.contains_key("authorization") {
                                s.saw_authorization = true;
                            }
                        }
                        let body = json!({
                            "req_result": status == 200,
                            "data": if status == 200 {
                                json!({
                                    "tree_items": tree_items,
                                    "file_tree": {"/": {"total_count": 1, "tree_items": [{"name": "ancestor", "path": "/x", "content_type": "file"}]}},
                                })
                            } else {
                                serde_json::Value::Null
                            },
                            "err_message": if status == 200 { "" } else { "mock failure" },
                        });
                        (StatusCode::from_u16(status).expect("valid status"), body.to_string())
                    }
                }),
            )
            .with_state(());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock mega2 tree");
        let addr = listener.local_addr().expect("mock mega2 tree addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            addr,
            state,
            _handle: handle,
        }
    }

    fn url(&self) -> String {
        format!("http://{addr}", addr = self.addr)
    }
}

#[tokio::test]
async fn transport_encodes_query_sends_no_auth_and_returns_sorted_listing() {
    let mock = MockMega2Tree::start(
        200,
        json!([
            {"name": "zeta-file", "path": "/", "content_type": "file"},
            {"name": "beta-dir", "path": "/", "content_type": "directory"},
            {"name": "alpha-file", "path": "/", "content_type": "file"},
        ]),
    )
    .await;

    let client = Mega2TreeClient::new(&mock.url()).expect("client from mock URL");
    let listing = client
        .fetch_listing("/src sub", Some("ref with space"))
        .await
        .expect("listing");

    let state = mock.state.lock().expect("mock state poisoned");
    assert_eq!(state.queries.len(), 1, "exactly one request");
    assert_eq!(state.queries[0].0, "/src sub", "path round-trips");
    assert_eq!(state.queries[0].1, "ref with space", "refs round-trips");
    assert!(!state.saw_authorization, "listing must be anonymous");
    drop(state);

    let names: Vec<(&str, ContentType)> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.content_type))
        .collect();
    assert_eq!(
        names,
        vec![
            ("beta-dir", ContentType::Directory),
            ("alpha-file", ContentType::File),
            ("zeta-file", ContentType::File),
        ]
    );
}

#[tokio::test]
async fn transport_maps_http_failures_and_never_writes_local_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = ChangeDirGuard::new(dir.path());

    let mock_401 = MockMega2Tree::start(401, json!([])).await;
    let err = Mega2TreeClient::new(&mock_401.url())
        .expect("client")
        .fetch_listing("/", None)
        .await
        .expect_err("401 must fail");
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::AuthPermissionDenied
    );
    assert!(
        !err.message().contains("mock failure"),
        "body must not leak"
    );

    let mock_500 = MockMega2Tree::start(500, json!([])).await;
    let err = Mega2TreeClient::new(&mock_500.url())
        .expect("client")
        .fetch_listing("/", None)
        .await
        .expect_err("500 must fail");
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::NetworkProtocol
    );

    // The transport must not write anything into the current directory.
    let leftovers: HashMap<_, _> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .map(|name| (name, ()))
        .collect();
    assert!(
        leftovers.is_empty(),
        "transport wrote local state: {leftovers:?}"
    );
}
