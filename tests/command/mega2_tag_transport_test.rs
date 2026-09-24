//! plan-20260912 MB-10: `tag_router` transport contract.
//!
//! Mock family: list page (GET + required query keys, anonymous), create
//! lightweight vs annotated, delete, 401 on create, 403 on a path-scoped
//! token for create and delete, hostile name, and the wrong-method
//! POST-list rejection (405) proving the client only ever uses GET.

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
        mega2_tag::{CreateTagOptions, Mega2TagClient},
    },
    utils::error::StableErrorCode,
};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: serde_json::Value,
}

/// Routes tag requests: methodology checks are done by the handler closure.
struct MockTagServer {
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

impl MockTagServer {
    /// `respond` maps (method, path) to a status code; 200 sends a valid body.
    fn start(respond: fn(&str, &str) -> u16) -> Self {
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
                        let (head, raw_body) = raw
                            .split_once("\r\n\r\n")
                            .map(|(h, b)| (h.to_string(), b.to_string()))
                            .unwrap_or_default();
                        let mut lines = head.split("\r\n");
                        let line = lines.next().unwrap_or_default().to_string();
                        let mut parts = line.split_whitespace();
                        let method = parts.next().unwrap_or_default().to_string();
                        let target = parts.next().unwrap_or_default().to_string();
                        let path = target.split('?').next().unwrap_or_default().to_string();
                        let mut headers = HashMap::new();
                        for header in lines {
                            if let Some((name, value)) = header.split_once(':') {
                                headers.insert(
                                    name.trim().to_ascii_lowercase(),
                                    value.trim().to_string(),
                                );
                            }
                        }
                        let body =
                            serde_json::from_str(&raw_body).unwrap_or(serde_json::Value::Null);
                        *last_clone.lock().expect("lock") = Some(Captured {
                            method: method.clone(),
                            target: target.clone(),
                            headers,
                            body: body.clone(),
                        });
                        requests_clone.fetch_add(1, Ordering::SeqCst);

                        let status = respond(&method, &path);
                        let payload = if status == 200 {
                            if method == "GET" && path.ends_with("/list") {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {
                                        "total": 2,
                                        "items": [{
                                            "name": "v1.0.0",
                                            "tag_id": "t1",
                                            "object_id": "o1",
                                            "object_type": "commit",
                                            "tagger": "Libra",
                                            "message": "",
                                            "created_at": "2026-09-21T00:00:00Z",
                                        }],
                                    },
                                })
                            } else if method == "DELETE" {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {"deleted_tag": "v1.0.0", "message": "deleted"},
                                })
                            } else {
                                serde_json::json!({
                                    "req_result": true,
                                    "data": {
                                        "name": "v1.0.0",
                                        "tag_id": "t1",
                                        "object_id": "o1",
                                        "object_type": "commit",
                                        "tagger": "Libra",
                                        "message": "release",
                                        "created_at": "2026-09-21T00:00:00Z",
                                    },
                                })
                            }
                        } else {
                            serde_json::json!({"req_result": false, "data": null})
                        }
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
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

impl Drop for MockTagServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn ok_respond(_method: &str, _path: &str) -> u16 {
    200
}

fn client(server: &MockTagServer, with_token: bool) -> Mega2TagClient {
    let token = with_token.then(|| Mega2Token::new("tag-token").expect("token"));
    Mega2TagClient::new(&server.url(), token).expect("client")
}

#[tokio::test]
async fn list_uses_get_with_the_three_required_keys_and_no_authorization() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    let page = client.list_tags(1, 50, "/").await.expect("list succeeds");

    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "v1.0.0");
    assert_eq!(page.items[0].tagger, "Libra");

    let captured = server.last();
    assert_eq!(captured.method, "GET", "list is GET, never POST");
    assert!(
        captured.target.starts_with("/api/v1/tags/list?"),
        "{}",
        captured.target
    );
    for key in ["page=1", "per_page=50", "path=%2F"] {
        assert!(captured.target.contains(key), "{}", captured.target);
    }
    assert!(
        !captured.headers.contains_key("authorization"),
        "list is anonymous: {:?}",
        captured.headers
    );
    assert_eq!(server.requests(), 1, "one page per request");
}

#[tokio::test]
async fn create_lightweight_and_annotated_shapes() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);

    let lightweight = CreateTagOptions {
        name: "v1.0.0",
        ..CreateTagOptions::default()
    };
    let tag = client.create_tag(&lightweight).await.expect("lightweight");
    assert_eq!(tag.name, "v1.0.0");
    let captured = server.last();
    assert_eq!(captured.method, "POST");
    assert!(
        captured.target.starts_with("/api/v1/tags"),
        "{}",
        captured.target
    );
    assert_eq!(captured.body["name"], "v1.0.0");
    assert_eq!(captured.body["path_context"], "/");
    assert!(
        captured.body.get("message").is_none(),
        "omitted message = lightweight: {}",
        captured.body
    );
    assert!(captured.body.get("tagger").is_none());
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tag-token")
    );

    let annotated = CreateTagOptions {
        name: "v2.0.0",
        target: Some("deadbeef"),
        tagger_name: Some("Libra"),
        message: Some("release 2.0.0"),
        ..CreateTagOptions::default()
    };
    client.create_tag(&annotated).await.expect("annotated");
    let body = server.last().body;
    assert_eq!(body["message"], "release 2.0.0");
    assert_eq!(body["target"], "deadbeef");
    assert_eq!(body["tagger_name"], "Libra");
    assert!(body.get("tagger").is_none());
    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn delete_uses_the_tag_route_with_the_path_selector() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    let receipt = client.delete_tag("v1.0.0", "/").await.expect("delete");

    assert_eq!(receipt.deleted_tag, "v1.0.0");
    assert_eq!(receipt.message, "deleted");
    let captured = server.last();
    assert_eq!(captured.method, "DELETE");
    assert!(
        captured.target.starts_with("/api/v1/tags/v1.0.0?"),
        "{}",
        captured.target
    );
    assert!(captured.target.contains("path=%2F"), "{}", captured.target);
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer tag-token")
    );
}

#[tokio::test]
async fn unauthenticated_create_maps_to_auth_missing_credentials() {
    let server = MockTagServer::start(|_m, _p| 401);
    let client = client(&server, false);
    let err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("401");
    assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
    assert_eq!(server.requests(), 1);
}

#[tokio::test]
async fn path_scoped_token_403_is_diagnosable_for_create_and_delete() {
    let server = MockTagServer::start(|_m, _p| 403);
    let client = client(&server, true);

    let create_err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            path_context: Some("/project"),
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("403 create");
    assert_eq!(
        create_err.stable_code(),
        StableErrorCode::AuthPermissionDenied
    );
    assert!(!create_err.render().contains("tag-token"), "token leaked");

    let delete_err = client.delete_tag("v1", "/").await.expect_err("403 delete");
    assert_eq!(
        delete_err.stable_code(),
        StableErrorCode::AuthPermissionDenied
    );
    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn hostile_names_are_refused_before_any_request() {
    let server = MockTagServer::start(ok_respond);
    let client = client(&server, true);
    for bad in ["..", "a..b", "a//b", "release.lock", "has space", "nul\0"] {
        let err = client
            .create_tag(&CreateTagOptions {
                name: bad,
                ..CreateTagOptions::default()
            })
            .await
            .expect_err("hostile name");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        assert!(client.get_tag(bad, "/").await.is_err());
        assert!(client.delete_tag(bad, "/").await.is_err());
    }
    assert_eq!(server.requests(), 0, "validation precedes the network");
}

#[tokio::test]
async fn list_never_posts_and_wrong_method_405_is_a_stable_error() {
    // Only GET on the list route is accepted; anything else is a 405.
    let server = MockTagServer::start(|method, path| {
        if method == "GET" && path.ends_with("/list") {
            200
        } else {
            405
        }
    });
    let client = client(&server, false);
    client.list_tags(1, 10, "/").await.expect("GET accepted");
    assert_eq!(server.last().method, "GET");

    // A 405 arriving for a write route maps to a stable usage error.
    let err = client
        .create_tag(&CreateTagOptions {
            name: "v1",
            ..CreateTagOptions::default()
        })
        .await
        .expect_err("405");
    assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
    assert_eq!(server.requests(), 2);
}
