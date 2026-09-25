//! L2 (`--features test-network`): the fetch client's `want`-line negotiates
//! exactly the capabilities Libra's pack decoder can honour — and none it
//! cannot — so a fetch never asks a server for a pack it could not decode.
//!
//! The want-line and shallow advertisement are checked directly; a local Git
//! daemon exercises shallow-source cloning and fetch against a real upload-pack.

#![cfg(feature = "test-network")]

use bytes::BytesMut;
use libra::{
    git_protocol::{ServiceType, add_pkt_line_string},
    internal::protocol::{
        generate_upload_pack_content, generate_upload_pack_content_with_capabilities,
        parse_discovered_references,
    },
};

#[cfg(unix)]
fn checked(command: &mut std::process::Command) -> std::process::Output {
    let output = command.output().expect("start fixture command");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn upload_pack_body_advertises_supported_capabilities_only() {
    let have: Vec<String> = Vec::new();
    let want = vec!["1".repeat(40)];
    let body = generate_upload_pack_content(&have, &want, &[], None);
    let text = String::from_utf8_lossy(&body);

    // Capabilities the decoder honours: sideband multiplexing, delta detail, and
    // in-pack offset deltas (git-internal resolves OffsetDelta).
    for capability in [
        "side-band-64k",
        "multi_ack_detailed",
        "ofs-delta",
        "include-tag",
    ] {
        assert!(
            text.contains(capability),
            "want line must advertise {capability}: {text}"
        );
    }
    // Identify the client to the server, as Git does.
    assert!(
        text.contains("agent=libra/"),
        "want line must send an agent string: {text}"
    );

    // Deliberately NOT advertised:
    // - `thin-pack` would delta against objects outside the pack, which the
    //   self-contained decoder cannot complete;
    // - `report-status` is a push (receive-pack) capability, not upload-pack.
    assert!(
        !text.contains("thin-pack"),
        "thin-pack is unsupported and must not be advertised: {text}"
    );
    assert!(
        !text.contains("report-status"),
        "report-status is push-only and must not be on an upload-pack want line: {text}"
    );
}

/// The SHA-256 negotiation adds `object-format=sha256`; a SHA-1 repository must
/// not (its absence means SHA-1, the wire default).
#[test]
fn upload_pack_body_is_sha1_by_default() {
    let have: Vec<String> = Vec::new();
    let want = vec!["1".repeat(40)];
    let body = generate_upload_pack_content(&have, &want, &[], None);
    let text = String::from_utf8_lossy(&body);
    // The default test process hash kind is SHA-1, so no object-format is sent.
    assert!(
        !text.contains("object-format=sha256"),
        "a SHA-1 fetch must not advertise object-format=sha256: {text}"
    );
}

#[test]
fn upload_pack_requests_advertised_shallow_with_and_without_depth() {
    let want = vec!["1".repeat(40)];
    let boundary = ["2".repeat(40)];
    let advertised = vec!["shallow".to_string()];

    for depth in [None, Some(2)] {
        let body = generate_upload_pack_content_with_capabilities(
            &[],
            &want,
            &boundary,
            depth,
            &advertised,
        )
        .expect("advertised shallow support permits the negotiation");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains(" shallow agent=libra/"), "{text}");
        assert!(
            text.contains(&format!("shallow {}\n", boundary[0])),
            "{text}"
        );
        assert_eq!(text.contains("deepen 2\n"), depth.is_some(), "{text}");
    }
}

#[test]
fn upload_pack_rejects_unadvertised_shallow_before_sending_depth_or_boundaries() {
    let want = vec!["1".repeat(40)];
    let boundary = ["2".repeat(40)];

    for (shallow, depth) in [(&[][..], Some(2)), (&boundary[..], None)] {
        let error = generate_upload_pack_content_with_capabilities(
            &[],
            &want,
            shallow,
            depth,
            &["multi_ack_detailed".to_string()],
        )
        .expect_err("the server did not advertise shallow support");
        assert!(error.to_string().contains("shallow"), "{error}");
    }

    let body = generate_upload_pack_content_with_capabilities(
        &[],
        &want,
        &[],
        None,
        &["multi_ack_detailed".to_string()],
    )
    .expect("full fetch does not require shallow support");
    assert!(!String::from_utf8_lossy(&body).contains(" shallow "));
}

#[test]
fn upload_pack_discovery_reads_source_shallow_boundary() {
    let oid = "1".repeat(40);
    let mut wire = BytesMut::new();
    add_pkt_line_string(&mut wire, format!("{oid} HEAD\0side-band-64k shallow\n"));
    add_pkt_line_string(&mut wire, format!("{oid} refs/heads/main\n"));
    add_pkt_line_string(&mut wire, format!("shallow {oid}\n"));
    wire.extend_from_slice(b"0000");

    let discovery = parse_discovered_references(wire.freeze(), ServiceType::UploadPack)
        .expect("Git's advertised shallow packet must parse");
    assert_eq!(discovery.shallow_boundaries, vec![oid]);
    assert_eq!(discovery.refs.len(), 2);
}

#[test]
fn upload_pack_discovery_rejects_malformed_shallow_boundary() {
    let oid = "1".repeat(40);
    for invalid in ["1".repeat(39), "z".repeat(40)] {
        let mut wire = BytesMut::new();
        add_pkt_line_string(&mut wire, format!("{oid} HEAD\0shallow\n"));
        add_pkt_line_string(&mut wire, format!("shallow {invalid}\n"));
        wire.extend_from_slice(b"0000");
        let error = parse_discovered_references(wire.freeze(), ServiceType::UploadPack)
            .expect_err("bad remote boundary must be rejected");
        assert!(
            error.to_string().contains("Invalid shallow boundary"),
            "{error}"
        );
    }

    let mut wire = BytesMut::new();
    add_pkt_line_string(&mut wire, format!("shallow {oid}\n"));
    wire.extend_from_slice(b"0000");
    let error = parse_discovered_references(wire.freeze(), ServiceType::UploadPack)
        .expect_err("shallow packet before first ref is invalid");
    assert!(
        error.to_string().contains("Unexpected shallow boundary"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn shallow_git_server_clone_persists_only_missing_history_boundaries() {
    use std::{
        fs,
        net::{TcpListener, TcpStream},
        process::{Child, Command, Stdio},
        thread,
        time::Duration,
    };

    struct Daemon(Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let root = tempfile::tempdir().expect("temporary Git fixture");
    let source = root.path().join("source");
    let full = root.path().join("full.git");
    let shallow = root.path().join("shallow.git");
    let home = root.path().join("home");
    fs::create_dir(&home).expect("isolated home");
    checked(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&source),
    );
    checked(
        Command::new("git")
            .args(["-C"])
            .arg(&source)
            .args(["config", "user.name", "Fixture"]),
    );
    checked(Command::new("git").args(["-C"]).arg(&source).args([
        "config",
        "user.email",
        "fixture@example.test",
    ]));
    checked(Command::new("git").args(["-C"]).arg(&source).args([
        "config",
        "commit.gpgsign",
        "false",
    ]));
    for number in 1..=3 {
        fs::write(source.join("history.txt"), format!("commit {number}\n")).expect("write history");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "history.txt"]),
        );
        checked(Command::new("git").arg("-C").arg(&source).args([
            "commit",
            "-qm",
            &format!("commit {number}"),
        ]));
    }
    checked(
        Command::new("git")
            .args(["clone", "-q", "--bare"])
            .arg(&source)
            .arg(&full),
    );
    fs::write(source.join("history.txt"), "commit 4\n").expect("write fourth commit");
    checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["commit", "-qam", "commit 4"]),
    );
    let newest = checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["rev-parse", "HEAD"]),
    );
    checked(
        Command::new("git")
            .args(["clone", "-q", "--bare", "--depth=3"])
            .arg(format!("file://{}", source.display()))
            .arg(&shallow),
    );
    let boundary = fs::read_to_string(shallow.join("shallow")).expect("shallow Git source");

    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve daemon port");
    let port = listener.local_addr().expect("daemon address").port();
    drop(listener);
    let mut daemon = Daemon(
        Command::new("git")
            .args(["daemon", "--reuseaddr", "--export-all"])
            .arg(format!("--base-path={}", root.path().display()))
            .args(["--listen=127.0.0.1", &format!("--port={port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start Git daemon"),
    );
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(daemon.0.try_wait().expect("poll Git daemon").is_none());
        thread::sleep(Duration::from_millis(20));
    }
    let shallow_url = format!("git://127.0.0.1:{port}/shallow.git");
    let full_url = format!("git://127.0.0.1:{port}/full.git");
    let shallow_dest = root.path().join("shallow-dest");
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("HOME", &home)
            .current_dir(root.path())
            .args(["clone", &shallow_url])
            .arg(&shallow_dest),
    );
    assert_eq!(
        fs::read_to_string(shallow_dest.join(".libra/shallow")).expect("preserved boundary"),
        boundary
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&shallow_dest)
            .arg("fsck"),
    );

    let rejected = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(root.path())
        .args(["clone", "--reject-shallow", &shallow_url])
        .arg(root.path().join("rejected"))
        .output()
        .expect("try reject-shallow clone");
    assert!(
        !rejected.status.success(),
        "shallow source must be rejected"
    );
    assert!(
        !root.path().join("rejected").exists(),
        "failed clone must remove its destination"
    );

    let full_dest = root.path().join("full-dest");
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("HOME", &home)
            .current_dir(root.path())
            .args(["clone", &full_url])
            .arg(&full_dest),
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&full_dest)
            .args(["remote", "set-url", "origin", &shallow_url]),
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&full_dest)
            .arg("fetch"),
    );
    let fetched = checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&full_dest)
            .args(["rev-parse", "refs/remotes/origin/main"]),
    );
    assert_eq!(
        String::from_utf8_lossy(&fetched.stdout).trim(),
        String::from_utf8_lossy(&newest.stdout).trim(),
        "fetch must import the newer tip from the shallow source"
    );
    assert!(
        !full_dest.join(".libra/shallow").exists(),
        "the complete local parent history must not become shallow"
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&full_dest)
            .arg("fsck"),
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_shallow_source_change_aborts_before_persisting_pack_or_refs() {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::Response,
        routing::any,
    };

    #[derive(Clone)]
    struct ServerState {
        source: PathBuf,
        old_boundary: String,
        new_boundary: String,
        restore_after_post: Arc<AtomicBool>,
        gets: Arc<AtomicUsize>,
        posts: Arc<AtomicUsize>,
    }

    async fn serve_upload_pack(
        State(state): State<ServerState>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request
            .uri()
            .path_and_query()
            .map(|uri| uri.as_str().to_owned());
        let is_get =
            method == "GET" && path.as_deref() == Some("/repo/info/refs?service=git-upload-pack");
        let is_post = method == "POST" && path.as_deref() == Some("/repo/git-upload-pack");
        if !is_get && !is_post {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("404 response");
        }
        let body = to_bytes(request.into_body(), 1024 * 1024)
            .await
            .expect("bounded upload-pack request");
        let response = tokio::task::spawn_blocking(move || {
            if is_get {
                state.gets.fetch_add(1, Ordering::SeqCst);
                let advertisement = checked(
                    Command::new("git")
                        .args(["upload-pack", "--stateless-rpc", "--advertise-refs"])
                        .arg(&state.source),
                );
                let mut wire = BytesMut::new();
                add_pkt_line_string(&mut wire, "# service=git-upload-pack\n".to_owned());
                wire.extend_from_slice(b"0000");
                wire.extend_from_slice(&advertisement.stdout);
                ("application/x-git-upload-pack-advertisement", wire.to_vec())
            } else {
                state.posts.fetch_add(1, Ordering::SeqCst);
                // The source becomes shallower after the first GET, before the
                // stateless POST. Without deepen, Git does not repeat this new
                // boundary in the response, so persisting the old one corrupts
                // the fetched ref's reachable history.
                fs::write(
                    state.source.join("shallow"),
                    format!("{}\n", state.new_boundary),
                )
                .expect("move source shallow boundary");
                let mut upload_pack = Command::new("git")
                    .args(["upload-pack", "--stateless-rpc"])
                    .arg(&state.source)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("start stateless upload-pack");
                upload_pack
                    .stdin
                    .take()
                    .expect("upload-pack stdin")
                    .write_all(&body)
                    .expect("send upload-pack request");
                let response = upload_pack
                    .wait_with_output()
                    .expect("read upload-pack result");
                assert!(
                    response.status.success(),
                    "upload-pack failed: {}",
                    String::from_utf8_lossy(&response.stderr)
                );
                if state.restore_after_post.load(Ordering::SeqCst) {
                    fs::write(state.source.join("shallow"), &state.old_boundary)
                        .expect("restore first advertisement after POST");
                }
                ("application/x-git-upload-pack-result", response.stdout)
            }
        })
        .await
        .expect("upload-pack task");
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", response.0)
            .body(Body::from(response.1))
            .expect("upload-pack response")
    }

    let root = tempfile::tempdir().expect("temporary Git fixture");
    let source = root.path().join("source");
    let shallow = root.path().join("shallow.git");
    let home = root.path().join("home");
    fs::create_dir(&home).expect("isolated home");
    checked(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&source),
    );
    for (key, value) in [
        ("user.name", "Fixture"),
        ("user.email", "fixture@example.test"),
        ("commit.gpgsign", "false"),
    ] {
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["config", key, value]),
        );
    }
    for number in 1..=4 {
        fs::write(source.join("history.txt"), format!("commit {number}\n")).expect("write history");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "history.txt"]),
        );
        checked(Command::new("git").arg("-C").arg(&source).args([
            "commit",
            "-qm",
            &format!("commit {number}"),
        ]));
    }
    checked(
        Command::new("git")
            .args(["clone", "-q", "--bare", "--depth=3"])
            .arg(format!("file://{}", source.display()))
            .arg(&shallow),
    );
    let old_boundary = fs::read_to_string(shallow.join("shallow")).expect("shallow source");
    let new_boundary = String::from_utf8(
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&shallow)
                .args(["rev-parse", "HEAD^"]),
        )
        .stdout,
    )
    .expect("UTF-8 commit id")
    .trim()
    .to_owned();
    assert_ne!(old_boundary.trim(), new_boundary);

    let gets = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let restore_after_post = Arc::new(AtomicBool::new(false));
    let state = ServerState {
        source: shallow.clone(),
        old_boundary: old_boundary.clone(),
        new_boundary,
        restore_after_post: restore_after_post.clone(),
        gets: gets.clone(),
        posts: posts.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP fixture");
    let url = format!(
        "http://{}/repo/",
        listener.local_addr().expect("HTTP address")
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/repo/{*path}", any(serve_upload_pack))
                .with_state(state),
        )
        .await
        .expect("HTTP fixture");
    });

    let clone_dest = root.path().join("failed-clone");
    let clone = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(root.path())
        .args(["clone", &url])
        .arg(&clone_dest)
        .output()
        .expect("clone from changing HTTP source");
    assert!(
        !clone.status.success(),
        "clone must reject changed shallow boundary: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert!(
        String::from_utf8_lossy(&clone.stderr).contains("shallow boundaries advertised"),
        "clone must fail for the boundary race: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert!(!clone_dest.exists(), "failed clone removes its destination");

    fs::write(shallow.join("shallow"), &old_boundary).expect("restore first advertisement");
    let fetch_dest = root.path().join("failed-fetch");
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(root.path())
            .env("HOME", &home)
            .args(["init"])
            .arg(&fetch_dest),
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&fetch_dest)
            .args(["remote", "add", "origin", &url]),
    );
    let fetch = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(&fetch_dest)
        .args(["fetch", "origin"])
        .output()
        .expect("fetch from changing HTTP source");
    assert!(
        !fetch.status.success(),
        "fetch must reject changed shallow boundary: {}",
        String::from_utf8_lossy(&fetch.stderr)
    );
    assert!(
        String::from_utf8_lossy(&fetch.stderr).contains("shallow boundaries advertised"),
        "fetch must fail for the boundary race: {}",
        String::from_utf8_lossy(&fetch.stderr)
    );
    assert!(
        !fetch_dest.join(".libra/shallow").exists(),
        "failed fetch must not record a stale shallow boundary"
    );
    assert!(
        !fetch_dest.join(".libra/refs/remotes/origin/main").exists(),
        "failed fetch must not update the remote-tracking ref"
    );
    let missing_ref = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&fetch_dest)
        .args(["rev-parse", "refs/remotes/origin/main"])
        .output()
        .expect("inspect remote-tracking ref");
    assert!(
        !missing_ref.status.success(),
        "failed fetch must leave no tracking ref"
    );
    let pack_dir = fetch_dest.join(".libra/objects/pack");
    if pack_dir.exists() {
        assert!(
            fs::read_dir(pack_dir)
                .expect("inspect pack directory")
                .next()
                .is_none(),
            "changed advertisement must fail before writing a pack"
        );
    }
    assert_eq!(posts.load(Ordering::SeqCst), 2, "both cases sent a POST");
    assert!(
        gets.load(Ordering::SeqCst) >= 4,
        "both cases must revalidate after the POST"
    );

    // GET #1 and the post-fetch GET now agree, but the POST itself was served
    // while the source was shallower. Advertisement equality alone must not
    // publish the dangling commit graph received in that pack.
    fs::write(shallow.join("shallow"), &old_boundary).expect("restore ABA source");
    restore_after_post.store(true, Ordering::SeqCst);
    let aba_clone_dest = root.path().join("aba-clone");
    let aba_clone = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(root.path())
        .args(["clone", &url])
        .arg(&aba_clone_dest)
        .output()
        .expect("clone from ABA source");
    assert!(
        !aba_clone.status.success(),
        "ABA clone must reject missing parent: {}",
        String::from_utf8_lossy(&aba_clone.stderr)
    );
    assert!(
        String::from_utf8_lossy(&aba_clone.stderr).contains("missing parent"),
        "ABA clone must fail for disconnected history: {}",
        String::from_utf8_lossy(&aba_clone.stderr)
    );
    assert!(
        !aba_clone_dest.exists(),
        "failed ABA clone removes its destination"
    );

    let aba_fetch = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(&fetch_dest)
        .args(["fetch", "origin"])
        .output()
        .expect("fetch from ABA source");
    assert!(
        !aba_fetch.status.success(),
        "ABA fetch must reject missing parent: {}",
        String::from_utf8_lossy(&aba_fetch.stderr)
    );
    assert!(
        String::from_utf8_lossy(&aba_fetch.stderr).contains("missing parent"),
        "ABA fetch must fail for disconnected history: {}",
        String::from_utf8_lossy(&aba_fetch.stderr)
    );
    assert!(
        !fetch_dest.join(".libra/shallow").exists(),
        "ABA fetch must not write shallow metadata"
    );
    let missing_aba_ref = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&fetch_dest)
        .args(["rev-parse", "refs/remotes/origin/main"])
        .output()
        .expect("inspect ABA remote-tracking ref");
    assert!(
        !missing_aba_ref.status.success(),
        "ABA fetch must not publish a disconnected ref"
    );
    assert_eq!(posts.load(Ordering::SeqCst), 4, "all scenarios sent a POST");
    server.abort();
}

/// A git:// upload-pack can advertise one shallow boundary but serve a pack
/// generated from a shallower source without including a new shallow packet.
/// The two advertisements still agree, so the received commit graph must be
/// checked before clone publishes a ref or a shallow marker.
#[cfg(unix)]
#[test]
fn git_shallow_source_missing_response_marker_rejects_clone() {
    use std::{
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    fn read_frame(stream: &mut TcpStream, context: &str) -> Vec<u8> {
        let mut header = [0u8; 4];
        stream
            .read_exact(&mut header)
            .unwrap_or_else(|error| panic!("read {context} packet header: {error}"));
        let length = usize::from_str_radix(std::str::from_utf8(&header).expect("hex header"), 16)
            .expect("packet length");
        assert!(length == 0 || (4..=0xffff).contains(&length));
        let mut frame = header.to_vec();
        if length > 0 {
            frame.resize(length, 0);
            stream
                .read_exact(&mut frame[4..])
                .unwrap_or_else(|error| panic!("read {context} packet payload: {error}"));
        }
        frame
    }

    fn read_optional_frame(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut first = [0u8; 1];
        match stream.read(&mut first) {
            Ok(0) => None,
            Ok(1) => {
                let mut header = [first[0], 0, 0, 0];
                stream
                    .read_exact(&mut header[1..])
                    .expect("read first negotiation packet header");
                let length =
                    usize::from_str_radix(std::str::from_utf8(&header).expect("hex header"), 16)
                        .expect("packet length");
                assert!(length == 0 || (4..=0xffff).contains(&length));
                let mut frame = header.to_vec();
                if length > 0 {
                    frame.resize(length, 0);
                    stream
                        .read_exact(&mut frame[4..])
                        .expect("read first negotiation packet payload");
                }
                Some(frame)
            }
            Ok(_) => unreachable!("single-byte read buffer"),
            Err(error) => panic!("read first negotiation packet header: {error}"),
        }
    }

    fn accept_with_deadline(listener: &TcpListener, deadline: Instant) -> TcpStream {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("blocking socket reads");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("set read timeout");
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .expect("set write timeout");
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "git:// client did not connect");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept git:// connection: {error}"),
            }
        }
    }

    let root = tempfile::tempdir().expect("temporary Git fixture");
    let source = root.path().join("source");
    let shallow = root.path().join("shallow.git");
    let home = root.path().join("home");
    fs::create_dir(&home).expect("isolated home");
    checked(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&source),
    );
    for (key, value) in [
        ("user.name", "Fixture"),
        ("user.email", "fixture@example.test"),
        ("commit.gpgsign", "false"),
    ] {
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["config", key, value]),
        );
    }
    for number in 1..=4 {
        fs::write(source.join("history.txt"), format!("commit {number}\n")).expect("write history");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "history.txt"]),
        );
        checked(Command::new("git").arg("-C").arg(&source).args([
            "commit",
            "-qm",
            &format!("commit {number}"),
        ]));
    }
    checked(
        Command::new("git")
            .args(["clone", "-q", "--bare", "--depth=3"])
            .arg(format!("file://{}", source.display()))
            .arg(&shallow),
    );
    let old_boundary = fs::read_to_string(shallow.join("shallow")).expect("shallow source");
    let new_boundary = String::from_utf8(
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&shallow)
                .args(["rev-parse", "HEAD^"]),
        )
        .stdout,
    )
    .expect("UTF-8 commit id")
    .trim()
    .to_owned();
    assert_ne!(old_boundary.trim(), new_boundary);

    // Snapshot the same old advertisement for both git:// connections, while
    // the real upload-pack response below reads the source with its new edge.
    let advertisement = checked(
        Command::new("git")
            .args(["upload-pack", "--stateless-rpc", "--advertise-refs"])
            .arg(&shallow),
    )
    .stdout;
    let old_marker = format!("shallow {}", old_boundary.trim());
    assert!(
        advertisement
            .windows(old_marker.len())
            .any(|part| part == old_marker.as_bytes()),
        "old shallow boundary must be advertised"
    );
    fs::write(shallow.join("shallow"), format!("{new_boundary}\n"))
        .expect("move source shallow boundary");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake git:// server");
    listener
        .set_nonblocking(true)
        .expect("bound accept deadline");
    let url = format!(
        "git://{}/repo",
        listener.local_addr().expect("git:// address")
    );
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut discoveries = 0;
        let mut fetches = 0;
        for _ in 0..6 {
            let mut stream = accept_with_deadline(&listener, deadline);
            let request = read_frame(&mut stream, "service request");
            assert!(
                request[4..].starts_with(b"git-upload-pack /repo\0"),
                "expected git-upload-pack service request"
            );
            stream
                .write_all(&advertisement)
                .expect("send old advertisement");
            let Some(mut frame) = read_optional_frame(&mut stream) else {
                discoveries += 1;
                continue;
            };
            fetches += 1;

            let mut body = Vec::new();
            let mut frame_count = 0;
            loop {
                assert!(body.len() + frame.len() <= 1024 * 1024, "bounded request");
                let done = frame.len() == 9 && &frame[4..] == b"done\n";
                body.extend_from_slice(&frame);
                if done {
                    break;
                }
                frame_count += 1;
                frame = read_frame(&mut stream, &format!("fetch negotiation #{frame_count}"));
            }
            let mut upload_pack = Command::new("git")
                .args(["upload-pack", "--stateless-rpc"])
                .arg(&shallow)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start stateless upload-pack");
            upload_pack
                .stdin
                .take()
                .expect("upload-pack stdin")
                .write_all(&body)
                .expect("send upload-pack request");
            let response = upload_pack
                .wait_with_output()
                .expect("read upload-pack result");
            assert!(
                response.status.success(),
                "upload-pack failed: {}",
                String::from_utf8_lossy(&response.stderr)
            );
            let missing_marker = format!("shallow {new_boundary}");
            assert!(
                !response
                    .stdout
                    .windows(missing_marker.len())
                    .any(|part| part == missing_marker.as_bytes()),
                "the response must omit the new shallow marker"
            );
            stream
                .write_all(&response.stdout)
                .expect("send stateless upload-pack response");
            break;
        }
        assert!(
            discoveries >= 1,
            "expected at least one discovery connection"
        );
        assert_eq!(fetches, 1, "expected exactly one fetch connection");
    });

    let clone_dest = root.path().join("failed-clone");
    let clone = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("HOME", &home)
        .current_dir(root.path())
        .args(["clone", &url])
        .arg(&clone_dest)
        .output()
        .expect("clone from changing git:// source");
    let stderr = String::from_utf8_lossy(&clone.stderr);
    assert!(
        server.join().is_ok(),
        "fake git:// server failed; clone status {:?}; stderr: {stderr}",
        clone.status
    );
    assert!(
        !clone.status.success(),
        "clone must reject missing parent: {stderr}"
    );
    assert!(
        stderr.contains("missing parent"),
        "clone must report incomplete history: {stderr}"
    );
    assert!(
        stderr.contains("LBR-NET-002"),
        "incomplete remote history must be a network protocol error: {stderr}"
    );
    assert!(
        !clone_dest.exists(),
        "failed clone must remove its destination"
    );
}

/// The server must not be able to turn an unrelated, already local commit into
/// a shallow boundary by naming it in a depth response.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_depth_response_rejects_unrelated_local_shallow_marker() {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::Response,
        routing::any,
    };

    #[derive(Clone)]
    struct ServerState {
        source: PathBuf,
        unrelated_commit: String,
        posts: Arc<AtomicUsize>,
    }

    async fn serve_upload_pack(
        State(state): State<ServerState>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request
            .uri()
            .path_and_query()
            .map(|uri| uri.as_str().to_owned());
        let is_get =
            method == "GET" && path.as_deref() == Some("/repo/info/refs?service=git-upload-pack");
        let is_post = method == "POST" && path.as_deref() == Some("/repo/git-upload-pack");
        if !is_get && !is_post {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("404 response");
        }
        let body = to_bytes(request.into_body(), 1024 * 1024)
            .await
            .expect("bounded upload-pack request");
        let response = tokio::task::spawn_blocking(move || {
            if is_get {
                let advertisement = checked(
                    Command::new("git")
                        .args(["upload-pack", "--stateless-rpc", "--advertise-refs"])
                        .arg(&state.source),
                );
                let mut wire = BytesMut::new();
                add_pkt_line_string(&mut wire, "# service=git-upload-pack\n".to_owned());
                wire.extend_from_slice(b"0000");
                wire.extend_from_slice(&advertisement.stdout);
                ("application/x-git-upload-pack-advertisement", wire.to_vec())
            } else {
                state.posts.fetch_add(1, Ordering::SeqCst);
                let mut upload_pack = Command::new("git")
                    .args(["upload-pack", "--stateless-rpc"])
                    .arg(&state.source)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("start stateless upload-pack");
                upload_pack
                    .stdin
                    .take()
                    .expect("upload-pack stdin")
                    .write_all(&body)
                    .expect("send upload-pack request");
                let response = upload_pack
                    .wait_with_output()
                    .expect("read upload-pack result");
                assert!(
                    response.status.success(),
                    "upload-pack failed: {}",
                    String::from_utf8_lossy(&response.stderr)
                );
                assert!(
                    response.stdout.windows(4).any(|part| part == b"PACK"),
                    "real upload-pack must supply a pack"
                );
                let mut wire = BytesMut::new();
                add_pkt_line_string(&mut wire, format!("shallow {}\n", state.unrelated_commit));
                wire.extend_from_slice(&response.stdout);
                ("application/x-git-upload-pack-result", wire.to_vec())
            }
        })
        .await
        .expect("upload-pack task");
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", response.0)
            .body(Body::from(response.1))
            .expect("upload-pack response")
    }

    let root = tempfile::tempdir().expect("temporary Git fixture");
    let source = root.path().join("source");
    let local = root.path().join("local");
    let home = root.path().join("home");
    fs::create_dir(&home).expect("isolated home");
    checked(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&source),
    );
    for (key, value) in [
        ("user.name", "Fixture"),
        ("user.email", "fixture@example.test"),
        ("commit.gpgsign", "false"),
    ] {
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["config", key, value]),
        );
    }
    for number in 1..=4 {
        fs::write(source.join("history.txt"), format!("commit {number}\n")).expect("write history");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "history.txt"]),
        );
        checked(Command::new("git").arg("-C").arg(&source).args([
            "commit",
            "-qm",
            &format!("commit {number}"),
        ]));
    }

    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(root.path())
            .env("HOME", &home)
            .arg("init")
            .arg(&local),
    );
    for (key, value) in [
        ("user.name", "Local Fixture"),
        ("user.email", "local@example.test"),
    ] {
        checked(
            Command::new(env!("CARGO_BIN_EXE_libra"))
                .current_dir(&local)
                .env("HOME", &home)
                .args(["config", key, value]),
        );
    }
    fs::write(local.join("unrelated.txt"), "unrelated local history\n")
        .expect("write unrelated file");
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&local)
            .env("HOME", &home)
            .args(["add", "unrelated.txt"]),
    );
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&local)
            .env("HOME", &home)
            .args(["commit", "-m", "unrelated", "--no-gpg-sign", "--no-verify"]),
    );
    let unrelated_commit = String::from_utf8(
        checked(
            Command::new(env!("CARGO_BIN_EXE_libra"))
                .current_dir(&local)
                .env("HOME", &home)
                .args(["rev-parse", "HEAD"]),
        )
        .stdout,
    )
    .expect("UTF-8 local commit ID")
    .trim()
    .to_owned();

    let posts = Arc::new(AtomicUsize::new(0));
    let state = ServerState {
        source,
        unrelated_commit: unrelated_commit.clone(),
        posts: posts.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP fixture");
    let url = format!(
        "http://{}/repo/",
        listener.local_addr().expect("HTTP address")
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/repo/{*path}", any(serve_upload_pack))
                .with_state(state),
        )
        .await
        .expect("HTTP fixture");
    });
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&local)
            .env("HOME", &home)
            .args(["remote", "add", "origin", &url]),
    );
    let fetch = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&local)
        .env("HOME", &home)
        .args(["fetch", "origin", "--depth", "2"])
        .output()
        .expect("fetch poisoned shallow response");
    let stderr = String::from_utf8_lossy(&fetch.stderr);
    assert!(
        !fetch.status.success(),
        "poisoned fetch must fail: {stderr}"
    );
    assert!(
        stderr.contains("LBR-NET-002"),
        "poisoned marker must be a network protocol error: {stderr}"
    );
    assert!(
        stderr.contains("shallow commit") && stderr.contains(&unrelated_commit),
        "error must identify the unrelated marker: {stderr}"
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1, "one real pack response");
    assert!(
        !local.join(".libra/shallow").exists(),
        "rejected marker must not update shallow metadata"
    );
    let remote_ref = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&local)
        .env("HOME", &home)
        .args(["rev-parse", "refs/remotes/origin/main"])
        .output()
        .expect("inspect remote-tracking ref");
    assert!(
        !remote_ref.status.success(),
        "rejected marker must not publish the remote-tracking ref"
    );
    let still_local = checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&local)
            .env("HOME", &home)
            .args(["rev-parse", "HEAD"]),
    );
    assert_eq!(
        String::from_utf8_lossy(&still_local.stdout).trim(),
        unrelated_commit,
        "the local commit must remain unchanged"
    );
    server.abort();
}

/// `fetch --all` retains the first remote's pack pin until FETCH_HEAD is
/// written. A second remote can deliver the same pack, so it must reuse that
/// pin without waiting on its own lock and both remote records must survive.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_fetch_all_reuses_same_pack_and_releases_keep_after_fetch_head() {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            Arc, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::Response,
        routing::any,
    };

    #[derive(Clone)]
    struct ServerState {
        source: PathBuf,
        pack_response: Arc<OnceLock<Vec<u8>>>,
        posts: Arc<AtomicUsize>,
    }

    async fn serve_upload_pack(
        State(state): State<ServerState>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request
            .uri()
            .path_and_query()
            .map(|uri| uri.as_str().to_owned());
        let is_get = method == "GET"
            && matches!(
                path.as_deref(),
                Some("/a/info/refs?service=git-upload-pack")
                    | Some("/b/info/refs?service=git-upload-pack")
            );
        let is_post = method == "POST"
            && matches!(
                path.as_deref(),
                Some("/a/git-upload-pack") | Some("/b/git-upload-pack")
            );
        if !is_get && !is_post {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("404 response");
        }
        let body = to_bytes(request.into_body(), 1024 * 1024)
            .await
            .expect("bounded upload-pack request");
        let response = tokio::task::spawn_blocking(move || {
            if is_get {
                let advertisement = checked(
                    Command::new("git")
                        .args(["upload-pack", "--stateless-rpc", "--advertise-refs"])
                        .arg(&state.source),
                );
                let mut wire = BytesMut::new();
                add_pkt_line_string(&mut wire, "# service=git-upload-pack\n".to_owned());
                wire.extend_from_slice(b"0000");
                wire.extend_from_slice(&advertisement.stdout);
                ("application/x-git-upload-pack-advertisement", wire.to_vec())
            } else {
                state.posts.fetch_add(1, Ordering::SeqCst);
                let wire = state.pack_response.get_or_init(|| {
                    let mut upload_pack = Command::new("git")
                        .args(["upload-pack", "--stateless-rpc"])
                        .arg(&state.source)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .expect("start stateless upload-pack");
                    upload_pack
                        .stdin
                        .take()
                        .expect("upload-pack stdin")
                        .write_all(&body)
                        .expect("send upload-pack request");
                    let response = upload_pack
                        .wait_with_output()
                        .expect("read upload-pack result");
                    assert!(
                        response.status.success(),
                        "upload-pack failed: {}",
                        String::from_utf8_lossy(&response.stderr)
                    );
                    assert!(
                        response.stdout.windows(4).any(|part| part == b"PACK"),
                        "real upload-pack must supply a pack"
                    );
                    response.stdout
                });
                ("application/x-git-upload-pack-result", wire.clone())
            }
        })
        .await
        .expect("upload-pack task");
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", response.0)
            .body(Body::from(response.1))
            .expect("upload-pack response")
    }

    let root = tempfile::tempdir().expect("temporary Git fixture");
    let source = root.path().join("source");
    let local = root.path().join("local");
    let home = root.path().join("home");
    fs::create_dir(&home).expect("isolated home");
    checked(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&source),
    );
    for (key, value) in [
        ("user.name", "Fixture"),
        ("user.email", "fixture@example.test"),
        ("commit.gpgsign", "false"),
    ] {
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["config", key, value]),
        );
    }
    for number in 1..=2 {
        fs::write(source.join("history.txt"), format!("commit {number}\n")).expect("write history");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "history.txt"]),
        );
        checked(Command::new("git").arg("-C").arg(&source).args([
            "commit",
            "-qm",
            &format!("commit {number}"),
        ]));
    }
    let tip = String::from_utf8(
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["rev-parse", "HEAD"]),
        )
        .stdout,
    )
    .expect("UTF-8 commit ID")
    .trim()
    .to_owned();

    let posts = Arc::new(AtomicUsize::new(0));
    let pack_response = Arc::new(OnceLock::new());
    let state = ServerState {
        source,
        pack_response: pack_response.clone(),
        posts: posts.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP fixture");
    let address = listener.local_addr().expect("HTTP address");
    let first_url = format!("http://{address}/a/");
    let second_url = format!("http://{address}/b/");
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/a/{*path}", any(serve_upload_pack))
                .route("/b/{*path}", any(serve_upload_pack))
                .with_state(state),
        )
        .await
        .expect("HTTP fixture");
    });
    checked(
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(root.path())
            .env("HOME", &home)
            .arg("init")
            .arg(&local),
    );
    for (name, url) in [("a", &first_url), ("b", &second_url)] {
        checked(
            Command::new(env!("CARGO_BIN_EXE_libra"))
                .current_dir(&local)
                .env("HOME", &home)
                .args(["remote", "add", name, url]),
        );
    }

    // A deadlock around the first remote's `.keep` lease must terminate this
    // child and fail the test within a fixed interval.
    let mut fetch = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&local)
        .env("HOME", &home)
        .args(["fetch", "--all"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start fetch --all");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if fetch.try_wait().expect("poll fetch --all").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            fetch.kill().expect("terminate hung fetch --all");
            let output = fetch.wait_with_output().expect("reap hung fetch --all");
            panic!(
                "fetch --all timed out with the same pack from two remotes: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = fetch.wait_with_output().expect("read fetch --all output");
    assert!(
        output.status.success(),
        "fetch --all failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(posts.load(Ordering::SeqCst), 2, "both remotes sent a pack");
    assert!(pack_response.get().is_some(), "same pack was replayed");

    let fetch_head = fs::read_to_string(local.join(".libra/FETCH_HEAD")).expect("FETCH_HEAD");
    let records = fetch_head.lines().collect::<Vec<_>>();
    assert_eq!(records.len(), 2, "one FETCH_HEAD entry per remote");
    for url in [&first_url, &second_url] {
        assert!(
            records.iter().any(|line| {
                line.starts_with(&format!("{tip}\tnot-for-merge\tbranch 'main' of "))
                    && line.contains(url)
            }),
            "FETCH_HEAD must record {url}: {fetch_head}"
        );
    }
    for remote in ["a", "b"] {
        let tracked = checked(
            Command::new(env!("CARGO_BIN_EXE_libra"))
                .current_dir(&local)
                .env("HOME", &home)
                .args(["rev-parse", &format!("refs/remotes/{remote}/main")]),
        );
        assert_eq!(String::from_utf8_lossy(&tracked.stdout).trim(), tip);
    }
    let pack_dir = local.join(".libra/objects/pack");
    let packs = fs::read_dir(&pack_dir)
        .expect("inspect pack directory")
        .map(|entry| entry.expect("pack entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect::<Vec<_>>();
    assert_eq!(packs.len(), 1, "both remotes shared one checksum pack");
    assert!(
        fs::read_dir(&pack_dir)
            .expect("inspect pack pins")
            .map(|entry| entry.expect("pack entry").path())
            .all(|path| !path.extension().is_some_and(|ext| ext == "keep")),
        "successful fetch --all must release its .keep pin"
    );
    server.abort();
}
