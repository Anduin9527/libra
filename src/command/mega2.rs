//! `libra mega2 browser` — the single public Mega2 surface (plan-20260912 MB-03).
//!
//! The command is a thin, read-only adapter over two already-verified layers:
//! the bounded transport in [`crate::internal::protocol::mega2_tree`] (MB-01)
//! and the resumable terminal state in [`crate::command::mega2_browser`]
//! (MB-02). It never opens the repository database, never touches the index or
//! object store, and never persists configuration — everything it needs comes
//! from argv and the remote server.
//!
//! Two output paths share the same validated inputs:
//!
//! - human/TUI: exactly the MB-02 interactive loop, which itself requires
//!   stdin and stdout to be TTYs before altering any terminal state;
//! - `--json`/`--machine`: exactly one MB-01 fetch whose validated listing is
//!   rendered through the shared JSON envelope.

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    internal::protocol::mega2_tree::{
        ContentType, Listing, Mega2TreeSession, normalize_path, validate_server_url,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

/// `EXAMPLES:` banner for the `mega2` parent command.
pub const MEGA2_EXAMPLES: &str = "\
EXAMPLES:
    libra mega2 browser --server https://mega2.example.com        Browse the remote root in the TUI
    libra mega2 browser --server https://mega2.example.com src    Open a rooted path directly
    libra --json mega2 browser --server http://127.0.0.1:8080     One bounded fetch as JSON
    libra --machine mega2 browser --server https://mega2.example.com  Strict machine mode

`mega2 browser` is Libra-only: it lists one remote directory level and has no
Git-equivalent contract. `libra ls-tree` inspects local tree objects instead.";

/// `EXAMPLES:` banner for `libra mega2 browser`.
pub const MEGA2_BROWSER_EXAMPLES: &str = "\
EXAMPLES:
    libra mega2 browser --server https://mega2.example.com             Interactive browse of /
    libra mega2 browser --server https://mega2.example.com src/pkg     Interactive browse of /src/pkg
    libra mega2 browser --server https://mega2.example.com --ref v1.2  List one commit or tag
    libra mega2 browser --server http://127.0.0.1:8080 --json          Exactly one fetch, JSON schema
    libra --machine mega2 browser --server https://mega2.example.com   NDJSON for automation

Keys (interactive mode): Up/Down or k/j select, Enter opens a directory,
Backspace or h goes to the parent, r reloads, q quits.";

/// `libra mega2 <subcommand>`; the parent exposes exactly one child.
#[derive(Args, Debug)]
pub struct Mega2Args {
    #[command(subcommand)]
    pub command: Mega2Subcommand,
}

/// The only registered `mega2` subcommand.
#[derive(Subcommand, Debug)]
pub enum Mega2Subcommand {
    #[command(
        about = "Browse one remote directory listing (TUI by default; --json/--machine for one fetch)",
        after_help = MEGA2_BROWSER_EXAMPLES
    )]
    Browser(BrowserArgs),
}

/// Arguments for the single public browser surface.
#[derive(Args, Debug)]
pub struct BrowserArgs {
    /// Mega2 server base URL (https://host, or loopback http://host:port)
    #[arg(long, value_name = "BASE-URL")]
    pub server: String,

    /// Rooted directory path to list; never escapes above `/`
    #[arg(value_name = "PATH", default_value = "/")]
    pub path: String,

    /// Optional commit or tag to list instead of the server default branch
    #[arg(long = "ref", value_name = "COMMIT-OR-TAG")]
    pub git_ref: Option<String>,
}

/// One validated listing entry in the documented machine schema.
#[derive(Serialize, Debug, PartialEq, Eq)]
struct BrowserItem<'a> {
    name: &'a str,
    content_type: &'a str,
}

/// The documented `mega2 browser` machine payload.
#[derive(Serialize, Debug)]
struct BrowserData<'a> {
    server: &'a str,
    #[serde(rename = "ref")]
    git_ref: Option<&'a str>,
    path: &'a str,
    items: Vec<BrowserItem<'a>>,
}

/// Canonical, credential-free rendering of a validated server URL.
///
/// `validate_server_url` already rejects userinfo, query and fragment, so the
/// serialization origin is exactly scheme + host + optional port.
fn canonical_server(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

fn content_type_name(content_type: ContentType) -> &'static str {
    match content_type {
        ContentType::Directory => "directory",
        ContentType::File => "file",
    }
}

fn browser_data<'a>(
    server: &'a str,
    git_ref: Option<&'a str>,
    path: &'a str,
    listing: &'a Listing,
) -> BrowserData<'a> {
    BrowserData {
        server,
        git_ref,
        path,
        items: listing
            .entries
            .iter()
            .map(|entry| BrowserItem {
                name: &entry.name,
                content_type: content_type_name(entry.content_type),
            })
            .collect(),
    }
}

/// # Side Effects
///
/// Reads one bounded remote listing (JSON/machine mode) or drives the MB-02
/// TUI (human mode). It never opens the repository database, object store,
/// index or configuration; no network request carries credentials.
///
/// # Errors
///
/// Returns structured CLI errors for invalid invocation (bad URL/path), a
/// missing terminal in human mode, unavailable network, refused/redirected
/// HTTP responses, and malformed or hostile server responses.
pub async fn execute_safe(args: Mega2Args, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        Mega2Subcommand::Browser(browser) => execute_browser(browser, output).await,
    }
}

async fn execute_browser(args: BrowserArgs, output: &OutputConfig) -> CliResult<()> {
    // Validation happens before any terminal change or network request.
    let url = validate_server_url(&args.server)?;
    let path = normalize_path(&args.path)?;
    let server = canonical_server(&url);
    let git_ref = args.git_ref.as_deref();

    if output.is_json() {
        // One fetch, one documented payload; no TTY required.
        let mut session = Mega2TreeSession::new(&server)?;
        let listing = session.fetch(&path, git_ref).await?;
        let data = browser_data(&server, git_ref, &path, &listing);
        return emit_json_data("mega2 browser", &data, output);
    }

    if output.quiet {
        return Err(CliError::fatal(
            "mega2 browser: --quiet needs a machine output mode; use --json or --machine",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("run `libra --machine mega2 browser --server <base-url>` for NDJSON output"));
    }

    crate::command::mega2_browser::run(&server, &path, git_ref).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_server_drops_trailing_slash_and_keeps_port() {
        let url = validate_server_url("https://mega2.example.com/").expect("valid url");
        assert_eq!(canonical_server(&url), "https://mega2.example.com");
        let loopback = validate_server_url("http://127.0.0.1:8080").expect("valid loopback");
        assert_eq!(canonical_server(&loopback), "http://127.0.0.1:8080");
    }

    #[test]
    fn machine_payload_uses_the_documented_schema() {
        let listing = Listing {
            entries: vec![
                crate::internal::protocol::mega2_tree::ListingEntry {
                    name: "dir".to_string(),
                    content_type: ContentType::Directory,
                },
                crate::internal::protocol::mega2_tree::ListingEntry {
                    name: "file.txt".to_string(),
                    content_type: ContentType::File,
                },
            ],
        };
        let data = browser_data("https://mega2.example.com", Some("v1"), "/src", &listing);
        let json = serde_json::to_value(&data).expect("serialize");
        assert_eq!(json["server"], "https://mega2.example.com");
        assert_eq!(json["ref"], "v1");
        assert_eq!(json["path"], "/src");
        assert_eq!(json["items"][0]["name"], "dir");
        assert_eq!(json["items"][0]["content_type"], "directory");
        assert_eq!(json["items"][1]["content_type"], "file");
    }

    #[test]
    fn browser_args_parse_ref_and_default_path() {
        use clap::Parser;

        #[derive(Parser, Debug)]
        struct Probe {
            #[command(flatten)]
            args: BrowserArgs,
        }

        let parsed = Probe::try_parse_from([
            "browser",
            "--server",
            "https://mega2.example.com",
            "--ref",
            "v1",
        ])
        .expect("parse");
        assert_eq!(parsed.args.path, "/");
        assert_eq!(parsed.args.git_ref.as_deref(), Some("v1"));
    }
}
