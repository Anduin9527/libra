//! plan-20260912 MB-02: human-readable directory browser TUI over the MB-01
//! validated listing. Owns canonical current path, selection, history/cache and
//! the terminal lifecycle; performs no prefetch and delegates every fetch to
//! [`Mega2TreeSession`].

pub mod terminal;

#[cfg(unix)]
pub(crate) mod terminal_unix;
#[cfg(windows)]
pub(crate) mod terminal_windows;

use std::io::{self, Read, Write};

#[cfg(unix)]
use libc::{STDIN_FILENO, STDOUT_FILENO};

use self::terminal::TerminalGuard;
use crate::{
    internal::protocol::mega2_tree::{ContentType, Listing, MAX_CACHE_ENTRIES, Mega2TreeSession},
    utils::error::{CliError, CliResult, StableErrorCode},
};

/// Logical key input after escape-sequence parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Backspace,
    Home,
    Reload,
    Quit,
    Other(char),
}

/// What the event loop must do after a key is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionResult {
    /// Fetch the current path (enter/back/reload), exactly one request.
    FetchCurrent,
    Continue,
    Quit,
}

/// Browser state: the only local truth is the canonical current path.
pub struct BrowserState {
    pub session: Mega2TreeSession,
    pub path: String,
    pub git_ref: Option<String>,
    pub selection: usize,
    pub entries: Vec<(String, ContentType)>,
    pub history: Vec<String>,
    pub status: Option<String>,
    pub running: bool,
}

impl BrowserState {
    pub fn new(session: Mega2TreeSession, path: &str, git_ref: Option<String>) -> CliResult<Self> {
        let path = crate::internal::protocol::mega2_tree::normalize_path(path)?;
        Ok(Self {
            session,
            path,
            git_ref,
            selection: 0,
            entries: Vec::new(),
            history: Vec::new(),
            status: None,
            running: true,
        })
    }

    /// Joins a validated parent path with a validated entry name.
    pub fn child_path(parent: &str, name: &str) -> String {
        if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    /// Parent of a canonical path; never climbs above `/`.
    pub fn parent_path(path: &str) -> String {
        match path.rfind('/') {
            None | Some(0) => "/".to_string(),
            Some(idx) => path[..idx].to_string(),
        }
    }

    /// One fetch per navigation action; no prefetch, no recursion.
    pub async fn fetch_current(&mut self) -> CliResult<()> {
        let listing = self
            .session
            .fetch(&self.path, self.git_ref.as_deref())
            .await?;
        self.apply_listing(listing);
        Ok(())
    }

    fn apply_listing(&mut self, listing: Listing) {
        self.entries = listing
            .entries
            .into_iter()
            .map(|e| (e.name, e.content_type))
            .collect();
        if self.selection >= self.entries.len() {
            self.selection = 0;
        }
        self.status = None;
    }

    /// Handles one key against the state; the caller performs `FetchCurrent`.
    pub fn handle_key(&mut self, key: Key) -> ActionResult {
        match key {
            Key::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                ActionResult::Continue
            }
            Key::Down => {
                if !self.entries.is_empty() && self.selection + 1 < self.entries.len() {
                    self.selection += 1;
                }
                ActionResult::Continue
            }
            Key::Enter => {
                if let Some((name, content_type)) = self.entries.get(self.selection) {
                    match content_type {
                        ContentType::Directory => {
                            let next = Self::child_path(&self.path, name);
                            self.history.push(self.path.clone());
                            // Bounded history: keep at most MAX_CACHE_ENTRIES entries.
                            if self.history.len() > MAX_CACHE_ENTRIES {
                                self.history.remove(0);
                            }
                            self.path = next;
                            self.selection = 0;
                            ActionResult::FetchCurrent
                        }
                        ContentType::File => {
                            self.status = Some(format!("file: {name} (no preview)"));
                            ActionResult::Continue
                        }
                    }
                } else {
                    ActionResult::Continue
                }
            }
            Key::Backspace | Key::Home => {
                if self.path != "/" {
                    self.path = Self::parent_path(&self.path);
                    self.selection = 0;
                    ActionResult::FetchCurrent
                } else {
                    ActionResult::Continue
                }
            }
            Key::Reload => ActionResult::FetchCurrent,
            Key::Quit => {
                self.running = false;
                ActionResult::Quit
            }
            Key::Other(_) => ActionResult::Continue,
        }
    }
}

/// Maps raw terminal bytes (after raw-mode reading) to logical keys.
pub fn parse_key(byte: u8, sequence: &[u8]) -> Key {
    match byte {
        b'\x1b' => match sequence {
            [b'[', b'A'] => Key::Up,
            [b'[', b'B'] => Key::Down,
            _ => Key::Other('?'),
        },
        b'\r' | b'\n' => Key::Enter,
        127 | 8 => Key::Backspace,
        b'q' | b'Q' => Key::Quit,
        b'r' | b'R' => Key::Reload,
        b'h' | b'H' => Key::Home,
        b'k' | b'K' => Key::Up,
        b'j' | b'J' => Key::Down,
        other => Key::Other(other as char),
    }
}

/// Prints only printable characters; anything else becomes a visible placeholder
/// so hostile names can never execute control sequences in the terminal.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Renders one frame: header, selection list (dirs first per MB-01 ordering),
/// status line. All untrusted strings pass through [`sanitize`].
pub fn render(state: &BrowserState, server: &str) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H");
    out.push_str(&format!(
        "mega2 browser — server: {server}  ref: {}  path: {}\r\n",
        state.git_ref.as_deref().unwrap_or("(default)"),
        state.path
    ));
    out.push_str("──────────────────────────────────────────────\r\n");
    if state.entries.is_empty() {
        out.push_str("(empty directory)\r\n");
    }
    for (idx, (name, content_type)) in state.entries.iter().enumerate() {
        let marker = if idx == state.selection { ">" } else { " " };
        let kind = match content_type {
            ContentType::Directory => "dir ",
            ContentType::File => "file",
        };
        out.push_str(&format!("{marker} {kind}  {}\r\n", sanitize(name)));
    }
    out.push_str("──────────────────────────────────────────────\r\n");
    let status = state
        .status
        .as_deref()
        .unwrap_or("Up/Down select · Enter open · Backspace/h back · r reload · q quit");
    out.push_str(&sanitize(status));
    out.push_str("\r\n");
    out
}

/// Requires stdin and stdout to be TTYs; refuses before any terminal change.
pub fn ensure_tty() -> CliResult<()> {
    let (stdin_tty, stdout_tty) = stdin_stdout_are_ttys();
    tty_required(stdin_tty, stdout_tty)
}

#[cfg(unix)]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    // SAFETY: STDIN_FILENO/STDOUT_FILENO are valid; isatty only reads.
    let stdin_tty = unsafe { libc::isatty(STDIN_FILENO) } == 1;
    let stdout_tty = unsafe { libc::isatty(STDOUT_FILENO) } == 1;
    (stdin_tty, stdout_tty)
}

#[cfg(windows)]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    fn is_console(handle: HANDLE) -> bool {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode: u32 = 0;
        // SAFETY: handle came from GetStdHandle; mode is writable.
        let ok = unsafe { GetConsoleMode(handle, &mut mode) };
        ok != 0
    }
    // SAFETY: STD_*_HANDLE are standard handle selectors.
    let stdin_tty = is_console(unsafe { GetStdHandle(STD_INPUT_HANDLE) });
    // SAFETY: as above.
    let stdout_tty = is_console(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });
    (stdin_tty, stdout_tty)
}

#[cfg(not(any(unix, windows)))]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    (false, false)
}

/// Pure TTY gate, testable on every platform (fail-closed seam).
pub fn tty_required(stdin_tty: bool, stdout_tty: bool) -> CliResult<()> {
    if !stdin_tty || !stdout_tty {
        return Err(CliError::fatal(
            "mega2 browser: interactive mode requires stdin and stdout to be terminals (use --json for scripts)",
        )
        .with_stable_code(StableErrorCode::Unsupported));
    }
    Ok(())
}

/// Reads one logical key from raw-mode stdin. A lone `Esc` (no sequence bytes
/// within a short window) maps to [`Key::Quit`].
pub fn read_key(stdin: &mut io::Stdin) -> io::Result<Key> {
    let mut byte = [0u8; 1];
    stdin.read_exact(&mut byte)?;
    if byte[0] != 0x1b {
        return Ok(parse_key(byte[0], &[]));
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let raw_fd = stdin.as_raw_fd();
        let mut pollfd = libc::pollfd {
            fd: raw_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to one valid descriptor entry.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
        if ready <= 0 {
            return Ok(Key::Quit); // lone Esc
        }
    }
    let mut seq = [0u8; 2];
    let n = stdin.read(&mut seq)?;
    let mut padded = [0u8; 2];
    padded[..n.min(2)].copy_from_slice(&seq[..n.min(2)]);
    if n == 0 {
        return Ok(Key::Quit);
    }
    Ok(parse_key(byte[0], &padded))
}

/// Full interactive run: TTY check → terminal guard → event loop with exactly
/// one fetch per navigation action and no background work.
pub async fn run(server: &str, start_path: &str, git_ref: Option<&str>) -> CliResult<()> {
    ensure_tty()?;
    let session = Mega2TreeSession::new(server)?;
    let mut state = BrowserState::new(session, start_path, git_ref.map(str::to_string))?;
    let mut guard = TerminalGuard::enter()?;

    state.fetch_current().await?;

    let result = (async {
        let mut stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut out_buf: Vec<u8> = Vec::with_capacity(4096);
        while state.running {
            out_buf.clear();
            out_buf.extend_from_slice(render(&state, server).as_bytes());
            stdout.write_all(&out_buf).map_err(|e| {
                CliError::fatal(format!("mega2 browser: output write failed: {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            stdout.flush().map_err(|e| {
                CliError::fatal(format!("mega2 browser: output flush failed: {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;

            let key = read_key(&mut stdin).map_err(|e| {
                CliError::fatal(format!("mega2 browser: input read failed: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            let action = state.handle_key(key);
            match action {
                ActionResult::Quit => break,
                ActionResult::FetchCurrent => {
                    if let Err(e) = state.fetch_current().await {
                        // Keep the last safe listing; show a secret-free status.
                        state.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::Continue => {}
            }
        }
        Ok::<(), CliError>(())
    })
    .await;

    // Terminal restoration is mandatory on every exit path; restoration
    // failure must not mask the primary error.
    if let Err(restore_error) = guard.restore() {
        match result {
            Ok(()) => Err(CliError::fatal(format!(
                "mega2 browser: failed to restore the terminal: {restore_error}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)),
            Err(primary) => {
                let _ = restore_error;
                Err(primary)
            }
        }
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use super::*;

    /// Minimal blocking mock of the mega2 tree route; counts requests.
    struct MockTreeServer {
        addr: std::net::SocketAddr,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl MockTreeServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
            listener.set_nonblocking(true).expect("nonblocking");
            let addr = listener.local_addr().expect("addr");
            let requests = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_clone = Arc::clone(&requests);
            let stop_clone = Arc::clone(&stop);
            thread::spawn(move || {
                let body = r#"{"req_result":true,"data":{"tree_items":[{"name":"sub","path":"/","content_type":"directory"},{"name":"file.txt","path":"/","content_type":"file"}]},"err_message":""}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                while !stop_clone.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
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

    fn state_on(server: &str) -> BrowserState {
        let session = Mega2TreeSession::new(server).expect("session");
        BrowserState::new(session, "/", None).expect("state")
    }

    #[tokio::test]
    async fn navigation_keys_fetch_exactly_once_and_files_are_inert() {
        let server = MockTreeServer::start();
        let mut state = state_on(&server.url());

        state.fetch_current().await.expect("initial listing");
        assert_eq!(server.requests(), 1);

        // Cursor motion and reload-free keys never touch the network.
        assert_eq!(state.handle_key(Key::Down), ActionResult::Continue);
        assert_eq!(state.handle_key(Key::Up), ActionResult::Continue);
        assert_eq!(server.requests(), 1);

        // Enter on the directory requests exactly one fetch once applied.
        assert_eq!(state.handle_key(Key::Enter), ActionResult::FetchCurrent);
        assert_eq!(state.path, "/sub");
        state.fetch_current().await.expect("child listing");
        assert_eq!(server.requests(), 2);

        // Enter on a file is inert (status only, no fetch).
        state.selection = 1;
        assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
        assert!(state.status.as_deref().unwrap_or("").contains("file.txt"));
        assert_eq!(server.requests(), 2);

        // Back to root requests one fetch; back at root is inert.
        assert_eq!(state.handle_key(Key::Backspace), ActionResult::FetchCurrent);
        assert_eq!(state.path, "/");
        state.fetch_current().await.expect("root listing");
        assert_eq!(server.requests(), 3);
        assert_eq!(state.handle_key(Key::Backspace), ActionResult::Continue);
        assert_eq!(server.requests(), 3);

        // Quit stops the loop without network activity.
        assert_eq!(state.handle_key(Key::Quit), ActionResult::Quit);
        assert!(!state.running);
        assert_eq!(server.requests(), 3);
    }

    #[test]
    fn path_never_climbs_above_root() {
        assert_eq!(BrowserState::parent_path("/"), "/");
        assert_eq!(BrowserState::parent_path("/a"), "/");
        assert_eq!(BrowserState::parent_path("/a/b"), "/a");
        assert_eq!(BrowserState::child_path("/", "x"), "/x");
        assert_eq!(BrowserState::child_path("/a", "b"), "/a/b");
    }

    #[test]
    fn parse_key_maps_arrows_and_control_keys() {
        assert_eq!(parse_key(0x1b, b"[A"), Key::Up);
        assert_eq!(parse_key(0x1b, b"[B"), Key::Down);
        assert_eq!(parse_key(b'\r', &[]), Key::Enter);
        assert_eq!(parse_key(127, &[]), Key::Backspace);
        assert_eq!(parse_key(8, &[]), Key::Backspace);
        assert_eq!(parse_key(b'q', &[]), Key::Quit);
        assert_eq!(parse_key(b'r', &[]), Key::Reload);
        assert_eq!(parse_key(b'h', &[]), Key::Home);
        assert_eq!(parse_key(b'x', &[]), Key::Other('x'));
    }

    #[test]
    fn hostile_text_is_sanitized_in_render() {
        assert_eq!(sanitize("a\u{1b}[31mb"), "a?[31mb");
        let session = Mega2TreeSession::new("https://example.com").expect("session");
        let mut state = BrowserState::new(session, "/", None).expect("state");
        state.entries = vec![("evil\u{1b}[2Jname".to_string(), ContentType::Directory)];
        let frame = render(&state, "https://example.com");
        assert!(!frame.contains("evil\u{1b}[2Jname"), "raw escape leaked");
        assert!(frame.contains("evil?[2Jname"), "sanitized name shown");
    }

    #[test]
    fn tty_and_platform_gates_fail_closed() {
        assert!(tty_required(false, true).is_err());
        assert!(tty_required(true, false).is_err());
        assert!(tty_required(true, true).is_ok());
        assert!(terminal::select_platform_impl(false, false).is_err());
        assert!(terminal::select_platform_impl(true, false).is_ok());
        assert!(terminal::select_platform_impl(false, true).is_ok());
    }

    #[test]
    fn tty_gate_error_is_not_an_internal_invariant() {
        let err = tty_required(false, false).expect_err("non-tty refused");
        assert_eq!(err.stable_code(), StableErrorCode::Unsupported);
        assert!(err.message().contains("requires stdin and stdout"));
    }
}
