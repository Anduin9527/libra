//! plan-20260912 MB-02: mega2 browser TUI state-machine integration.
//!
//! Navigation semantics, one-fetch-per-action accounting, hostile-name
//! fail-closed behaviour and rendering back-stops — all against a mock server.

use std::{
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use libra::{
    command::mega2_browser::{ActionResult, BrowserState, Key, render},
    internal::protocol::mega2_tree::Mega2TreeSession,
};
use serde_json::json;

/// Minimal blocking mock of the mega2 tree route with a canned body.
struct MockTreeServer {
    addr: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl MockTreeServer {
    fn start(tree_items: serde_json::Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let stop_clone = Arc::clone(&stop);
        thread::spawn(move || {
            let body = json!({
                "req_result": true,
                "data": {"tree_items": tree_items},
                "err_message": "",
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking mock connection");
                        use std::io::{Read, Write};
                        let mut buf = [0u8; 8192];
                        let _ = stream.read(&mut buf);
                        requests_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            stop,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockTreeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn navigation_root_and_no_prefetch() {
    let server = MockTreeServer::start(json!([
        {"name": "beta-dir", "path": "/", "content_type": "directory"},
        {"name": "alpha-file", "path": "/", "content_type": "file"},
    ]));
    let session = Mega2TreeSession::new(&server.url()).expect("session");
    let mut state = BrowserState::new(session, "/", None).expect("state");

    state.fetch_current().await.expect("root listing");
    assert_eq!(server.requests(), 1, "one request for the initial level");
    assert_eq!(state.entries.len(), 2);

    // Directories were sorted before files by MB-01; selection walks within the
    // current level only and issues no request.
    state.selection = 0;
    assert_eq!(state.handle_key(Key::Enter), ActionResult::FetchCurrent);
    assert_eq!(
        server.requests(),
        1,
        "cursor/enter planning issues no fetch"
    );
    state.fetch_current().await.expect("child listing");
    assert_eq!(server.requests(), 2, "exactly one fetch per navigation");
    assert_eq!(state.path, "/beta-dir");

    // A file entry is inert: no fetch, status line only.
    state.selection = 1;
    assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
    assert_eq!(server.requests(), 2);

    // Reload is one fetch; the rendered frame carries the validated listing.
    assert_eq!(state.handle_key(Key::Reload), ActionResult::FetchCurrent);
    state.fetch_current().await.expect("reload");
    assert_eq!(server.requests(), 3);
    let frame = render(&state, &server.url());
    assert!(frame.contains("beta-dir"));
    assert!(frame.contains(&state.path));
}

#[tokio::test]
async fn hostile_names_fail_closed_before_render() {
    let server = MockTreeServer::start(json!([
        {"name": "evil\u{1b}[2Jname", "path": "/", "content_type": "directory"},
    ]));
    let session = Mega2TreeSession::new(&server.url()).expect("session");
    let mut state = BrowserState::new(session, "/", None).expect("state");

    let err = state
        .fetch_current()
        .await
        .expect_err("hostile name refused");
    assert_eq!(
        err.stable_code(),
        libra::utils::error::StableErrorCode::NetworkProtocol
    );
    assert!(
        !err.message().contains("evil"),
        "untrusted payload must not leak into errors"
    );
    assert_eq!(state.entries.len(), 0, "no listing applied on failure");
}
