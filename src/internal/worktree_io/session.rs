//! Parent-side lexical worktree sessions for bounded I/O requests.
//!
//! The parent never seals or opens the root. It only resolves the root once
//! per status/probe session and converts caller paths into the strict relative
//! representation carried by the wire protocol. Capability sealing remains in
//! the helper handler.

use std::{cell::RefCell, io, path::Path};

use super::protocol::{path_to_bytes, relative_worktree_path};

thread_local! {
    static ROOT_BYTES: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Resolve and cache the current worktree root for this parent-side session.
pub(crate) fn root_bytes() -> io::Result<Vec<u8>> {
    ROOT_BYTES.with(|slot| {
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

/// Convert an absolute or relative caller path to a validated wire-relative
/// path without probing the filesystem.
pub(crate) fn relative_path(root: &[u8], path: &Path, allow_root: bool) -> io::Result<Vec<u8>> {
    relative_worktree_path(root, path, allow_root).map(|relative| path_to_bytes(&relative))
}

pub(crate) fn prime(root: &Path) {
    let bytes = path_to_bytes(root);
    if bytes.is_empty() {
        return;
    }
    ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = Some(bytes);
    });
}

pub(crate) fn clear() {
    ROOT_BYTES.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// RAII guard that clears the parent-side root session when dropped.
pub(crate) struct RootGuard;

impl Drop for RootGuard {
    fn drop(&mut self) {
        clear();
    }
}

/// Begin a parent-side worktree root session.
pub(crate) fn begin() -> io::Result<RootGuard> {
    let path = crate::utils::util::try_working_dir().map_err(|error| {
        io::Error::other(format!(
            "cannot resolve worktree root for beneath I/O: {error}"
        ))
    })?;
    prime(&path);
    Ok(RootGuard)
}
