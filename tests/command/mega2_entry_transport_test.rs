//! plan-20260912 MB-04: `create-entry` transport contract.
//!
//! Drives the real client against a loopback mock that records method, target,
//! headers and body, so the wire shape (directory-only POST, Bearer header
//! presence/absence), stable error mapping, bounded body handling and token
//! redaction are all pinned.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use libra::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_entry::{Mega2EntryClient, validate_entry_name},
    },
    utils::error::StableErrorCode,
};

#[derive(Debug, Clone)]
struct CapturedRequest {
    line: String,
    headers: HashMap<String, String>,
    body: String,
}

struct MockEntryServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    last: Arc<Mutex<Option<CapturedRequest>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockEntryServer {
    fn start(status: u16, body: String, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let last_clone = Arc::clone(&last);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking mock connection");
                        if !delay.is_zero() {
                            thread::sleep(delay);
                        }
                        let mut buf = vec![0u8; 64 * 1024];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
                        let mut lines = head.split("\r\n");
                        let line = lines.next().unwrap_or_default().to_string();
                        let mut headers = HashMap::new();
                        for header in lines {
                            if let Some((name, value)) = header.split_once(':') {
                                headers.insert(
                                    name.trim().to_ascii_lowercase(),
                                    value.trim().to_string(),
                                );
                            }
                        }
                        let body = body.to_string();
                        *last_clone.lock().expect("lock") = Some(CapturedRequest {
                            line,
                            headers,
                            body,
                        });
                        requests_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            last,
            stop,
            join: Some(join),
        }
    }

    fn ok() -> Self {
        Self::start(
            200,
            serde_json::json!({
                "req_result": true,
                "data": {
                    "commit_id": "commit-abc",
                    "new_oid": "oid-def",
                    "path": "/src/pkg",
                    "cl_link": null,
                },
                "err_message": "",
            })
            .to_string(),
            Duration::ZERO,
        )
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn last(&self) -> CapturedRequest {
        self.last
            .lock()
            .expect("lock")
            .clone()
            .expect("captured request")
    }
}

impl Drop for MockEntryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn token(raw: &str) -> Mega2Token {
    Mega2Token::new(raw).expect("token")
}

#[tokio::test]
async fn success_posts_directory_body_with_bearer_token() {
    let server = MockEntryServer::ok();
    let client = Mega2EntryClient::new(&server.url(), Some(token("secret-token"))).expect("client");

    let receipt = client
        .create_directory("/src", "pkg")
        .await
        .expect("create succeeds");
    assert_eq!(receipt.commit_id, "commit-abc");
    assert_eq!(receipt.new_oid, "oid-def");
    assert_eq!(receipt.path.as_deref(), Some("/src/pkg"));
    assert_eq!(receipt.cl_link, None);

    assert_eq!(server.requests(), 1, "exactly one POST");
    let request = server.last();
    assert!(
        request.line.starts_with("POST /api/v1/create-entry "),
        "{:?}",
        request.line
    );
    let auth = request
        .headers
        .get("authorization")
        .expect("authorization header present");
    assert_eq!(auth, "Bearer secret-token");

    let body: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(body["is_directory"], true);
    assert_eq!(body["name"], "pkg");
    assert_eq!(body["path"], "/src");
    assert_eq!(body["skip_build"], true);
    assert!(body.get("mode").is_none(), "mode must be omitted: {body}");
    assert!(body.get("author_email").is_none(), "email omitted: {body}");
    assert!(
        body.get("author_username").is_none(),
        "username omitted: {body}"
    );
}

#[tokio::test]
async fn anonymous_configuration_sends_no_authorization_header() {
    let server = MockEntryServer::ok();
    let client = Mega2EntryClient::new(&server.url(), None).expect("client");
    assert!(!client.has_token());

    client
        .create_directory("/", "newdir")
        .await
        .expect("anonymous create succeeds");
    let request = server.last();
    assert!(
        !request.headers.contains_key("authorization"),
        "anonymous request must not carry Authorization: {:?}",
        request.headers
    );
    let body: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(body["path"], "/", "root parent is sent as /");
}

#[tokio::test]
async fn remote_rejections_map_to_stable_secret_free_errors() {
    let cases: &[(u16, StableErrorCode, &str)] = &[
        (400, StableErrorCode::CliInvalidTarget, "already exist"),
        (401, StableErrorCode::AuthMissingCredentials, "write token"),
        (403, StableErrorCode::AuthPermissionDenied, "not authorized"),
        (409, StableErrorCode::ConflictOperationBlocked, "409"),
    ];
    for (status, code, needle) in cases {
        let server = MockEntryServer::start(*status, "SECRET-BODY".to_string(), Duration::ZERO);
        let client =
            Mega2EntryClient::new(&server.url(), Some(token("redact-me"))).expect("client");
        let err = client
            .create_directory("/", "dir")
            .await
            .expect_err("rejected");
        assert_eq!(err.stable_code(), *code, "status {status}");
        let rendered = err.render();
        assert!(
            rendered.contains(needle),
            "status {status} message: {rendered}"
        );
        assert!(!rendered.contains("SECRET-BODY"), "body leaked: {rendered}");
        assert!(!rendered.contains("redact-me"), "token leaked: {rendered}");
    }
}

#[tokio::test]
async fn schema_failures_are_rejected() {
    let bodies = [
        serde_json::json!({"req_result": false, "data": null}).to_string(),
        serde_json::json!({"req_result": true, "data": null}).to_string(),
        serde_json::json!({
            "req_result": true,
            "data": {"commit_id": "", "new_oid": "oid", "path": "/", "cl_link": null}
        })
        .to_string(),
        "not json at all".to_string(),
    ];
    for body in bodies {
        let server = MockEntryServer::start(200, body.clone(), Duration::ZERO);
        let client = Mega2EntryClient::new(&server.url(), None).expect("client");
        let err = client
            .create_directory("/", "dir")
            .await
            .expect_err("schema refused");
        assert_eq!(
            err.stable_code(),
            StableErrorCode::NetworkProtocol,
            "{body}"
        );
        assert!(!err.render().contains("not json at all"), "body leaked");
    }
}

#[tokio::test]
async fn hostile_names_and_paths_are_refused_without_any_request() {
    let server = MockEntryServer::ok();
    let client = Mega2EntryClient::new(&server.url(), None).expect("client");

    for bad_name in ["a/b", "..", ".", "nul\0", "ctrl\u{1b}", ""] {
        let err = client
            .create_directory("/", bad_name)
            .await
            .expect_err("name refused");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
    }
    for bad_path in ["relative", "..", "/..", "/a/../b"] {
        let err = client
            .create_directory(bad_path, "dir")
            .await
            .expect_err("path refused");
        // MB-01 path validation owns this code (CliInvalidTarget).
        assert_eq!(
            err.stable_code(),
            StableErrorCode::CliInvalidTarget,
            "path {bad_path}"
        );
    }
    assert_eq!(server.requests(), 0, "validation precedes the network");
    assert!(validate_entry_name("ok-name").is_ok());
}

#[tokio::test]
async fn oversized_response_is_aborted() {
    let huge = format!(
        "{{\"req_result\":true,\"data\":{{\"commit_id\":\"c\",\"new_oid\":\"o\",\"pad\":\"{}\"}}}}",
        "x".repeat(1_100_000)
    );
    let server = MockEntryServer::start(200, huge, Duration::ZERO);
    let client = Mega2EntryClient::new(&server.url(), None).expect("client");
    let err = client
        .create_directory("/", "dir")
        .await
        .expect_err("oversize refused");
    assert_eq!(err.stable_code(), StableErrorCode::NetworkProtocol);
    assert!(err.render().contains("limit"), "{}", err.render());
}

#[tokio::test]
async fn timeout_maps_to_network_unavailable() {
    let server = MockEntryServer::start(200, "{}".to_string(), Duration::from_millis(400));
    let client = Mega2EntryClient::with_timeouts(&server.url(), None, Duration::from_millis(50))
        .expect("client");
    let err = client
        .create_directory("/", "dir")
        .await
        .expect_err("timeout");
    assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
}

#[tokio::test]
async fn invalid_server_urls_are_refused_before_any_request() {
    let server = MockEntryServer::ok();
    for bad in [
        "http://example.com",
        "ftp://example.com",
        "https://u:p@example.com",
    ] {
        let err = Mega2EntryClient::new(bad, None).expect_err("url refused");
        assert!(
            matches!(
                err.stable_code(),
                StableErrorCode::CliInvalidArguments | StableErrorCode::NetworkProtocol
            ) || err.render().contains("mega2 server URL"),
            "unexpected error for {bad}: {}",
            err.render()
        );
    }
    assert_eq!(server.requests(), 0);
}
