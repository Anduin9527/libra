//! SSH protocol client that spawns an `ssh` subprocess for Git transport.
//!
//! Supports both `ssh://[user@]host[:port]/path` and `user@host:path` URL formats.
//! Uses the vault-generated SSH private key for authentication when available.

use std::{
    io::{Error as IoError, ErrorKind, IsTerminal},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::stream::StreamExt;
use git_internal::errors::GitError;
use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_stream::wrappers::ReceiverStream;

use super::{
    DiscoveryResult, FetchStream, generate_upload_pack_content, parse_discovered_references,
};
use crate::{
    command::fetch::is_pkt_line_io_error,
    git_protocol::{PktLineError, ServiceType, pkt_frame_payload_len, pkt_line_read_error},
};

const DEFAULT_SSH_PORT: u16 = 22;

/// Default idle timeout for SSH I/O operations. Read/write loops reset this
/// timeout after each successful I/O operation; process-wait phases use it as
/// the maximum silent processing window.
const DEFAULT_SSH_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const SSH_SEND_PACK_CHUNK_SIZE: usize = 64 * 1024;
/// Maximum wait for the direct SSH child after an advertisement read fails.
const SSH_READ_ERROR_REAP_TIMEOUT: Duration = Duration::from_secs(2);
/// Give an SSH process which closed stdout a short chance to report its exit
/// status. This window is included in the total read-error cleanup budget.
const SSH_HEADER_EOF_STATUS_TIMEOUT: Duration = Duration::from_millis(100);

fn default_ssh_idle_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(raw) = std::env::var("LIBRA_TEST_SSH_IDLE_TIMEOUT_MS")
        && let Ok(ms) = raw.parse::<u64>()
        && ms > 0
    {
        return Duration::from_millis(ms);
    }

    DEFAULT_SSH_IDLE_TIMEOUT
}

pub struct SshClient {
    user: String,
    host: String,
    port: u16,
    repo_path: String,
    key_path: Option<String>,
    temp_key_file: Option<NamedTempFile>,
    strict_host_key_checking: String,
    idle_timeout: Duration,
}

impl SshClient {
    /// Parse an SSH URL in either `ssh://[user@]host[:port]/path` or `user@host:path` format.
    pub fn from_ssh_spec(spec: &str) -> Result<Self, String> {
        if spec.starts_with("ssh://") {
            Self::from_ssh_url(spec)
        } else {
            Self::from_scp_style(spec)
        }
    }

    /// Set the path to the SSH private key for authentication.
    pub fn with_key_path(mut self, key_path: String) -> Self {
        self.key_path = Some(key_path);
        self
    }

    /// Hold a temporary SSH private key file for the lifetime of the client.
    pub fn with_temp_key_file(mut self, temp_key_file: NamedTempFile) -> Self {
        self.temp_key_file = Some(temp_key_file);
        self.key_path = None;
        self
    }

    /// Override the SSH per-operation idle timeout for callers that need a
    /// longer-lived transport, such as push send-pack.
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Configure StrictHostKeyChecking mode.
    ///
    /// Supported values: `ask` (default), `yes`, `accept-new`, `no` — the same
    /// four policies OpenSSH/Git expose. In `ask` mode the option is not passed
    /// to `ssh` at all, so the user's `~/.ssh/config` governs, matching Git.
    pub fn with_strict_host_key_checking(mut self, mode: String) -> Result<Self, String> {
        let normalized = normalize_host_key_checking_mode(&mode).ok_or_else(|| {
            format!(
                "invalid ssh.strictHostKeyChecking value '{mode}', \
                 expected 'ask', 'yes', 'accept-new', or 'no'"
            )
        })?;
        self.strict_host_key_checking = normalized.to_string();
        Ok(self)
    }

    fn from_ssh_url(spec: &str) -> Result<Self, String> {
        let url = url::Url::parse(spec).map_err(|e| format!("invalid SSH URL: {e}"))?;
        let user = if url.username().is_empty() {
            "git".to_string()
        } else {
            url.username().to_string()
        };
        let host = url.host_str().ok_or("missing host in SSH URL")?.to_string();
        let port = url.port().unwrap_or(DEFAULT_SSH_PORT);
        let mut repo_path = url.path().to_string();
        if repo_path.starts_with('/') {
            repo_path = repo_path[1..].to_string();
        }
        if repo_path.ends_with('/') && repo_path.len() > 1 {
            repo_path.pop();
        }
        Ok(Self {
            user,
            host,
            port,
            repo_path,
            key_path: None,
            temp_key_file: None,
            strict_host_key_checking: "ask".to_string(),
            idle_timeout: default_ssh_idle_timeout(),
        })
    }

    /// Parse SCP-style `user@host:path` format.
    fn from_scp_style(spec: &str) -> Result<Self, String> {
        let (user_host, path) = spec
            .split_once(':')
            .ok_or_else(|| format!("invalid SCP-style SSH spec: {spec}"))?;
        let (user, host) = if let Some((u, h)) = user_host.split_once('@') {
            (u.to_string(), h.to_string())
        } else {
            ("git".to_string(), user_host.to_string())
        };
        let repo_path = path.trim_end_matches('/').to_string();
        Ok(Self {
            user,
            host,
            port: DEFAULT_SSH_PORT,
            repo_path,
            key_path: None,
            temp_key_file: None,
            strict_host_key_checking: "ask".to_string(),
            idle_timeout: default_ssh_idle_timeout(),
        })
    }

    /// Spawn an SSH subprocess running the given Git service on the remote.
    ///
    /// Host key checking mirrors Git's transport: in the default `ask` mode no
    /// `StrictHostKeyChecking` option is passed, so the user's `~/.ssh/config`
    /// governs and OpenSSH offers its interactive trust prompt (TOFU) on the
    /// terminal. Explicit modes are forwarded verbatim.
    ///
    /// Interactivity is decided by libra's own stdin: in headless contexts
    /// (CI, agents, tests) prompts can never be answered, so `BatchMode=yes`
    /// makes ssh fail fast instead of hanging, and stderr stays piped so
    /// diagnostics land in the error message. Interactive sessions inherit
    /// stderr so the user sees ssh's host-key warning, fingerprint, and
    /// "Permanently added" confirmation live — exactly like `git clone`.
    async fn spawn_service(&self, service: ServiceType) -> Result<tokio::process::Child, IoError> {
        let service_cmd = match service {
            ServiceType::UploadPack => "git-upload-pack",
            ServiceType::ReceivePack => "git-receive-pack",
        };
        // Build: ssh [opts] user@host "git-upload-pack '/repo/path'"
        let ssh_bin = std::env::var("LIBRA_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
        let interactive = std::io::stdin().is_terminal();
        let mut cmd = tokio::process::Command::new(ssh_bin);
        // In `ask` mode (default) defer to the user's ssh_config, like Git.
        if self.strict_host_key_checking != "ask" {
            cmd.arg("-o").arg(format!(
                "StrictHostKeyChecking={}",
                self.strict_host_key_checking
            ));
        }
        if !interactive {
            cmd.arg("-o").arg("BatchMode=yes");
        }
        if let Some(ref key_file) = self.temp_key_file {
            cmd.arg("-i").arg(key_file.path());
        } else if let Some(ref key) = self.key_path {
            cmd.arg("-i").arg(key);
        }
        if self.port != DEFAULT_SSH_PORT {
            cmd.arg("-p").arg(self.port.to_string());
        }
        cmd.arg(format!("{}@{}", self.user, self.host));
        cmd.arg(format!(
            "{service_cmd} {}",
            shell_single_quote(&self.repo_path)
        ));
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if interactive {
            // Let ssh talk to the user's terminal directly (host-key prompt
            // context, banners, remote diagnostics), as Git does.
            cmd.stderr(std::process::Stdio::inherit());
        } else {
            cmd.stderr(std::process::Stdio::piped());
        }
        // The local `ssh` process can outlive the remote service (GitHub in
        // particular keeps the channel open briefly, and ControlMaster
        // setups can keep the client process alive even longer). Killing
        // on drop ensures `fetch_objects`'s background task cannot leave
        // an orphaned subprocess blocking shutdown.
        cmd.kill_on_drop(true).spawn()
    }

    /// Read pkt-line advertisement from the SSH child's stdout.
    ///
    /// Each individual `read_exact` call is wrapped with the configured idle
    /// timeout so a stalled remote triggers a timely error instead of blocking
    /// forever.
    async fn read_advertisement<R: AsyncRead + Unpin>(
        &self,
        stdout: &mut R,
    ) -> Result<Bytes, IoError> {
        let mut buf = BytesMut::new();
        loop {
            let mut len_buf = [0u8; 4];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, stdout.read_exact(&mut len_buf))
                .await
                .map_err(|_| {
                    IoError::other(format!(
                        "SSH read timed out after {}s (idle)",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|error| {
                    wrap_ssh_read_error(
                        pkt_line_read_error(error, PktLineError::TruncatedHeader),
                        "SSH read failed",
                        None,
                    )
                })?;
            let len_str = std::str::from_utf8(&len_buf)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            let len = usize::from_str_radix(len_str, 16)
                .map_err(|e| IoError::other(format!("invalid pkt-line length: {e}")))?;
            buf.extend_from_slice(&len_buf);
            if len == 0 {
                break;
            }
            let payload_len = pkt_frame_payload_len(len as u32)
                .map_err(|error| IoError::new(ErrorKind::InvalidData, PktLineError::from(error)))?;
            let mut data = vec![0u8; payload_len];
            let timeout = self.idle_timeout;
            tokio::time::timeout(timeout, stdout.read_exact(&mut data))
                .await
                .map_err(|_| {
                    IoError::other(format!(
                        "SSH read timed out after {}s (idle)",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|error| {
                    wrap_ssh_read_error(
                        pkt_line_read_error(error, PktLineError::TruncatedPayload),
                        "SSH read failed",
                        None,
                    )
                })?;
            buf.extend_from_slice(&data);
        }
        Ok(buf.freeze())
    }

    pub async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        let mut child = self
            .spawn_service(service)
            .await
            .map_err(|e| GitError::NetworkError(format!("SSH spawn failed: {e}")))?;
        let response = {
            let stdout = child.stdout.as_mut().ok_or_else(|| {
                GitError::NetworkError("SSH child stdout not captured".to_string())
            })?;
            self.read_advertisement(stdout).await
        };
        let response = match response {
            Ok(response) => response,
            Err(read_err) => {
                let error = finish_ssh_read_error(child, read_err, "SSH read failed").await;
                return Err(GitError::NetworkError(error.to_string()));
            }
        };
        // Discovery only needs the advertisement packet. Kill and reap the child
        // to avoid leaving an unreaped process around.
        let _ = child.kill().await;
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| GitError::NetworkError(format!("SSH wait failed: {e}")))?;
        // If the process was not killed by signal and exited non-zero, surface diagnostics.
        if !output.status.success() && output.status.code().is_some() {
            return Err(GitError::NetworkError(format!(
                "SSH discovery command failed: {}",
                describe_process_output(&output)
            )));
        }
        parse_discovered_references(response, service)
    }

    pub async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        let mut child = self.spawn_service(ServiceType::UploadPack).await?;
        let advertisement = {
            let stdout = child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout).await
        };
        if let Err(read_err) = advertisement {
            return Err(
                finish_ssh_read_error(child, read_err, "SSH advertisement read failed").await,
            );
        }

        // Send the upload-pack request
        let body = generate_upload_pack_content(have, want, shallow, depth);
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| IoError::other("SSH child stdin not captured"))?;
        stdin.write_all(&body).await?;
        stdin.shutdown().await?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
        // stderr may be uncaptured when it is inherited in interactive
        // sessions; treat that the same as an empty stream.
        let stderr = child.stderr.take();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, IoError>>(32);
        let idle_timeout = self.idle_timeout;

        tokio::spawn(async move {
            let stderr_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_end(&mut buf).await;
                }
                buf
            });

            let mut buf = [0u8; 16 * 1024];
            let mut forward_err: Option<IoError> = None;
            let mut sent_any_stdout = false;
            loop {
                match tokio::time::timeout(idle_timeout, stdout.read(&mut buf)).await {
                    Err(_) => {
                        let _ = child.start_kill();
                        if !sent_any_stdout {
                            forward_err = Some(IoError::new(
                                std::io::ErrorKind::TimedOut,
                                format!(
                                    "SSH upload-pack stdout timed out after {}s (idle)",
                                    idle_timeout.as_secs()
                                ),
                            ));
                        }
                        break;
                    }
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => {
                        sent_any_stdout = true;
                        if tx
                            .send(Ok(Bytes::copy_from_slice(&buf[..n])))
                            .await
                            .is_err()
                        {
                            // Consumer dropped the stream; rely on
                            // `kill_on_drop` (set in `spawn_service`) to take
                            // down the ssh subprocess when `child` is dropped.
                            stderr_task.abort();
                            return;
                        }
                    }
                    Ok(Err(err)) => {
                        forward_err = Some(IoError::other(format!(
                            "failed to read SSH upload-pack stdout: {err}"
                        )));
                        break;
                    }
                }
            }

            // After stdout EOF the upload-pack service is finished. The local
            // `ssh` process can still take a while to exit (control sockets,
            // late exit-status, server-side keepalive), so don't block the
            // consumer's stream on a clean exit — wait briefly, then kill.
            let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => Some(status),
                Ok(Err(_)) => None,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                    None
                }
            };

            // Stderr stays open until the ssh process actually exits; if we
            // had to kill it above the read may already be unblocked, but cap
            // the join anyway so a stuck pipe can't keep the channel alive.
            let stderr_buf = match tokio::time::timeout(Duration::from_secs(1), stderr_task).await {
                Ok(Ok(buf)) => buf,
                _ => Vec::new(),
            };

            if let Some(err) = forward_err {
                let _ = tx.send(Err(err)).await;
            } else if let Some(status) = status
                && !status.success()
            {
                let _ = tx
                    .send(Err(IoError::other(format!(
                        "SSH upload-pack failed: {}",
                        describe_status_with_stderr(&status, &stderr_buf)
                    ))))
                    .await;
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    pub async fn send_pack(&self, data: Bytes) -> Result<Bytes, IoError> {
        let mut child = self.spawn_service(ServiceType::ReceivePack).await?;
        let advertisement = {
            let stdout = child
                .stdout
                .as_mut()
                .ok_or_else(|| IoError::other("SSH child stdout not captured"))?;
            self.read_advertisement(stdout).await
        };
        if let Err(read_err) = advertisement {
            return Err(
                finish_ssh_read_error(child, read_err, "SSH advertisement read failed").await,
            );
        }

        // Send the pack data with the timeout resetting after each successful
        // write. A single write_all over the whole pack would incorrectly turn
        // this into a total-transfer timeout for large pushes.
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| IoError::other("SSH child stdin not captured"))?;
        Self::write_all_with_idle_timeout(stdin, data.as_ref(), self.idle_timeout).await?;
        let timeout = self.idle_timeout;
        tokio::time::timeout(timeout, stdin.shutdown())
            .await
            .map_err(|_| {
                IoError::other(format!(
                    "SSH shutdown timed out after {}s (idle)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| IoError::other(format!("SSH shutdown failed: {e}")))?;

        // Wait for remote to process the pack (with idle timeout)
        let timeout = self.idle_timeout;
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                IoError::other(format!(
                    "SSH receive-pack timed out after {}s (idle)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| IoError::other(format!("SSH wait failed: {e}")))?;
        if !output.status.success() {
            return Err(IoError::other(format!(
                "SSH receive-pack failed: {}",
                describe_process_output(&output)
            )));
        }
        Ok(Bytes::from(output.stdout))
    }

    async fn write_all_with_idle_timeout<W>(
        writer: &mut W,
        data: &[u8],
        idle_timeout: Duration,
    ) -> Result<(), IoError>
    where
        W: AsyncWrite + Unpin,
    {
        let mut written = 0;
        while written < data.len() {
            let chunk_end = (written + SSH_SEND_PACK_CHUNK_SIZE).min(data.len());
            let n = tokio::time::timeout(idle_timeout, writer.write(&data[written..chunk_end]))
                .await
                .map_err(|_| {
                    IoError::new(
                        ErrorKind::TimedOut,
                        format!(
                            "SSH write timed out after {}s (idle)",
                            idle_timeout.as_secs()
                        ),
                    )
                })?
                .map_err(|e| IoError::other(format!("SSH write failed: {e}")))?;
            if n == 0 {
                return Err(IoError::new(
                    ErrorKind::WriteZero,
                    "SSH write failed: wrote zero bytes",
                ));
            }
            written += n;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SshProtocolReadExit {
    source: IoError,
    code: i32,
}

impl std::fmt::Display for SshProtocolReadExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; SSH exited with status {}; check SSH connectivity, trusted host keys, ssh-agent authentication, and remote repository access",
            self.source, self.code
        )
    }
}

impl std::error::Error for SshProtocolReadExit {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Preserve the protocol carrier before adding any SSH or cleanup diagnostics.
/// All inner reads and outer advertisement failures share this formatter.
fn wrap_ssh_read_error(
    read_error: IoError,
    context: &'static str,
    output: Option<Result<std::process::Output, IoError>>,
) -> IoError {
    if is_pkt_line_io_error(&read_error) {
        if let Some(Ok(output)) = &output
            && let Some(code) = output.status.code()
            && code != 0
        {
            return IoError::new(
                read_error.kind(),
                SshProtocolReadExit {
                    source: read_error,
                    code,
                },
            );
        }
        return read_error;
    }
    match output {
        None => IoError::other(format!("{context}: {read_error}")),
        Some(Ok(output)) => IoError::other(format!(
            "{context}: {read_error}; {}",
            describe_process_output(&output)
        )),
        Some(Err(error)) => IoError::other(format!(
            "{context}: {read_error}; unable to collect process output: {error}"
        )),
    }
}

/// Bound the entire direct-child cleanup, including a short header-EOF window
/// for SSH's own exit status. Other read failures request termination immediately.
async fn finish_ssh_read_error(
    mut child: tokio::process::Child,
    read_error: IoError,
    context: &'static str,
) -> IoError {
    let deadline = tokio::time::Instant::now() + SSH_READ_ERROR_REAP_TIMEOUT;
    let header_eof = read_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<PktLineError>())
        == Some(&PktLineError::TruncatedHeader);
    let mut cleanup_error = None;
    let exited = if header_eof {
        let status_deadline =
            (tokio::time::Instant::now() + SSH_HEADER_EOF_STATUS_TIMEOUT).min(deadline);
        match tokio::time::timeout_at(status_deadline, child.wait()).await {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                cleanup_error = Some(IoError::other(format!(
                    "unable to read SSH exit status: {error}"
                )));
                false
            }
            Err(_) => false,
        }
    } else {
        false
    };
    if !exited && let Err(error) = child.start_kill() {
        let detail = format!("unable to stop SSH child: {error}");
        cleanup_error = Some(match cleanup_error {
            Some(previous) => IoError::other(format!("{previous}; {detail}")),
            None => IoError::other(detail),
        });
    }
    if is_pkt_line_io_error(&read_error) {
        // Only a local exit status may supplement a protocol error. Drop captured
        // byte streams so descendants holding them open cannot delay the reap.
        drop(child.stdout.take());
        drop(child.stderr.take());
    }
    let output = match tokio::time::timeout_at(deadline, child.wait_with_output()).await {
        Ok(output) => output,
        Err(_) => Err(IoError::new(
            ErrorKind::TimedOut,
            "SSH child cleanup exceeded the two-second limit",
        )),
    };
    // The primary protocol error survives even a cleanup failure. The child is
    // configured with kill_on_drop as a fallback; no remote output is rendered.
    finish_ssh_read_result(read_error, context, output, cleanup_error)
}

fn finish_ssh_read_result(
    read_error: IoError,
    context: &'static str,
    output: Result<std::process::Output, IoError>,
    cleanup_error: Option<IoError>,
) -> IoError {
    let error = wrap_ssh_read_error(read_error, context, Some(output));
    if let Some(cleanup_error) = cleanup_error
        && !is_pkt_line_io_error(&error)
    {
        // Keep the actual collected status/output even when a kill request
        // failed; the additional local warning must not replace that evidence.
        return IoError::other(format!("{error}; SSH cleanup warning: {cleanup_error}"));
    }
    error
}

fn describe_process_output(output: &std::process::Output) -> String {
    describe_status_with_stderr(&output.status, &output.stderr)
}

fn describe_status_with_stderr(status: &std::process::ExitStatus, stderr: &[u8]) -> String {
    let status = status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        format!("exit status {status}")
    } else {
        format!("exit status {status}, stderr: {stderr}")
    }
}

fn normalize_host_key_checking_mode(mode: &str) -> Option<&'static str> {
    ["ask", "yes", "accept-new", "no"]
        .into_iter()
        .find(|known| mode.eq_ignore_ascii_case(known))
}

fn shell_single_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

/// Check if a remote spec looks like an SSH URL.
pub fn is_ssh_spec(spec: &str) -> bool {
    if spec.starts_with("ssh://") {
        return true;
    }

    // SCP-style: [user@]host:path
    if spec.contains("://")
        || spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
    {
        return false;
    }

    let Some((user_host, path)) = spec.split_once(':') else {
        return false;
    };
    if user_host.is_empty() || path.is_empty() {
        return false;
    }

    // Avoid mistaking Windows local paths (e.g. C:\repo) for SSH remotes.
    if user_host.len() == 1
        && user_host
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        return false;
    }

    if user_host.contains('/') || user_host.contains('\\') {
        return false;
    }

    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const PKT12_SENTINEL: &str = "PKT12_REMOTE_SECRET_0ec451";

    fn pkt12_output() -> std::process::Output {
        pkt12_output_with_code(23)
    }

    fn pkt12_output_with_code(code: i32) -> std::process::Output {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code as u32)
        };
        std::process::Output {
            status,
            stdout: PKT12_SENTINEL.as_bytes().to_vec(),
            stderr: PKT12_SENTINEL.as_bytes().to_vec(),
        }
    }

    fn pkt12_typed_error() -> IoError {
        IoError::new(ErrorKind::InvalidData, PktLineError::TruncatedHeader)
    }

    fn pkt12_assert_wrapper(context: &'static str) {
        for output in [
            Ok(pkt12_output_with_code(0)),
            Ok(pkt12_output()),
            Err(IoError::other(format!("collect failed: {PKT12_SENTINEL}"))),
        ] {
            let exit_code = output.as_ref().ok().and_then(|output| output.status.code());
            let error = wrap_ssh_read_error(pkt12_typed_error(), context, Some(output));
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            let original = if exit_code == Some(23) {
                let cause = error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<SshProtocolReadExit>()
                    .unwrap();
                assert_eq!(cause.code, 23);
                assert!(
                    error
                        .to_string()
                        .starts_with(&PktLineError::TruncatedHeader.to_string())
                );
                assert!(error.to_string().contains("SSH exited with status 23"));
                assert!(error.to_string().contains("ssh-agent authentication"));
                &cause.source
            } else {
                assert_eq!(error.to_string(), PktLineError::TruncatedHeader.to_string());
                &error
            };
            assert_eq!(
                original
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<PktLineError>()),
                Some(&PktLineError::TruncatedHeader)
            );
            assert!(!error.to_string().contains(PKT12_SENTINEL));
        }
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_marker_passthrough_discovery() {
        pkt12_assert_wrapper("SSH read failed");
        for output in [Ok(pkt12_output()), Err(IoError::other(PKT12_SENTINEL))] {
            let error = wrap_ssh_read_error(pkt12_typed_error(), "SSH read failed", Some(output));
            let expected = error.to_string();
            let carrier = GitError::NetworkError(expected.clone());
            assert!(matches!(carrier, GitError::NetworkError(ref detail)
                if detail == &expected));
        }
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_passthrough_fetch_objects() {
        pkt12_assert_wrapper("SSH advertisement read failed");
    }

    #[test]
    fn pkt_line_client_ssh_wrapper_passthrough_send_pack() {
        pkt12_assert_wrapper("SSH advertisement read failed");
    }

    #[tokio::test]
    async fn pkt_line_client_non_marker_wrapped_regression() {
        let ordinary = || IoError::new(ErrorKind::ConnectionReset, "connection reset fixture");
        assert_eq!(
            wrap_ssh_read_error(ordinary(), "SSH read failed", None).to_string(),
            "SSH read failed: connection reset fixture"
        );
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            assert_eq!(
                wrap_ssh_read_error(ordinary(), context, Some(Ok(pkt12_output()))).to_string(),
                format!(
                    "{context}: connection reset fixture; exit status 23, stderr: {PKT12_SENTINEL}"
                )
            );
            assert_eq!(
                wrap_ssh_read_error(
                    ordinary(),
                    context,
                    Some(Err(IoError::other("fixture wait failure")))
                )
                .to_string(),
                format!(
                    "{context}: connection reset fixture; unable to collect process output: fixture wait failure"
                )
            );
            let collected = finish_ssh_read_result(
                ordinary(),
                context,
                Ok(pkt12_output()),
                Some(IoError::other("fixture kill denied")),
            );
            assert_eq!(
                collected.to_string(),
                format!(
                    "{context}: connection reset fixture; exit status 23, stderr: {PKT12_SENTINEL}; SSH cleanup warning: fixture kill denied"
                )
            );
            let failed = finish_ssh_read_result(
                ordinary(),
                context,
                Err(IoError::other("fixture wait failure")),
                Some(IoError::other("fixture kill denied")),
            );
            assert!(failed.to_string().contains("fixture wait failure"));
            assert!(failed.to_string().contains("fixture kill denied"));
        }

        #[cfg(unix)]
        {
            let mut child = pkt12_fault_child(b"0001").await;
            let pid = child.id().unwrap();
            // Reading the fixture's frame confirms its stderr was written and
            // the still-running process is ready before injecting ordinary IO.
            let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
            tokio::time::timeout(
                Duration::from_secs(5),
                client.read_advertisement(child.stdout.as_mut().unwrap()),
            )
            .await
            .expect("ordinary-error fixture becomes ready")
            .unwrap_err();
            assert!(child.try_wait().unwrap().is_none());
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                finish_ssh_read_error(child, ordinary(), "SSH read failed"),
            )
            .await
            .expect("ordinary read-error cleanup is bounded");
            assert!(
                error
                    .to_string()
                    .starts_with("SSH read failed: connection reset fixture; ")
            );
            assert!(error.to_string().contains("terminated by signal"));
            assert!(error.to_string().contains(PKT12_SENTINEL));
            assert!(!is_pkt_line_io_error(&error));
            pkt12_assert_reaped(pid);
        }
    }

    #[test]
    fn pkt_line_client_zero_echo_sentinel() {
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            pkt12_assert_wrapper(context);
        }
        // Actual malicious child stderr is additionally exercised by the existing
        // end-to-end named gate below; helper inputs are not that proof.
    }

    #[test]
    fn pkt_line_client_marker_at_string_start() {
        use crate::git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX;
        for context in ["SSH read failed", "SSH advertisement read failed"] {
            let error = wrap_ssh_read_error(pkt12_typed_error(), context, Some(Ok(pkt12_output())));
            assert!(
                error
                    .to_string()
                    .starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX)
            );
            let lookalike = IoError::other(format!(
                "context: {PKT_LINE_PROTOCOL_ERROR_PREFIX}lookalike"
            ));
            let error = wrap_ssh_read_error(lookalike, context, None);
            assert!(error.to_string().starts_with(context));
        }
    }

    struct Pkt12InjectedReader {
        prefix: &'static [u8],
    }

    impl AsyncRead for Pkt12InjectedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.prefix.is_empty() {
                return std::task::Poll::Ready(Err(pkt12_typed_error()));
            }
            let count = self.prefix.len().min(buf.remaining());
            buf.put_slice(&self.prefix[..count]);
            self.prefix = &self.prefix[count..];
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_inner_wrapper_marker_passthrough() {
        for prefix in [b"".as_slice(), b"0005"] {
            let error =
                read_test_stream(&mut Pkt12InjectedReader { prefix }, Duration::from_secs(1))
                    .await
                    .unwrap_err();
            assert_eq!(error.to_string(), PktLineError::TruncatedHeader.to_string());
            assert_eq!(error.kind(), ErrorKind::InvalidData);
        }
    }

    #[test]
    fn pkt_line_client_shared_helper_single_source() {
        let source = include_str!("ssh_client.rs");
        let production = source.split("pub(crate) mod tests {").next().unwrap();
        assert_eq!(production.matches("fn wrap_ssh_read_error(").count(), 1);
        assert_eq!(production.matches("fn finish_ssh_read_error(").count(), 1);
        assert_eq!(production.matches("wrap_ssh_read_error(").count(), 4);
        assert_eq!(
            production
                .matches("finish_ssh_read_error(child, read_err,")
                .count(),
            3
        );
    }

    #[cfg(unix)]
    async fn pkt12_fault_child(wire: &[u8]) -> tokio::process::Child {
        // Octal format escapes encode only fixed fixture bytes. No remote text is
        // interpreted as shell syntax, and exec leaves a single direct child PID.
        let encoded = wire
            .iter()
            .map(|b| format!("\\{b:03o}"))
            .collect::<String>();
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "printf '%s' '{PKT12_SENTINEL}' >&2; printf '{encoded}'; exec 1>&-; exec sleep 30"
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    fn pkt12_assert_reaped(pid: u32) {
        let mut status = 0;
        // SAFETY: waitpid receives a writable status pointer and the exact PID of
        // this test's child; WNOHANG cannot wait on an unrelated process.
        let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1, "direct SSH child should already be reaped");
        assert_eq!(IoError::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pkt_line_client_ssh_read_error_reaps_child() {
        let mut child = pkt12_fault_child(b"0008abc").await;
        let pid = child.id().unwrap();
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.read_advertisement(child.stdout.as_mut().unwrap()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            PktLineError::TruncatedPayload.to_string()
        );
        assert!(child.try_wait().unwrap().is_none());
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_ssh_read_error(child, error, "SSH read failed"),
        )
        .await
        .expect("direct child cleanup bounded");
        assert_eq!(
            error.to_string(),
            PktLineError::TruncatedPayload.to_string()
        );
        assert!(!error.to_string().contains(PKT12_SENTINEL));
        pkt12_assert_reaped(pid);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pkt_line_client_ssh_read_error_no_hang() {
        let mut child = pkt12_fault_child(b"0001").await;
        let pid = child.id().unwrap();
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.read_advertisement(child.stdout.as_mut().unwrap()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            child.try_wait().unwrap().is_none(),
            "malformed peer still running"
        );
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_ssh_read_error(child, error, "SSH advertisement read failed"),
        )
        .await
        .expect("must not wait for the 30-second sleeper");
        assert!(is_pkt_line_io_error(&error));
        assert!(!error.to_string().contains(PKT12_SENTINEL));
        pkt12_assert_reaped(pid);
    }

    #[cfg(unix)]
    struct Pkt12SshFixture {
        script: std::path::PathBuf,
        transcript: std::path::PathBuf,
        expected_calls: usize,
    }

    #[cfg(unix)]
    impl Pkt12SshFixture {
        fn write(
            root: &std::path::Path,
            command: &str,
            malformed: &[u8],
            native_exit: bool,
        ) -> Self {
            use std::os::unix::fs::PermissionsExt;

            use crate::git_protocol::add_pkt_line_string;

            let successful_advertisements = if native_exit {
                0
            } else {
                match command {
                    "ls-remote" => 0,
                    "clone" => 2,
                    _ => 1,
                }
            };
            let mut valid = BytesMut::new();
            let oid = "1111111111111111111111111111111111111111";
            if command == "push" {
                add_pkt_line_string(
                    &mut valid,
                    format!(
                        "{oid} refs/heads/main\0report-status delete-refs object-format=sha1\n"
                    ),
                );
            } else {
                add_pkt_line_string(
                    &mut valid,
                    format!(
                        "{oid} HEAD\0multi_ack_detailed side-band-64k ofs-delta symref=HEAD:refs/heads/main object-format=sha1\n"
                    ),
                );
                add_pkt_line_string(&mut valid, format!("{oid} refs/heads/main\n"));
            }
            valid.extend_from_slice(b"0000");
            let encode = |wire: &[u8]| {
                wire.iter()
                    .map(|b| format!("\\{b:03o}"))
                    .collect::<String>()
            };
            let counter = root.join("ssh-count");
            let transcript = root.join("ssh-calls");
            let script = root.join("fake-ssh");
            std::fs::write(&counter, "0\n").unwrap();
            let counter_arg = shell_single_quote(counter.to_str().unwrap());
            let log_arg = shell_single_quote(transcript.to_str().unwrap());
            let stderr = if native_exit {
                format!("Permission denied (publickey). {PKT12_SENTINEL}")
            } else {
                PKT12_SENTINEL.to_string()
            };
            let termination = if native_exit {
                "exit 255"
            } else {
                "exec sleep 30"
            };
            let script_text = format!(
                "#!/bin/sh\nset -eu\nread -r count < {counter_arg}\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > {counter_arg}\nremote_command=''\nfor arg; do remote_command=$arg; done\nprintf '%s %s\\n' \"$$\" \"$remote_command\" >> {log_arg}\nif [ \"$count\" -le {successful_advertisements} ]; then\n  printf '{}'\n  exit 0\nfi\nprintf '%s' '{stderr}' >&2\nprintf '{}'\nexec 1>&-\n{termination}\n",
                encode(&valid),
                encode(malformed),
            );
            std::fs::write(&script, script_text).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                script,
                transcript,
                expected_calls: successful_advertisements + 1,
            }
        }

        fn assert_calls_and_last_reaped(&self, command: &str) {
            let log = std::fs::read_to_string(&self.transcript).unwrap();
            let lines = log.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), self.expected_calls, "{command}: {log}");
            let expected_service = if command == "push" {
                "git-receive-pack"
            } else {
                "git-upload-pack"
            };
            for line in &lines {
                let (_, service) = line.split_once(' ').unwrap();
                assert_eq!(service, format!("{expected_service} 'repo'"));
            }
            let (pid, _) = lines.last().unwrap().split_once(' ').unwrap();
            pkt12_assert_reaped(pid.parse().unwrap());
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial(env, cwd, hash_kind)]
    async fn pkt_line_client_ssh_frame_errors_end_to_end_net_002() {
        use clap::Parser;

        use crate::{
            command::{clone, fetch, ls_remote, pull, push},
            git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX,
            internal::{branch::Branch, config::ConfigKv},
            utils::{
                error::StableErrorCode,
                output::OutputConfig,
                test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
            },
        };

        assert!(
            !std::io::stdin().is_terminal(),
            "captured SSH zero-echo proof requires non-terminal stdin; run under nextest or redirect stdin from /dev/null"
        );
        // The existing local fallback avoids cloud lookups for delete-only push.
        // Keyed lanes and per-test nextest processes follow existing env fixtures.
        let _storage = ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let mut checked = 0;
        for (malformed, native_exit) in [
            b"0001".as_slice(),
            b"0002",
            b"0003",
            b"",
            b"0",
            b"00",
            b"000",
            b"0005",
            b"0008abc",
            b"ffffabc",
        ]
        .into_iter()
        .map(|wire| (wire, false))
        .chain([(b"".as_slice(), true)])
        {
            let expected_reason = if matches!(malformed, b"0001" | b"0002" | b"0003") {
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                )
            } else if malformed.len() < 4 {
                PktLineError::TruncatedHeader
            } else {
                PktLineError::TruncatedPayload
            };
            for command in ["ls-remote", "fetch", "clone", "pull", "push"] {
                let repo = tempfile::tempdir().unwrap();
                setup_with_new_libra_in(repo.path()).await;
                let _cwd = ChangeDirGuard::new(repo.path());
                let fixture = Pkt12SshFixture::write(repo.path(), command, malformed, native_exit);
                let _ssh = ScopedEnvVar::set("LIBRA_SSH_COMMAND", &fixture.script);
                ConfigKv::set("remote.origin.url", "git@fixture.invalid:repo", false)
                    .await
                    .unwrap();
                let tracking = "refs/remotes/origin/main";
                let oid = "1111111111111111111111111111111111111111";
                if command == "push" {
                    Branch::update_branch(tracking, oid, Some("origin"))
                        .await
                        .unwrap();
                }
                let target = repo.path().join("clone-target");
                let output = OutputConfig::default();
                let error = tokio::time::timeout(Duration::from_secs(45), async {
                    match command {
                        "ls-remote" => ls_remote::execute_safe(
                            ls_remote::LsRemoteArgs::try_parse_from(["ls-remote", "origin"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "fetch" => fetch::execute_safe(
                            fetch::FetchArgs::try_parse_from(["fetch", "origin"]).unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "clone" => clone::execute_safe(
                            clone::CloneArgs::try_parse_from([
                                "clone",
                                "git@fixture.invalid:repo",
                                target.to_str().unwrap(),
                            ])
                            .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "pull" => pull::execute_safe(
                            pull::PullArgs::try_parse_from(["pull", "--ff-only", "origin", "main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        "push" => push::execute_safe(
                            push::PushArgs::try_parse_from(["push", "origin", ":refs/heads/main"])
                                .unwrap(),
                            &output,
                        )
                        .await
                        .unwrap_err(),
                        _ => unreachable!(),
                    }
                })
                .await
                .expect("malformed SSH command must terminate");
                assert_eq!(
                    error.stable_code(),
                    StableErrorCode::NetworkProtocol,
                    "{command}: {error:?}"
                );
                assert_eq!(error.stable_code().as_str(), "LBR-NET-002");
                assert_eq!(error.stable_code().exit_code().as_i32(), 128);
                assert!(
                    error.message().contains(PKT_LINE_PROTOCOL_ERROR_PREFIX),
                    "{command}: {error:?}"
                );
                assert!(
                    error.message().contains(&expected_reason.to_string()),
                    "{command}: {error:?}"
                );
                if native_exit {
                    assert!(
                        error.message().contains("SSH exited with status 255"),
                        "{command}: {error:?}"
                    );
                    assert!(
                        error.message().contains("ssh-agent authentication"),
                        "{command}: {error:?}"
                    );
                    assert!(!error.message().contains("Permission denied (publickey)"));
                }
                let hint = if command == "push" {
                    "check the remote Git service or proxy response and retry"
                } else {
                    "check that the remote serves Git data and that a proxy has not altered the response"
                };
                assert_eq!(
                    error.hints().iter().map(|h| h.as_str()).collect::<Vec<_>>(),
                    [hint]
                );
                for rendered in [
                    error.render(),
                    error.render_report(),
                    error.render_json().to_string(),
                ] {
                    assert!(!rendered.contains(PKT12_SENTINEL), "{command}: {rendered}");
                }
                fixture.assert_calls_and_last_reaped(command);
                if command == "push" {
                    assert_eq!(
                        Branch::find_branch_result(tracking, Some("origin"))
                            .await
                            .unwrap()
                            .unwrap()
                            .commit
                            .to_string(),
                        oid
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 55);
    }

    pub(crate) async fn read_test_stream<R: AsyncRead + Unpin>(
        stream: &mut R,
        idle: Duration,
    ) -> Result<Bytes, IoError> {
        let client = SshClient::from_ssh_spec("git@fixture.invalid:repo")
            .unwrap()
            .with_idle_timeout(idle);
        client.read_advertisement(stream).await
    }

    pub(crate) async fn read_frame_fixture(mut input: &[u8]) -> Result<Bytes, IoError> {
        read_test_stream(&mut input, Duration::from_secs(1)).await
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_rejects_len_below_four() {
        for input in [b"0001", b"0002", b"0003"] {
            crate::internal::protocol::git_client::tests::assert_typed_frame_error(
                read_frame_fixture(input).await.unwrap_err(),
                PktLineError::InvalidFrameLength(
                    crate::git_protocol::PktFrameError::LengthBelowHeader,
                ),
            );
        }
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_flush_regression() {
        assert_eq!(
            read_frame_fixture(b"0000zzzz").await.unwrap(),
            b"0000".as_slice()
        );
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_len4_regression() {
        let input = b"00040005x0000";
        assert_eq!(read_frame_fixture(input).await.unwrap(), input.as_slice());
    }

    #[tokio::test]
    async fn pkt_line_client_ssh_upper_bound_regression() {
        for header in [b"ffff", b"FFFF"] {
            let mut input = header.to_vec();
            input.extend_from_slice(&vec![0xff; 0xffff - 4]);
            input.extend_from_slice(b"0000");
            assert_eq!(read_frame_fixture(&input).await.unwrap(), input.as_slice());
        }
    }

    #[test]
    fn test_is_ssh_spec() {
        assert!(is_ssh_spec("git@github.com:user/repo.git"));
        assert!(is_ssh_spec("github.com:user/repo.git"));
        assert!(is_ssh_spec("ssh://git@github.com/user/repo.git"));
        assert!(is_ssh_spec("ssh://github.com/user/repo.git"));
        assert!(!is_ssh_spec("https://github.com/user/repo.git"));
        assert!(!is_ssh_spec("git://github.com/user/repo.git"));
        assert!(!is_ssh_spec("/local/path/to/repo"));
        assert!(!is_ssh_spec("C:\\repo\\path"));
        assert!(!is_ssh_spec("foo/bar:baz"));
    }

    #[test]
    fn test_parse_scp_style() {
        let client = SshClient::from_scp_style("git@github.com:user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
        assert_eq!(client.repo_path, "user/repo.git");
        assert_eq!(client.port, 22);
    }

    #[test]
    fn test_parse_ssh_url() {
        let client = SshClient::from_ssh_url("ssh://git@github.com:2222/user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
        assert_eq!(client.repo_path, "user/repo.git");
        assert_eq!(client.port, 2222);
    }

    #[test]
    fn test_parse_ssh_url_default_user() {
        let client = SshClient::from_ssh_url("ssh://github.com/user/repo.git").unwrap();
        assert_eq!(client.user, "git");
        assert_eq!(client.host, "github.com");
    }

    #[test]
    fn test_shell_single_quote() {
        assert_eq!(shell_single_quote("user/repo.git"), "'user/repo.git'");
        assert_eq!(
            shell_single_quote("user/repo'weird.git"),
            "'user/repo'\"'\"'weird.git'"
        );
    }

    #[test]
    fn test_default_host_key_checking_is_ask() {
        // Git parity: the default defers to the user's ssh_config and lets
        // OpenSSH run its interactive TOFU prompt; no option is injected.
        let client = SshClient::from_scp_style("git@github.com:user/repo.git").unwrap();
        assert_eq!(client.strict_host_key_checking, "ask");
        let client = SshClient::from_ssh_url("ssh://git@github.com/user/repo.git").unwrap();
        assert_eq!(client.strict_host_key_checking, "ask");
    }

    #[test]
    fn test_with_strict_host_key_checking_accepts_git_modes() {
        for mode in ["ask", "yes", "accept-new", "no", "ACCEPT-NEW"] {
            let client = SshClient::from_scp_style("git@github.com:user/repo.git")
                .unwrap()
                .with_strict_host_key_checking(mode.to_string())
                .unwrap();
            assert_eq!(client.strict_host_key_checking, mode.to_lowercase());
        }
    }

    #[test]
    fn test_with_strict_host_key_checking_invalid_value() {
        let result = SshClient::from_scp_style("git@github.com:user/repo.git")
            .unwrap()
            .with_strict_host_key_checking("bogus".to_string());
        assert!(result.is_err(), "invalid mode should be rejected");
        let err = result.err().unwrap();
        assert!(err.contains("expected 'ask', 'yes', 'accept-new', or 'no'"));
    }

    #[tokio::test]
    async fn send_pack_write_helper_writes_large_payload_in_chunks() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let data = vec![42u8; SSH_SEND_PACK_CHUNK_SIZE * 2 + 17];
        let expected_len = data.len();

        let reader_task = tokio::spawn(async move {
            let mut received = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut received)
                .await
                .expect("duplex reader should drain written bytes");
            received.len()
        });

        SshClient::write_all_with_idle_timeout(&mut writer, &data, Duration::from_secs(1))
            .await
            .expect("chunked write should complete while the reader drains data");
        writer
            .shutdown()
            .await
            .expect("duplex writer should shut down cleanly");

        assert_eq!(
            reader_task.await.expect("reader task should finish"),
            expected_len
        );
    }
}
