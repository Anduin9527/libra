//! Out-of-process recyclable status I/O worker (plan-20260715 WIO-01).
//!
//! Basic scan / probe syscalls run in a bounded pool of helper processes
//! (cap 8). The parent keeps a stably sorted pending queue; a stuck task is
//! killed by process group and its slot is reused. Streaming `read_dir`
//! emits `Begin` / `Record` / `Checkpoint` so a mid-stream kill keeps the
//! last checkpointed partial and marks the current edge `IoBlocked`.
//!
//! The helper entry (`--libra-internal-status-io-worker`) is handled in
//! `main` before upgrade, recovery, or any repository write. It accepts only
//! an anonymous pipe plus a capability token.

use std::{
    cell::RefCell,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// Hidden argv token. Must be the second argv element; parsed in `main` before CLI.
pub const STATUS_IO_WORKER_ARG: &str = "--libra-internal-status-io-worker";
/// Capability token env. Worker exits 2 if missing or mismatched.
pub const STATUS_IO_WORKER_CAP_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_CAP";
/// Parent pid, so a helper blocked in a syscall can still exit when status dies.
pub const STATUS_IO_WORKER_PPID_ENV: &str = "LIBRA_INTERNAL_STATUS_IO_PPID";

use crate::internal::worktree_io::executor::{WorkerConfig, WorktreeIo};
pub(crate) use crate::internal::worktree_io::protocol::{
    CapRequest, CapturedStat, Dirent, DirentKind, FRAME_CAP, IoEvent, IoRequest, ObjectBlobStatus,
    ObjectStoreCapability, ReadDirListing, WireResult, WorktreeRootCapability, bytes_to_path,
    dirent_os, io_from_wire, kind_to_u8, path_to_bytes, read_frame, unwrap_wire, wire_result,
    write_frame,
};

static WORKTREE_IO: OnceLock<WorktreeIo> = OnceLock::new();

fn worktree_io() -> &'static WorktreeIo {
    WORKTREE_IO.get_or_init(|| {
        WorktreeIo::new(WorkerConfig {
            worker_arg: STATUS_IO_WORKER_ARG,
            cap_env: STATUS_IO_WORKER_CAP_ENV,
            ppid_env: STATUS_IO_WORKER_PPID_ENV,
            in_process_handler: handle_request_to_buffer,
        })
    })
}

fn handle_request_to_buffer(request: IoRequest, stdout: &mut Vec<u8>) -> io::Result<bool> {
    handle_request(request, stdout)
}

fn seal_worktree_capability(request: &IoRequest) -> io::Result<Option<WorktreeRootCapability>> {
    let root = match request {
        IoRequest::SymlinkMetadata { root, .. }
        | IoRequest::CanonicalizePair { root, .. }
        | IoRequest::ReadDir { root, .. }
        | IoRequest::FileBlobHash { root, .. }
        | IoRequest::MarkerProbe { root, .. } => root,
        IoRequest::ReadObjectBlob { .. } | IoRequest::Shutdown => return Ok(None),
    };
    HELPER_WORKTREE_CAPABILITY.with(|slot| {
        if let Some((cached_root, capability)) = slot.borrow().as_ref()
            && cached_root == root
        {
            return Ok(Some(capability.clone()));
        }
        let capability = WorktreeRootCapability::seal(&bytes_to_path(root))?;
        *slot.borrow_mut() = Some((root.clone(), capability.clone()));
        Ok(Some(capability))
    })
}

fn seal_object_store_capability(request: &IoRequest) -> io::Result<Option<ObjectStoreCapability>> {
    let objects_root = match request {
        IoRequest::ReadObjectBlob { objects_root, .. } => objects_root,
        _ => return Ok(None),
    };
    match ObjectStoreCapability::seal(&bytes_to_path(objects_root)) {
        Ok(capability) => Ok(Some(capability)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parent_still_alive(ppid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::getppid() as u32 == ppid }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, ppid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = ppid;
        true
    }
}

fn start_parent_watchdog() {
    let Ok(ppid) = std::env::var(STATUS_IO_WORKER_PPID_ENV) else {
        return;
    };
    let Ok(ppid) = ppid.parse::<u32>() else {
        return;
    };
    if ppid == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("libra-status-io-ppid".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if !parent_still_alive(ppid) {
                    std::process::exit(1);
                }
            }
        });
}

/// Worker main: capability check, then serve framed requests until EOF.
pub fn run_worker() -> i32 {
    let expected = match std::env::var(STATUS_IO_WORKER_CAP_ENV) {
        Ok(value) if !value.is_empty() => value,
        _ => return 2,
    };
    start_parent_watchdog();
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    if write_frame(&mut stdout, &IoEvent::Ready).is_err() {
        return 1;
    }
    loop {
        let wrapped: CapRequest = match read_frame(&mut stdin) {
            Ok(wrapped) => wrapped,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return 0,
            Err(_) => return 1,
        };
        if wrapped.cap != expected {
            return 2;
        }
        match handle_request(wrapped.request, &mut stdout) {
            Ok(true) => {}
            Ok(false) => return 0,
            Err(_) => return 1,
        }
    }
}

fn handle_request(request: IoRequest, stdout: &mut impl Write) -> io::Result<bool> {
    // Requests can be supplied by the helper's pipe peer, so validate the
    // lexical root and relative paths again after deserialization. Root
    // sealing is intentionally done once below, before the read operation.
    request.validate()?;
    let worktree_capability = seal_worktree_capability(&request)?;
    let object_store_capability = seal_object_store_capability(&request)?;
    match request {
        IoRequest::Shutdown => return Ok(false),
        IoRequest::SymlinkMetadata { path, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let result = lstat_request(&path, capability);
            write_frame(stdout, &IoEvent::DoneStat { result })?;
        }
        IoRequest::CanonicalizePair { left, right, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let left_path = bytes_to_path(&left);
            let right_path = bytes_to_path(&right);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            write_frame(
                stdout,
                &IoEvent::DoneCanonicalize {
                    left: wire_result(
                        capability
                            .resolve(&left_path)
                            .and_then(|path| path.canonicalize())
                            .map(|p| path_to_bytes(&p)),
                    ),
                    right: wire_result(
                        capability
                            .resolve(&right_path)
                            .and_then(|path| path.canonicalize())
                            .map(|p| path_to_bytes(&p)),
                    ),
                },
            )?;
        }
        IoRequest::ReadDir {
            path,
            remaining,
            checkpoint_every,
            ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let listing = read_dir_request(&path, capability, remaining, checkpoint_every, stdout)?;
            write_frame(stdout, &IoEvent::DoneReadDir { listing })?;
        }
        IoRequest::FileBlobHash {
            path, hash_kind, ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let path = bytes_to_path(&path);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let relative = capability.relative(&path)?;
            // Validate all parent components through the no-follow beneath
            // walker before handing the path to the existing hash routine.
            // A symlink leaf is intentionally retained: Git hashes its link
            // target bytes, while an interior symlink is rejected.
            let root_fd = crate::utils::beneath::open_root(capability.root())?;
            crate::utils::beneath::lstat_beneath(&root_fd, &relative)?;
            let path = if std::env::var_os(STATUS_IO_WORKER_CAP_ENV).is_some() {
                // The standalone helper has an isolated process, so changing
                // its CWD lets Git's attribute lookup discover the request's
                // `.gitattributes` without exposing an absolute path to the
                // hash routine. In-process callers keep their CWD untouched.
                std::env::set_current_dir(capability.root())?;
                relative
            } else {
                capability.resolve(&relative)?
            };
            apply_hash_kind(&hash_kind);
            let result = match crate::command::calc_file_blob_hash(&path) {
                Ok(hash) => WireResult::Ok(hash.to_string()),
                Err(error) => WireResult::Err {
                    kind: kind_to_u8(error.kind()),
                    raw_os: error.raw_os_error(),
                },
            };
            write_frame(stdout, &IoEvent::DoneHash { hex: result })?;
        }
        IoRequest::ReadObjectBlob {
            oid,
            byte_limit,
            hash_kind,
            ..
        } => {
            write_frame(stdout, &IoEvent::Begin)?;
            maybe_test_slow_object_read(&oid);
            apply_hash_kind(&hash_kind);
            let outcome = match object_store_capability.as_ref() {
                Some(capability) => read_object_blob_request(&oid, capability, byte_limit),
                None => Err(ObjectBlobStatus::Unavailable),
            };
            write_object_blob_outcome(stdout, outcome)?;
        }
        IoRequest::MarkerProbe { dir, .. } => {
            write_frame(stdout, &IoEvent::Begin)?;
            let dir = bytes_to_path(&dir);
            let Some(capability) = worktree_capability.as_ref() else {
                return Err(io::Error::other("missing worktree capability"));
            };
            let (present, err_kind, err_raw_os) = marker_probe_request(&dir, capability);
            write_frame(
                stdout,
                &IoEvent::DoneMarker {
                    present,
                    err_kind,
                    err_raw_os,
                },
            )?;
        }
    }
    Ok(true)
}

fn request_root_bytes() -> io::Result<Vec<u8>> {
    STATUS_IO_ROOT_BYTES.with(|slot| {
        if let Some(bytes) = slot.borrow().as_ref() {
            return Ok(bytes.clone());
        }
        let path = crate::utils::util::try_working_dir().map_err(|error| {
            io::Error::other(format!(
                "cannot resolve worktree root for beneath I/O: {error}"
            ))
        })?;
        let bytes = path_to_bytes(&path);
        if bytes.is_empty() {
            return Err(io::Error::other(
                "worktree root resolved empty for beneath I/O",
            ));
        }
        *slot.borrow_mut() = Some(bytes.clone());
        Ok(bytes)
    })
}

/// Convert the status layer's historical absolute paths into the strict
/// relative representation carried on the wire. This is deliberately done at
/// submission time as well as in the helper after decoding, so malformed
/// requests never enter the queue or get serialized.
fn request_worktree_relative(root: &[u8], path: &Path, allow_root: bool) -> io::Result<Vec<u8>> {
    let capability = request_worktree_capability(root)?;
    let relative = if path.is_absolute() {
        capability.relative_from_absolute(path)?
    } else if allow_root {
        capability.relative_or_root(path)?
    } else {
        capability.relative(path)?
    };
    Ok(path_to_bytes(&relative))
}

thread_local! {
    /// The helper is a long-lived single-threaded request loop. Cache the
    /// lexical root key and its sealed capability between requests, while
    /// keeping every actual read behind a fresh `beneath::open_root` call.
    /// A different wire root cannot reuse the previous capability.
    static HELPER_WORKTREE_CAPABILITY:
        RefCell<Option<(Vec<u8>, WorktreeRootCapability)>> = const { RefCell::new(None) };
    /// Parent-side worktree root for beneath requests. Resolved once per
    /// status session so `deadline_stat` / `read_dir` do not re-walk the
    /// repository ancestry for every path.
    static STATUS_IO_ROOT_BYTES: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    /// Sealed once at session start and reused for lexical path conversion.
    /// Actual reads still open a fresh root through `beneath`.
    static STATUS_IO_ROOT_CAPABILITY: RefCell<Option<WorktreeRootCapability>> =
        const { RefCell::new(None) };
}

fn request_worktree_capability(root: &[u8]) -> io::Result<WorktreeRootCapability> {
    STATUS_IO_ROOT_CAPABILITY.with(|slot| {
        if let Some(capability) = slot.borrow().as_ref().cloned()
            && path_to_bytes(capability.root()) == root
        {
            return Ok(capability);
        }
        let capability = WorktreeRootCapability::seal(&bytes_to_path(root))?;
        *slot.borrow_mut() = Some(capability.clone());
        Ok(capability)
    })
}

/// Prime the parent-side worktree-root cache for a status/probe session.
pub(crate) fn prime_status_io_root_cache(root: &Path) {
    let capability = WorktreeRootCapability::seal(root).ok();
    let bytes = capability
        .as_ref()
        .map(|capability| path_to_bytes(capability.root()))
        .unwrap_or_else(|| path_to_bytes(root));
    if bytes.is_empty() {
        return;
    }
    STATUS_IO_ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = Some(bytes);
    });
    STATUS_IO_ROOT_CAPABILITY.with(|slot| {
        *slot.borrow_mut() = capability;
    });
}

/// Drop the parent-side worktree-root cache at the end of a status session.
pub(crate) fn clear_status_io_root_cache() {
    STATUS_IO_ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = None;
    });
    STATUS_IO_ROOT_CAPABILITY.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// RAII session that primes [`prime_status_io_root_cache`] and clears on drop.
pub(crate) struct StatusIoRootGuard;

impl Drop for StatusIoRootGuard {
    fn drop(&mut self) {
        clear_status_io_root_cache();
    }
}

/// Begin a status I/O session: resolve the worktree root once for all
/// subsequent beneath requests on this thread.
pub(crate) fn begin_status_io_root_session() -> io::Result<StatusIoRootGuard> {
    let path = crate::utils::util::try_working_dir().map_err(|error| {
        io::Error::other(format!(
            "cannot resolve worktree root for beneath I/O: {error}"
        ))
    })?;
    prime_status_io_root_cache(&path);
    Ok(StatusIoRootGuard)
}

fn lstat_request(path: &Path, capability: &WorktreeRootCapability) -> WireResult<CapturedStat> {
    let rel = match capability.relative_or_root(path) {
        Ok(rel) => rel,
        Err(error) => {
            return WireResult::Err {
                kind: kind_to_u8(error.kind()),
                raw_os: error.raw_os_error(),
            };
        }
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::lstat_beneath(&fd, &rel))
    {
        Ok(raw) => WireResult::Ok(CapturedStat::from_raw_lstat(&raw)),
        Err(error) => WireResult::Err {
            kind: kind_to_u8(error.kind()),
            raw_os: error.raw_os_error(),
        },
    }
}

fn marker_probe_request(
    dir: &Path,
    capability: &WorktreeRootCapability,
) -> (Option<bool>, Option<u8>, Option<i32>) {
    let rel = match capability.relative(dir) {
        Ok(rel) => rel,
        Err(error) => return (None, Some(kind_to_u8(error.kind())), error.raw_os_error()),
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::marker_present_beneath(&fd, &rel))
    {
        Ok(present) => (Some(present), None, None),
        Err(error) => (None, Some(kind_to_u8(error.kind())), error.raw_os_error()),
    }
}

fn read_dir_request(
    path: &Path,
    capability: &WorktreeRootCapability,
    remaining: usize,
    checkpoint_every: u32,
    stdout: &mut impl Write,
) -> io::Result<ReadDirListing> {
    let mut listing = ReadDirListing {
        entries: Vec::new(),
        error_kinds: Vec::new(),
        taken: 0,
        hit_cap: false,
        timed_out: false,
    };
    let rel = match capability.relative_or_root(path) {
        Ok(rel) => rel,
        Err(error) => {
            listing
                .error_kinds
                .push((kind_to_u8(error.kind()), error.raw_os_error()));
            listing.entries.clear();
            return Ok(listing);
        }
    };
    match crate::utils::beneath::open_root(capability.root())
        .and_then(|fd| crate::utils::beneath::open_beneath(&fd, &rel))
        .and_then(crate::utils::beneath::read_dir_fd)
    {
        Err(error) => {
            listing
                .error_kinds
                .push((kind_to_u8(error.kind()), error.raw_os_error()));
        }
        Ok(reader) => {
            emit_read_dir(
                reader.map(|entry| entry.map(|entry| Dirent::from_fd_dirent(&entry))),
                &capability.resolve(&rel)?,
                remaining,
                checkpoint_every,
                &mut listing,
                stdout,
            )?;
        }
    }
    listing.entries.clear();
    Ok(listing)
}

fn emit_read_dir<I>(
    reader: I,
    #[cfg_attr(not(debug_assertions), allow(unused_variables))] path: &Path,
    remaining: usize,
    checkpoint_every: u32,
    listing: &mut ReadDirListing,
    stdout: &mut impl Write,
) -> io::Result<()>
where
    I: Iterator<Item = io::Result<Dirent>>,
{
    let mut seq = 0u64;
    let mut records = 0u64;
    let every = checkpoint_every.max(1);
    #[cfg(debug_assertions)]
    let mut injected_notfound = false;
    for entry in reader {
        #[cfg(debug_assertions)]
        let entry = if !injected_notfound
            && std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
            && std::env::var("LIBRA_TEST_READDIR_ENTRY_NOTFOUND_DIR")
                .is_ok_and(|target| path.ends_with(&target))
        {
            injected_notfound = true;
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "injected vanished entry",
            ))
        } else {
            entry
        };
        listing.taken += 1;
        if listing.taken > remaining {
            listing.hit_cap = true;
            break;
        }
        match entry {
            Ok(dirent) => {
                write_frame(stdout, &IoEvent::RecordDirent(dirent))?;
                records += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                write_frame(
                    stdout,
                    &IoEvent::RecordError {
                        kind: kind_to_u8(error.kind()),
                        raw_os: error.raw_os_error(),
                    },
                )?;
                listing
                    .error_kinds
                    .push((kind_to_u8(error.kind()), error.raw_os_error()));
                break;
            }
        }
        if records > 0 && (records as u32).is_multiple_of(every) {
            seq += 1;
            write_frame(stdout, &IoEvent::Checkpoint { seq, records })?;
            maybe_test_kill_after_checkpoint(seq);
        }
        #[cfg(debug_assertions)]
        if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
            && std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_DIR")
                .is_ok_and(|target| path.ends_with(&target))
        {
            let kind = match std::env::var("LIBRA_TEST_READDIR_ITER_ERROR_KIND").as_deref() {
                Ok("timedout") => io::ErrorKind::TimedOut,
                _ => io::ErrorKind::Other,
            };
            write_frame(
                stdout,
                &IoEvent::RecordError {
                    kind: kind_to_u8(kind),
                    raw_os: None,
                },
            )?;
            listing.error_kinds.push((kind_to_u8(kind), None));
            break;
        }
    }
    Ok(())
}

fn apply_hash_kind(kind: &str) {
    match kind {
        "sha256" => git_internal::hash::set_hash_kind(git_internal::hash::HashKind::Sha256),
        _ => git_internal::hash::set_hash_kind(git_internal::hash::HashKind::Sha1),
    }
}

fn maybe_test_kill_after_checkpoint(seq: u64) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return;
    }
    let Ok(wanted) = std::env::var("LIBRA_TEST_STATUS_IO_KILL_AFTER_CHECKPOINT") else {
        return;
    };
    let Ok(wanted) = wanted.parse::<u64>() else {
        return;
    };
    if seq == wanted {
        std::process::exit(99);
    }
}

/// Debug seam: sleep before a local object read so WIO-03 can prove the
/// parent kills the helper when the batch deadline elapses mid-read.
fn maybe_test_slow_object_read(oid: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return;
    }
    let Ok(ms) = std::env::var("LIBRA_TEST_SLOW_OBJECT_READ_MS") else {
        return;
    };
    let Ok(ms) = ms.parse::<u64>() else {
        return;
    };
    if let Ok(wanted) = std::env::var("LIBRA_TEST_SLOW_OBJECT_READ_OID")
        && !wanted.is_empty()
        && wanted != oid
    {
        return;
    }
    std::thread::sleep(Duration::from_millis(ms));
}

fn read_object_blob_request(
    oid: &str,
    object_capability: &ObjectStoreCapability,
    byte_limit: u64,
) -> Result<Vec<u8>, ObjectBlobStatus> {
    use crate::utils::client_storage::{ClientStorage, ObjectReadFailure};

    let Ok(hash) = oid.parse::<git_internal::hash::ObjectHash>() else {
        return Err(ObjectBlobStatus::Failed);
    };
    // Local-only + alternates, no directory creation / remote hydrate
    // (WIO-03 security AC).
    let storage =
        ClientStorage::init_local_existing_with_alternates(object_capability.root().to_path_buf());
    match storage.get_with_limit(&hash, byte_limit) {
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(match ClientStorage::classify_read_failure(&error) {
            ObjectReadFailure::Missing => ObjectBlobStatus::Missing,
            ObjectReadFailure::Corrupt => ObjectBlobStatus::Corrupt,
            ObjectReadFailure::Unavailable => ObjectBlobStatus::Unavailable,
            ObjectReadFailure::TooLarge => ObjectBlobStatus::TooLarge,
            ObjectReadFailure::Other => ObjectBlobStatus::Failed,
        }),
    }
}

fn write_object_blob_outcome(
    writer: &mut impl Write,
    outcome: Result<Vec<u8>, ObjectBlobStatus>,
) -> io::Result<()> {
    match outcome {
        Ok(bytes) => {
            // Decide the over-cap case BEFORE the Ok header goes out: a
            // blob past FRAME_CAP used to fail inside `write_raw_frame`
            // AFTER `Ok` was already written, leaving the parent blocked on
            // a raw frame that never arrives (indistinguishable from a hung
            // read until the deadline kill). Reporting `TooLarge` up front
            // keeps the stream consistent and lets callers with a byte
            // limit above the frame cap (diff, W5-09) fall back promptly.
            if bytes.len() > FRAME_CAP {
                return write_frame(
                    writer,
                    &IoEvent::DoneObjectBlob {
                        status: ObjectBlobStatus::TooLarge,
                        bytes: None,
                    },
                );
            }
            write_frame(
                writer,
                &IoEvent::DoneObjectBlob {
                    status: ObjectBlobStatus::Ok,
                    bytes: None,
                },
            )?;
            write_raw_frame(writer, &bytes)
        }
        Err(status) => write_frame(
            writer,
            &IoEvent::DoneObjectBlob {
                status,
                bytes: None,
            },
        ),
    }
}

fn write_raw_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    crate::internal::worktree_io::protocol::write_raw_frame(writer, payload)
}

fn current_hash_kind() -> String {
    match git_internal::hash::get_hash_kind() {
        git_internal::hash::HashKind::Sha256 => "sha256".to_string(),
        _ => "sha1".to_string(),
    }
}

pub(crate) fn deadline_stat(path: &Path) -> Result<io::Result<CapturedStat>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let relative = match request_worktree_relative(&root, path, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::SymlinkMetadata {
                path: relative,
                root,
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
    for event in events {
        if let IoEvent::DoneStat { result } = event {
            return Ok(unwrap_wire(result));
        }
    }
    Err(())
}

pub(crate) fn deadline_canonicalize_pair(
    left: &Path,
    right: &Path,
) -> Result<(io::Result<PathBuf>, io::Result<PathBuf>), ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => {
            return Ok((
                Err(error),
                Err(io::Error::other("worktree root unavailable")),
            ));
        }
    };
    let left_relative = match request_worktree_relative(&root, left, true) {
        Ok(relative) => relative,
        Err(error) => {
            let second = io::Error::new(error.kind(), error.to_string());
            return Ok((Err(error), Err(second)));
        }
    };
    let right_relative = match request_worktree_relative(&root, right, true) {
        Ok(relative) => relative,
        Err(error) => {
            let first = io::Error::new(error.kind(), error.to_string());
            return Ok((Err(first), Err(error)));
        }
    };
    let events = worktree_io()
        .submit(
            IoRequest::CanonicalizePair {
                left: left_relative,
                right: right_relative,
                root,
            },
            path_to_bytes(left),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
    for event in events {
        if let IoEvent::DoneCanonicalize { left, right } = event {
            return Ok((
                unwrap_wire(left).map(|bytes| bytes_to_path(&bytes)),
                unwrap_wire(right).map(|bytes| bytes_to_path(&bytes)),
            ));
        }
    }
    Err(())
}

pub(crate) fn deadline_read_dir(
    path: &Path,
    remaining: usize,
    progress: &AtomicUsize,
) -> Result<io::Result<ReadDirListing>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let relative = match request_worktree_relative(&root, path, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::ReadDir {
                path: relative,
                root,
                remaining,
                checkpoint_every: 32,
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ());
    match events {
        Err(()) => Err(()),
        Ok(events) => {
            let mut partial = ReadDirListing {
                entries: Vec::new(),
                error_kinds: Vec::new(),
                taken: 0,
                hit_cap: false,
                timed_out: false,
            };
            let mut complete = false;
            for event in events {
                match event {
                    IoEvent::RecordDirent(dirent) => {
                        progress.fetch_add(1, Ordering::SeqCst);
                        partial.taken += 1;
                        partial.entries.push(dirent);
                    }
                    IoEvent::RecordError { kind, raw_os } => {
                        progress.fetch_add(1, Ordering::SeqCst);
                        partial.taken += 1;
                        partial.error_kinds.push((kind, raw_os));
                    }
                    IoEvent::DoneReadDir { listing } => {
                        if listing.entries.is_empty()
                            && listing.error_kinds.len() == 1
                            && listing.taken == 0
                            && partial.taken == 0
                        {
                            let (kind, raw_os) = listing.error_kinds[0];
                            return Ok(Err(io_from_wire(kind, raw_os)));
                        }
                        if !listing.entries.is_empty() {
                            partial.entries = listing.entries;
                        }
                        if !listing.error_kinds.is_empty() {
                            partial.error_kinds = listing.error_kinds;
                        }
                        partial.hit_cap = listing.hit_cap;
                        if listing.taken > partial.taken {
                            partial.taken = listing.taken;
                        }
                        complete = true;
                    }
                    _ => {}
                }
            }
            if complete {
                Ok(Ok(partial))
            } else if partial.taken > 0 || !partial.error_kinds.is_empty() {
                partial.timed_out = true;
                Ok(Ok(partial))
            } else {
                Err(())
            }
        }
    }
}

/// Use the worker-side `file_type()` when present; otherwise one killable
/// `deadline_stat` for that single name (DT_UNKNOWN / `file_type` error).
pub(crate) fn deadline_dirent_kind(
    path: &Path,
    dirent: &Dirent,
) -> Result<io::Result<DirentKind>, ()> {
    if dirent.type_ok {
        return Ok(Ok(DirentKind {
            is_dir: dirent.is_dir,
            is_file: dirent.is_file,
            is_symlink: dirent.is_symlink,
        }));
    }
    match deadline_stat(path) {
        Err(()) => Err(()),
        Ok(Err(error)) => Ok(Err(error)),
        Ok(Ok(stat)) => Ok(Ok(DirentKind {
            is_dir: stat.is_dir(),
            is_file: stat.is_file(),
            is_symlink: stat.is_symlink(),
        })),
    }
}

pub(crate) fn deadline_file_blob_hash(
    path: &Path,
    workdir: &Path,
) -> Result<io::Result<git_internal::hash::ObjectHash>, ()> {
    let root = path_to_bytes(workdir);
    let relative = match request_worktree_relative(&root, path, false) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::FileBlobHash {
                path: relative,
                root,
                hash_kind: current_hash_kind(),
            },
            path_to_bytes(path),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
    for event in events {
        if let IoEvent::DoneHash { hex } = event {
            return Ok(unwrap_wire(hex).and_then(|hex| {
                hex.parse::<git_internal::hash::ObjectHash>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
            }));
        }
    }
    Err(())
}

/// Outcome of a killable local object-store read (WIO-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectBlobOutcome {
    Bytes(Vec<u8>),
    Missing,
    Corrupt,
    Unavailable,
    TooLarge,
    Failed,
}

/// Read a peeled OID from `objects_root` under `timeout`. On the `libra` CLI,
/// a hung store read kills the helper process group and returns `Err(())` so
/// the caller can map the edge to a metadata skip without stalling the batch.
///
/// Library / `cargo test` harness binaries cannot spawn the CLI helper. Those
/// callers read locally on the **caller** thread (R0 mid-read hang semantics)
/// instead of occupying a dispatcher slot with an unkillable syscall
/// (WIO-03 Codex: pool-slot leak under hung mounts).
pub(crate) fn deadline_read_object_blob(
    oid: &git_internal::hash::ObjectHash,
    objects_root: &Path,
    byte_limit: u64,
    timeout: Duration,
) -> Result<ObjectBlobOutcome, ()> {
    if timeout.is_zero() {
        return Err(());
    }
    if !worktree_io().helper_available() {
        let outcome = match ObjectStoreCapability::seal(objects_root) {
            Ok(capability) => read_object_blob_request(&oid.to_string(), &capability, byte_limit),
            Err(_) => Err(ObjectBlobStatus::Unavailable),
        };
        return Ok(object_blob_outcome_from_status(outcome));
    }
    let oid_hex = oid.to_string();
    let events = worktree_io()
        .submit_absolute(
            IoRequest::ReadObjectBlob {
                oid: oid_hex.clone(),
                objects_root: path_to_bytes(objects_root),
                byte_limit,
                hash_kind: current_hash_kind(),
            },
            oid_hex.into_bytes(),
            timeout,
        )
        .map_err(|_| ())?;
    for event in events {
        if let IoEvent::DoneObjectBlob { status, bytes } = event {
            return Ok(match status {
                ObjectBlobStatus::Ok => match bytes {
                    Some(bytes) => ObjectBlobOutcome::Bytes(bytes),
                    // Wire claimed Ok but the trailing binary frame was
                    // lost — treat as corrupt rather than silently empty.
                    None => ObjectBlobOutcome::Corrupt,
                },
                other => object_blob_outcome_from_status(Err(other)),
            });
        }
    }
    Err(())
}

fn object_blob_outcome_from_status(
    outcome: Result<Vec<u8>, ObjectBlobStatus>,
) -> ObjectBlobOutcome {
    match outcome {
        Ok(bytes) => ObjectBlobOutcome::Bytes(bytes),
        Err(ObjectBlobStatus::Ok) => ObjectBlobOutcome::Corrupt,
        Err(ObjectBlobStatus::Missing) => ObjectBlobOutcome::Missing,
        Err(ObjectBlobStatus::Corrupt) => ObjectBlobOutcome::Corrupt,
        Err(ObjectBlobStatus::Unavailable) => ObjectBlobOutcome::Unavailable,
        Err(ObjectBlobStatus::TooLarge) => ObjectBlobOutcome::TooLarge,
        Err(ObjectBlobStatus::Failed) => ObjectBlobOutcome::Failed,
    }
}

pub(crate) fn deadline_marker_probe(dir: &Path) -> Result<Result<bool, io::Error>, ()> {
    let root = match request_root_bytes() {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let relative = match request_worktree_relative(&root, dir, true) {
        Ok(relative) => relative,
        Err(error) => return Ok(Err(error)),
    };
    let events = worktree_io()
        .submit(
            IoRequest::MarkerProbe {
                dir: relative,
                root,
            },
            path_to_bytes(dir),
            crate::command::status_probe::io_op_timeout(),
        )
        .map_err(|_| ())?;
    for event in events {
        if let IoEvent::DoneMarker {
            present,
            err_kind,
            err_raw_os,
        } = event
        {
            if let Some(kind) = err_kind {
                return Ok(Err(io_from_wire(kind, err_raw_os)));
            }
            return Ok(Ok(present.unwrap_or(false)));
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_frame_rejects_payload_above_cap_without_writing() {
        let event = super::IoEvent::Error {
            message: "x".repeat(super::FRAME_CAP),
        };
        let mut wire = Vec::new();

        let error = super::write_frame(&mut wire, &event).expect_err("frame must be capped");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(wire.is_empty(), "oversized frame must not write a header");
    }

    #[test]
    fn helper_rejects_absolute_worktree_request_paths() {
        let root = tempfile::tempdir().expect("create worktree root");
        let request = super::IoRequest::ReadDir {
            path: super::path_to_bytes(&root.path().join("outside")),
            root: super::path_to_bytes(root.path()),
            remaining: 1,
            checkpoint_every: 1,
        };
        let mut wire = Vec::new();

        let error = super::handle_request(request, &mut wire)
            .expect_err("helper must reject absolute worktree paths");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(wire.is_empty(), "invalid requests must not emit events");
    }

    #[test]
    fn helper_reseals_when_the_wire_root_changes() {
        let root_a = tempfile::tempdir().expect("create first worktree root");
        std::fs::create_dir(root_a.path().join("nested")).expect("first nested directory");
        std::fs::create_dir(root_a.path().join("nested/.libra")).expect("first marker");
        let root_b = tempfile::tempdir().expect("create second worktree root");
        std::fs::create_dir(root_b.path().join("nested")).expect("second nested directory");

        fn probe(root: &std::path::Path) -> Option<bool> {
            let request = super::IoRequest::MarkerProbe {
                dir: b"nested".to_vec(),
                root: super::path_to_bytes(root),
            };
            let mut wire = Vec::new();
            super::handle_request(request, &mut wire).expect("marker probe");
            let events = crate::internal::worktree_io::protocol::parse_event_frames(&wire)
                .expect("marker probe frames");
            events.into_iter().find_map(|event| match event {
                super::IoEvent::DoneMarker { present, .. } => present,
                _ => None,
            })
        }

        assert_eq!(probe(root_a.path()), Some(true));
        assert_eq!(probe(root_b.path()), Some(false));
    }

    #[test]
    fn raw_frame_rejects_payload_above_cap_before_writing_or_reading() {
        let oversized = vec![0u8; super::FRAME_CAP + 1];
        let mut wire = Vec::new();

        let write_error =
            super::write_raw_frame(&mut wire, &oversized).expect_err("raw frame must be capped");
        assert_eq!(write_error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            wire.is_empty(),
            "oversized raw frame must not write a header"
        );

        let invalid_wire = (super::FRAME_CAP as u32 + 1).to_le_bytes().to_vec();
        let read_error =
            crate::internal::worktree_io::protocol::read_raw_frame(&mut &invalid_wire[..])
                .expect_err("raw frame reader must reject an oversized length");
        assert_eq!(read_error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_object_blob_is_reported_without_an_ok_raw_frame() {
        let mut wire = Vec::new();
        super::write_object_blob_outcome(&mut wire, Ok(vec![0u8; super::FRAME_CAP + 1]))
            .expect("oversized object must produce a status event");

        let events = crate::internal::worktree_io::protocol::parse_event_frames(&wire)
            .expect("status event must remain framed");
        assert!(matches!(
            events.as_slice(),
            [super::IoEvent::DoneObjectBlob {
                status: super::ObjectBlobStatus::TooLarge,
                bytes: None,
            }]
        ));
    }

    #[test]
    fn wire_errors_preserve_kind_and_raw_os_error() {
        let kind_only = super::wire_result::<()>(Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        )));
        let super::WireResult::Err { kind, raw_os } = &kind_only else {
            panic!("permission error must be encoded as an error");
        };
        assert_eq!(*kind, 1);
        assert_eq!(*raw_os, None);
        let decoded = super::unwrap_wire(kind_only).expect_err("wire error must decode");
        assert_eq!(decoded.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(decoded.raw_os_error(), None);

        let source = std::io::Error::from_raw_os_error(2);
        let source_kind = source.kind();
        let with_raw = super::wire_result::<()>(Err(source));
        let super::WireResult::Err { kind, raw_os } = &with_raw else {
            panic!("raw OS error must be encoded as an error");
        };
        assert_eq!(*kind, super::kind_to_u8(source_kind));
        assert_eq!(*raw_os, Some(2));
        let decoded = super::unwrap_wire(with_raw).expect_err("wire error must decode");
        assert_eq!(decoded.kind(), source_kind);
        assert_eq!(decoded.raw_os_error(), Some(2));
    }

    #[test]
    fn absolute_deadline_cancels_job_without_leaking_pending_entry() {
        let path_key = b"unit-test-expired-status-io-job".to_vec();
        let outcome = super::worktree_io().submit_absolute(
            super::IoRequest::Shutdown,
            path_key.clone(),
            std::time::Duration::ZERO,
        );

        assert!(matches!(
            outcome,
            Err(crate::internal::worktree_io::executor::ExecutorError::DeadlineExpired)
        ));
        let _ = path_key;
    }

    #[test]
    fn captured_stat_round_trips_a_regular_file() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let meta = std::fs::metadata(&manifest).expect("Cargo.toml");
        let captured = super::CapturedStat::from_metadata(&meta);
        assert!(captured.is_file());
        assert!(!captured.is_dir());
        assert!(!captured.is_symlink());
        assert!(captured.len() > 0);
    }

    #[test]
    fn dirent_captures_file_type_from_readdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("a.txt"), b"x").expect("file");
        std::fs::create_dir(tmp.path().join("d")).expect("dir");
        let mut seen_file = false;
        let mut seen_dir = false;
        for entry in std::fs::read_dir(tmp.path()).expect("read_dir") {
            let dirent = super::Dirent::from_dir_entry(&entry.expect("entry"));
            assert!(dirent.type_ok, "readdir file_type must succeed here");
            seen_file |= dirent.is_file;
            seen_dir |= dirent.is_dir;
        }
        assert!(seen_file && seen_dir);
    }

    #[test]
    #[serial_test::serial]
    fn file_blob_hash_helper_uses_request_workdir_not_spawn_cwd() {
        use std::{ffi::OsString, path::Path};

        use git_internal::internal::object::blob::Blob;

        struct CapEnvGuard(Option<OsString>);
        impl Drop for CapEnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match &self.0 {
                        Some(value) => {
                            std::env::set_var(super::STATUS_IO_WORKER_CAP_ENV, value);
                        }
                        None => std::env::remove_var(super::STATUS_IO_WORKER_CAP_ENV),
                    }
                }
            }
        }
        struct HashKindGuard(git_internal::hash::HashKind);
        impl Drop for HashKindGuard {
            fn drop(&mut self) {
                git_internal::hash::set_hash_kind(self.0);
            }
        }

        fn fake_repo(label: &str) -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{label}: {error}"));
            let libra = dir.path().join(".libra");
            std::fs::create_dir(&libra).unwrap_or_else(|error| panic!("{label} .libra: {error}"));
            std::fs::write(libra.join("libra.db"), b"").expect("libra.db");
            dir
        }

        let repo_a = fake_repo("A");
        let repo_b = fake_repo("B");
        std::fs::write(repo_b.path().join(".gitattributes"), "*.bin filter=lfs\n")
            .expect("gitattributes");
        let payload = b"payload\n";
        let file_b = repo_b.path().join("tracked.bin");
        std::fs::write(&file_b, payload).expect("tracked.bin");

        let _cwd = crate::utils::test::ChangeDirGuard::new(repo_a.path());
        let _cap = CapEnvGuard(std::env::var_os(super::STATUS_IO_WORKER_CAP_ENV));
        let _hash = HashKindGuard(git_internal::hash::get_hash_kind());
        unsafe {
            std::env::set_var(super::STATUS_IO_WORKER_CAP_ENV, "test-cap");
        }

        let request = super::IoRequest::FileBlobHash {
            path: super::path_to_bytes(Path::new("tracked.bin")),
            root: super::path_to_bytes(repo_b.path()),
            hash_kind: "sha1".to_string(),
        };
        let mut buf = Vec::new();
        super::handle_request(request, &mut buf).expect("handle_request");

        let events =
            crate::internal::worktree_io::protocol::parse_event_frames(&buf).expect("frames");
        let hex = events.into_iter().find_map(|event| match event {
            super::IoEvent::DoneHash {
                hex: super::WireResult::Ok(hex),
            } => Some(hex),
            _ => None,
        });
        let hex = hex.expect("DoneHash ok");
        let (pointer, _) = crate::utils::lfs::generate_pointer_file(&file_b);
        let lfs_hash = Blob::from_content(&pointer).id.to_string();
        let content_hash = Blob::from_content_bytes(payload.to_vec()).id.to_string();
        assert_eq!(
            hex, lfs_hash,
            "helper must hash via B's LFS attrs, not spawn CWD A"
        );
        assert_ne!(lfs_hash, content_hash, "sanity: LFS pointer ≠ content");
    }
}
