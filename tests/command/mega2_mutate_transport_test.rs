//! plan-20260912 MB-07: `delete-entry` / `move-entry` transport contract.
//!
//! Mock-family coverage: delete success, move success, rename success, 401,
//! 403 on one of the two move paths, missing source, and a traversal
//! destination that must be refused before any request.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
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
        mega2_mutate::{DELETE_ENTRY_ROUTE, MOVE_ENTRY_ROUTE, Mega2MutateClient},
        mega2_tree::ContentType,
    },
    utils::error::StableErrorCode,
};

#[derive(Debug, Clone)]
struct Captured {
    line: String,
    headers: HashMap<String, String>,
    body: serde_json::Value,
}

struct MockMutateServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    last: Arc<Mutex<Option<Captured>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    const MAX_REQUEST_BYTES: usize = 64 * 1024;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 || bytes.len() + n > MAX_REQUEST_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
        let Some(head_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = std::str::from_utf8(&bytes[..head_end]).ok()?;
        let content_length = head
            .split("\r\n")
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse::<usize>())
            .transpose()
            .ok()?
            .unwrap_or(0);
        let request_end = head_end.checked_add(4)?.checked_add(content_length)?;
        if request_end > MAX_REQUEST_BYTES {
            return None;
        }
        if bytes.len() >= request_end {
            return String::from_utf8(bytes).ok();
        }
    }
}

impl MockMutateServer {
    /// `status_for` decides the response per request body; `None` means 200.
    fn start(status_for: Option<fn(&serde_json::Value) -> u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let last_clone = Arc::clone(&last);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking mock connection");
                        let Some(raw) = read_request(&mut stream) else {
                            continue;
                        };
                        let (head, body) = raw
                            .split_once("\r\n\r\n")
                            .map(|(h, b)| (h.to_string(), b.to_string()))
                            .unwrap_or_default();
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
                        let body: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                        let status = status_for.map(|f| f(&body)).unwrap_or(200);
                        *last_clone.lock().expect("lock") = Some(Captured {
                            line,
                            headers,
                            body: body.clone(),
                        });
                        requests_clone.fetch_add(1, Ordering::SeqCst);

                        let payload = if status == 200 {
                            serde_json::json!({
                                "req_result": true,
                                "data": {
                                    "commit_id": "commit-1",
                                    "path": "/gone",
                                    "from_path": "/src/old",
                                    "to_path": "/dst/old",
                                    "cl_link": null,
                                },
                            })
                        } else {
                            serde_json::json!({"req_result": false, "data": null})
                        }
                        .to_string();
                        let reason = if status == 200 { "OK" } else { "Error" };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
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

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn last(&self) -> Captured {
        self.last.lock().expect("lock").clone().expect("captured")
    }
}

impl Drop for MockMutateServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn client(server: &MockMutateServer, with_token: bool) -> Mega2MutateClient {
    let token = with_token.then(|| Mega2Token::new("tok-1").expect("token"));
    Mega2MutateClient::new(&server.url(), token).expect("client")
}

#[tokio::test]
async fn delete_success_posts_the_mounted_route_and_fields() {
    let server = MockMutateServer::start(None);
    let client = client(&server, true);
    let receipt = client
        .delete_directory("/src", "old", Some(ContentType::Directory))
        .await
        .expect("delete succeeds");

    assert_eq!(receipt.commit_id, "commit-1");
    assert_eq!(receipt.path.as_deref(), Some("/gone"));
    assert_eq!(server.requests(), 1, "exactly one write request");
    let captured = server.last();
    assert!(
        captured
            .line
            .starts_with(&format!("POST {DELETE_ENTRY_ROUTE} ")),
        "{}",
        captured.line
    );
    assert_eq!(
        captured.body,
        serde_json::json!({"path": "/src", "name": "old", "skip_build": true})
    );
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tok-1")
    );
}

#[tokio::test]
async fn move_and_rename_success_use_the_move_route() {
    let server = MockMutateServer::start(None);
    let client = client(&server, false);

    client
        .move_entry("/src", "old", "/dst", "new", Some(ContentType::Directory))
        .await
        .expect("move succeeds");
    assert!(
        server
            .last()
            .line
            .starts_with(&format!("POST {MOVE_ENTRY_ROUTE} ")),
        "{}",
        server.last().line
    );
    assert_eq!(
        server.last().body,
        serde_json::json!({
            "from_path": "/src",
            "from_name": "old",
            "to_path": "/dst",
            "to_name": "new",
            "skip_build": true,
        })
    );
    assert!(
        !server.last().headers.contains_key("authorization"),
        "anonymous deployment sends no Authorization header"
    );

    let receipt = client
        .rename_directory("/src", "old", "renamed", Some(ContentType::Directory))
        .await
        .expect("rename succeeds");
    assert_eq!(receipt.from_path.as_deref(), Some("/src/old"));
    assert_eq!(server.requests(), 2);
    let body = server.last().body;
    assert_eq!(body["from_path"], body["to_path"]);
    assert_eq!(body["to_name"], "renamed");
}

#[tokio::test]
async fn unauthenticated_mutation_maps_to_auth_missing_credentials() {
    let server = MockMutateServer::start(Some(|_| 401));
    let client = client(&server, false);
    let err = client
        .delete_directory("/src", "old", None)
        .await
        .expect_err("401");
    assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
    assert_eq!(server.requests(), 1);
}

#[tokio::test]
async fn forbidden_destination_rejects_the_whole_move() {
    // 403 when the destination parent is `/blocked` (the second authorized path).
    let server = MockMutateServer::start(Some(|body: &serde_json::Value| {
        if body.get("to_path").and_then(|v| v.as_str()) == Some("/blocked") {
            403
        } else {
            200
        }
    }));
    let client = client(&server, true);

    client
        .move_entry("/src", "old", "/dst", "new", None)
        .await
        .expect("allowed destination");
    let err = client
        .move_entry("/src", "old", "/blocked", "new", None)
        .await
        .expect_err("403 on to_path");
    assert_eq!(err.stable_code(), StableErrorCode::AuthPermissionDenied);
    assert!(!err.render().contains("tok-1"), "token leaked");
    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn missing_source_and_duplicate_destination_map_to_stable_errors() {
    for status in [400, 404, 409] {
        let status_fn: fn(&serde_json::Value) -> u16 = match status {
            400 => |_| 400,
            404 => |_| 404,
            _ => |_| 409,
        };
        let server = MockMutateServer::start(Some(status_fn));
        let client = client(&server, true);
        let err = client
            .delete_directory("/src", "missing", None)
            .await
            .expect_err("refused");
        assert!(
            matches!(
                err.stable_code(),
                StableErrorCode::CliInvalidTarget
                    | StableErrorCode::ConflictOperationBlocked
                    | StableErrorCode::NetworkProtocol
            ),
            "status {status}: {:?}",
            err.stable_code()
        );
        assert!(!err.render().contains("tok-1"), "token leaked");
        assert_eq!(server.requests(), 1);
    }
}

#[tokio::test]
async fn traversal_destination_is_refused_before_any_request() {
    let server = MockMutateServer::start(None);
    let client = client(&server, false);

    for (from_path, to_path) in [("/src", "/../escape"), ("/..", "/dst")] {
        let err = client
            .move_entry(from_path, "old", to_path, "new", None)
            .await
            .expect_err("traversal refused");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
    }
    // Hostile names are refused too.
    let err = client
        .move_entry("/src", "old", "/dst", "a/b", None)
        .await
        .expect_err("separator refused");
    assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);

    assert_eq!(server.requests(), 0, "validation precedes the network");
}
