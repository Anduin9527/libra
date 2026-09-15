//! Packet-line helpers and service type parsing for the Git smart protocol, covering both
//! `upload-pack` (fetch/clone) and `receive-pack` (push) flows.
//!
//! The Git smart protocol frames every payload as a sequence of `pkt-line` records: a
//! 4-byte ASCII hex length header followed by `length - 4` bytes of payload. A length of
//! `0000` is a flush marker. This module exposes the minimum primitives required to read
//! and write these frames and to identify which side of the protocol a request targets.

use core::fmt;
use std::str::FromStr;

use bytes::{Buf, BufMut};
use git_internal::errors::GitError;

/// Identifies the direction of a smart-protocol exchange.
///
/// Used by HTTP routers and SSH dispatchers to pick the correct backend handler.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ServiceType {
    /// Server-to-client transfer: clone, fetch, ls-remote.
    UploadPack,
    /// Client-to-server transfer: push.
    ReceivePack,
}

impl fmt::Display for ServiceType {
    /// Render the variant as the on-the-wire service name expected by Git clients
    /// (e.g. the `service=` query parameter in `info/refs`).
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ServiceType::UploadPack => write!(f, "git-upload-pack"),
            ServiceType::ReceivePack => write!(f, "git-receive-pack"),
        }
    }
}

impl FromStr for ServiceType {
    type Err = GitError;

    /// Parse a wire-format service name back into a `ServiceType`.
    ///
    /// Boundary conditions:
    /// - Comparison is case-sensitive — `"git-upload-pack"` and `"git-receive-pack"` are
    ///   the only accepted strings.
    /// - Any other input returns `GitError::InvalidArgument` with the offending value
    ///   embedded for debugging.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "git-upload-pack" => Ok(ServiceType::UploadPack),
            "git-receive-pack" => Ok(ServiceType::ReceivePack),
            _ => Err(GitError::InvalidArgument(format!(
                "Invalid service name: {}",
                s
            ))),
        }
    }
}

/// Flush packet (`0000`). Marks the end of a logical group of pkt-lines.
pub const PKT_LINE_END_MARKER: &[u8; 4] = b"0000";

/// String-boundary marker for typed protocol errors carried by external error types.
pub(crate) const PKT_LINE_PROTOCOL_ERROR_PREFIX: &str = "pkt-line protocol error: ";

/// A declared pkt-line length that cannot represent a supported frame.
///
/// Reasons are fixed and never include bytes received from a remote peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PktFrameError {
    /// A non-flush length does not include the complete four-byte header.
    LengthBelowHeader,
    /// A length cannot be encoded in the four hexadecimal header digits.
    LengthAboveMaximum,
}

impl fmt::Display for PktFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::LengthBelowHeader => "frame length is smaller than the four-byte header",
            Self::LengthAboveMaximum => "frame length exceeds the four-digit header limit",
        };
        write!(f, "{PKT_LINE_PROTOCOL_ERROR_PREFIX}{reason}")
    }
}

impl std::error::Error for PktFrameError {}

/// Validate a declared pkt-line length and return the number of payload bytes.
///
/// The length includes the four-byte header. Zero is a flush marker, and four is
/// an empty data frame; both return zero payload bytes. Callers must retain the
/// declared length when they need to distinguish those two cases.
///
/// This helper neither allocates nor consumes input. It does not check whether a
/// payload is actually present; readers must separately reject truncated frames.
///
/// # Errors
///
/// Returns [`PktFrameError::LengthBelowHeader`] for lengths one through three,
/// or [`PktFrameError::LengthAboveMaximum`] for lengths greater than `0xffff`.
pub fn pkt_frame_payload_len(declared_len: u32) -> Result<usize, PktFrameError> {
    match declared_len {
        0 => Ok(0),
        1..=3 => Err(PktFrameError::LengthBelowHeader),
        4..=0xffff => Ok((declared_len - 4) as usize),
        _ => Err(PktFrameError::LengthAboveMaximum),
    }
}

use bytes::Bytes;

/// Consume a single pkt-line frame from the front of `bytes` and return its
/// `(declared_length, payload)`.
///
/// Functional scope:
/// - Reads the 4-byte ASCII hex header, decodes it as the total frame length, then
///   splits off `length - 4` bytes of payload.
/// - Mutates the input buffer in place: after a successful call, `bytes` advances past
///   the consumed frame.
///
/// Boundary conditions:
/// - Returns `(0, Bytes::new())` when `bytes` is empty so callers can use a
///   zero-length response as a stop condition.
/// - Returns `(0, Bytes::new())` when the decoded length is zero (the flush marker
///   `0000`); the leading 4 header bytes are still consumed.
/// - **Panics** when the 4-byte header is not valid UTF-8 hex. Callers must therefore
///   validate or trust the source — typically only network code that rejects malformed
///   frames upstream invokes this helper.
pub fn read_pkt_line(bytes: &mut Bytes) -> (usize, Bytes) {
    if bytes.is_empty() {
        return (0, Bytes::new());
    }
    let pkt_length_bytes = bytes.copy_to_bytes(4);
    // INVARIANT: the function's doc comment explicitly documents that
    // callers must validate the 4-byte header as UTF-8 hex. Network code
    // upstream rejects malformed frames before they reach this helper.
    let header_str = core::str::from_utf8(&pkt_length_bytes)
        .expect("pkt-line header must be 4 bytes of ASCII hex (caller contract)");
    let pkt_length = usize::from_str_radix(header_str, 16).unwrap_or_else(|_| {
        panic!("pkt-line header {pkt_length_bytes:?} is not valid hex (caller contract)")
    });
    if pkt_length == 0 {
        return (0, Bytes::new());
    }
    // Advance the buffer past the payload — the caller receives the payload slice and
    // any subsequent read continues from the next frame.
    let pkt_line = bytes.copy_to_bytes(pkt_length - 4);
    tracing::debug!("pkt line: {:?}", pkt_line);

    (pkt_length, pkt_line)
}

use bytes::BytesMut;

/// Append a UTF-8 string as a pkt-line to `pkt_line_stream`.
///
/// Functional scope:
/// - Writes the 4-byte ASCII hex length header (`buf_str.len() + 4`, including the
///   header itself) followed by the raw bytes of `buf_str`.
/// - Does **not** add a trailing newline; callers that need newline-terminated lines
///   (the common case for capability advertisements) must include the `\n` in
///   `buf_str`.
///
/// Boundary conditions:
/// - Maximum frame size is `0xffff` bytes (65,535) per the Git protocol spec; this
///   helper does not enforce that limit and will silently produce malformed frames if
///   given an oversized string. Callers must chunk longer payloads themselves.
pub fn add_pkt_line_string(pkt_line_stream: &mut BytesMut, buf_str: String) {
    let buf_str_length = buf_str.len() + 4;
    pkt_line_stream.put(Bytes::from(format!("{:04x}", buf_str_length)));
    pkt_line_stream.put(buf_str.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::{PKT_LINE_PROTOCOL_ERROR_PREFIX, PktFrameError, pkt_frame_payload_len};

    #[test]
    fn pkt_frame_payload_len_ok_flush_zero() {
        assert_eq!(pkt_frame_payload_len(0), Ok(0));
    }

    #[test]
    fn pkt_frame_payload_len_rejects_len_below_four() {
        for declared_len in 1..=3 {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Err(PktFrameError::LengthBelowHeader)
            );
        }
        let message = PktFrameError::LengthBelowHeader.to_string();
        assert_eq!(
            message,
            "pkt-line protocol error: frame length is smaller than the four-byte header"
        );
        assert!(message.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
    }

    #[test]
    fn pkt_frame_payload_len_ok_len_4_empty() {
        assert_eq!(pkt_frame_payload_len(4), Ok(0));
    }

    #[test]
    fn pkt_frame_payload_len_ok_normal_and_upper_bound() {
        for declared_len in 5..=0xffff {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Ok((declared_len - 4) as usize)
            );
        }
        assert_eq!(pkt_frame_payload_len(0xffff), Ok(65_531));
    }

    #[test]
    fn pkt_frame_payload_len_rejects_over_0xffff() {
        for declared_len in [0x10000, 0x10001, 0x100000, u32::MAX - 1, u32::MAX] {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Err(PktFrameError::LengthAboveMaximum)
            );
        }
        let message = PktFrameError::LengthAboveMaximum.to_string();
        assert_eq!(
            message,
            "pkt-line protocol error: frame length exceeds the four-digit header limit"
        );
        assert!(message.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
    }
}
