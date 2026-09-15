# `libra ls-remote`

List references advertised by a remote repository without downloading objects or updating local refs.

```bash
libra ls-remote [OPTIONS] <repository> [patterns...]
```

`<repository>` can be a configured remote name when run inside a Libra repository, a URL, or a local Git/Libra repository path.

## Options

| Flag | Description | Example |
|------|-------------|---------|
| `--heads` | Show only `refs/heads/*` branch refs | `libra ls-remote --heads origin` |
| `-t`, `--tags` | Show only `refs/tags/*` tag refs | `libra ls-remote --tags origin` |
| `--refs` | Omit `HEAD` and peeled tag refs ending in `^{}` | `libra ls-remote --refs origin` |
| `--get-url` | Resolve and print the configured URL without contacting the remote | `libra ls-remote --get-url origin` |
| `--exit-code` | Exit with status 2 when discovery succeeds but no refs match | `libra ls-remote --exit-code origin main` |
| `--sort <KEY>` | Sort refs by `refname`, `-refname`, `version:refname`, or `-version:refname` | `libra ls-remote --sort=version:refname --tags origin` |
| `--symref` | Print symbolic-ref targets advertised by the remote (e.g. `ref: refs/heads/main\tHEAD`) above the matching ref | `libra ls-remote --symref origin` |
| `patterns...` | Match full ref names or trailing path components; `*` and `?` follow Git-style glob behavior and can match `/` | `libra ls-remote origin main 'refs/heads/*'` |

## Human Output

Each matching ref is printed as:

```text
<object-id>	<refname>
```

Example:

```text
4f3c2d1a...	HEAD
4f3c2d1a...	refs/heads/main
```

## JSON Output

With `--json`, output uses the standard command envelope:

```json
{
  "ok": true,
  "command": "ls-remote",
  "data": {
    "remote": "origin",
    "url": "https://example.com/repo.git",
    "heads_only": false,
    "tags_only": false,
    "refs_only": false,
    "get_url": false,
    "exit_code": false,
    "sort": null,
    "patterns": [],
    "entries": [
      {
        "hash": "4f3c2d1a...",
        "refname": "refs/heads/main"
      }
    ]
  }
}
```

## Examples

```bash
# List all refs from a named remote
libra ls-remote origin

# List all refs from a URL directly (no remote registration required)
libra ls-remote https://example.com/repo.git

# Restrict to branches matching a pattern
libra ls-remote --heads origin main

# Resolve a configured remote URL without discovery
libra ls-remote --get-url origin

# Sort tags with version-aware refname ordering
libra ls-remote --sort=version:refname --tags origin

# Show symbolic-ref targets (HEAD) advertised by the remote
libra ls-remote --symref origin

# Structured JSON envelope for agents, tags only
libra --json ls-remote --tags origin
```

The same banner is rendered by `libra ls-remote --help` so the doc and
the CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout,
see `docs/development/commands/_general.md` item B).

## Notes

- `ls-remote` performs only protocol discovery (the in-process equivalent of `git-upload-pack --advertise-refs` for local Git repositories — Libra reads their refs directly).
- It does not write objects, remote-tracking refs, config, or working-tree files.
- `--heads` and `--tags` can be combined to show both branch and tag refs while excluding `HEAD`.
- `--get-url` exits before protocol discovery and prints the same redacted URL form used by remote diagnostics.
- `--exit-code` is a silent script signal: no matches returns status 2 without rendering an error.
- `--symref` prints a `ref: <target>\t<name>` line above a symbolic ref's own OID line when its name survives the active filters. Advertised `symref=` capabilities remain authoritative. If a transport omits that capability (notably a local Libra source), Libra derives `HEAD` from the advertised HEAD OID and branch tips using the same deterministic resolver as fetch (OID match, then `main`, `master`, first branch). JSON reports the same result in `symrefs[]`.

## Malformed HTTP(S) discovery responses

During HTTP(S) reference discovery, Libra rejects a zero-byte advertisement and
malformed pkt-line frames, including short or non-hexadecimal headers, frame
lengths below four, and truncated payloads. A valid `0000` flush remains distinct
from an absent response; a valid empty-repository advertisement is supported.
An unsupported object-format capability reports the fixed message
`Unsupported object format capability` without echoing its remote value.
Check that the URL points to a Git smart HTTP service and that a proxy has not
truncated or replaced the response; then retry.

## pkt-line error classification

Detected pkt-line framing errors return `LBR-NET-002` (exit 128), including an
empty HTTP(S) discovery advertisement. Ordinary connection failures, resets and
timeouts return `LBR-NET-001` (exit 128). Verify the Git service and any proxy
response when a protocol error occurs. Discovery framing errors use the hint
`check that the remote serves Git data and that a proxy has not altered the response`.

This classification applies to reference discovery. Authentication failures and
local configuration read errors keep their existing error codes.

## SSH advertisement error handling

SSH advertisement frames with lengths `0001` through `0003`, incomplete headers
(including zero-byte EOF), or truncated payloads return `LBR-NET-002`. The fixed
protocol reason and its marker are retained; captured SSH stdout/stderr is never
inserted into that protocol error.

A missing advertisement can also mean SSH failed before Git negotiation, for
example because of connectivity, host trust, authentication or repository access.
This release still reports that incomplete advertisement as `LBR-NET-002`. When
SSH's local non-zero exit status is available, the message adds only
`SSH exited with status N` and fixed guidance to check SSH connectivity, trusted
host keys, ssh-agent authentication and remote repository access. The original
SSH stderr is not shown in this protocol diagnostic; specific host-key guidance
is not yet provided by this path.

After an incomplete required header, Libra allows up to 100 milliseconds for SSH
to report its exit status, then requests termination if it is still running.
Other advertisement read errors request termination immediately. The status
window and direct-child cleanup share a two-second total budget. Cleanup failure
does not replace the primary protocol reason. This does not promise cleanup of
arbitrary descendant processes.

Ordinary IO and timeout errors keep their transport classification. Cleanup can
terminate a running child, so its reported exit status and the amount of
available diagnostics can change; a local cleanup warning is appended to any
collected process result. Interactive stderr inheritance and other SSH
process-error diagnostics retain their existing behavior, so this change does
not suppress every SSH terminal message.

The `git://` object-fetch path already reports these frame errors as `LBR-NET-002`.
Git discovery can still report them as `LBR-NET-001`. Non-ASCII/non-hex headers
retain their existing classification. HTTP(S) behavior is unchanged.
