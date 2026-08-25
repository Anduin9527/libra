# `libra review`

Run read-only code reviews with external agent CLIs (AG-22).

## Synopsis

```bash
libra review --agent <slug>... [--since <rev>] [--checkpoint <id>] [--json]
libra review --fix [--json]
libra review list [--json] [--limit <n>] [--cursor <token>]
libra review show <run_id> [--json]
libra review cancel <run_id>
libra review clean [--run <run_id>] [--all]
libra review attach <run_id> <file> [--json]
```

## Artifacts

A finished run objectizes its `findings.md` into the object store: the run
manifest's `findings_oid` is a content-addressed blob, `object_index`-tagged
so cloud sync and retention track it and `libra agent doctor` can repair it if
it goes missing. `libra review attach <run_id> <file>` records an external
file on the run's audit chain with `provenance=manual` — the bytes are
redacted, objectized the same way, and appended to the manifest's
`manual_attach` list. Attach never modifies findings or run state.

## Description

`libra review` fans a fixed review prompt out to one or more external
reviewer CLIs and records their findings as an auditable run. The
first-batch launchable reviewers are `claude-code`, `codex`, and
`opencode`; any other agent slug is refused with an actionable error
before anything is spawned.

Every reviewer runs in an **isolated workspace** — a mirror of the
repository materialized with ignore rules applied (gitignored secret
files such as `.env.test` never enter it) — with a minimal read-only
CLI invocation and an environment cleared down to a documented
allowlist. Reviewers never run in the repository worktree itself.

A run blocks in the foreground until every reviewer reaches an outcome
and ends in exactly one of five terminal states: `success`, `error`,
`cancelled`, `timeout`, or `partial`. Pressing Ctrl-C (or sending
SIGTERM) cancels the run through the same cleanup path as
`libra review cancel`: reviewer process trees are killed, reader tasks
drained, the workspace released, and the run stamped `cancelled`.

Run state is persisted under `.libra/sessions/agent-runs/<run_id>/`:
`state.json`, `manifest.json`, `findings.md`, and per-reviewer
`reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`. All
persisted reviewer output goes through the secret-redaction pipeline;
reviewer output is capped at 64 KiB per stream (a flooding reviewer is
truncated with a marker and never blocks its siblings).

Reviewer findings are **untrusted free text**. `libra review show`
always strips ANSI/terminal control sequences before rendering
`findings.md`, so a hostile reviewer cannot forge terminal output, and
the JSON output carries the same sanitized rendering.

### Scope selection

The recorded `target_scope` labels what the reviewers were asked to
review:

- default: `HEAD~1..HEAD` (the last commit's changes);
- `--since <rev>`: `<rev>..HEAD`;
- `--checkpoint <id>`: `checkpoint:<id>` (an agent checkpoint from
  `libra agent checkpoint list`). The reviewers' workspace is the
  checkpoint's own captured content — `metadata.json`, an optional
  `manifest.json`, and the transcript files — materialized READ-ONLY
  under the run directory (`<run_dir>/checkpoint-input/`). It is never
  disguised as a worktree diff: no repository snapshot is materialized
  at all, so a scoped run can only consume the named checkpoint, and the
  scoped prompt tells reviewers they are looking at a captured
  transcript whose content is untrusted data. A missing checkpoint, a
  malformed checkpoint tree, or content that is not present in the local
  object store fails closed BEFORE any run is created (no run residue).
  The materialization lives inside the run directory, so it shares the
  run's retention exactly (`review clean` removes it with the run) and
  `agent doctor` needs no new orphan class.

### Pagination

`libra review list` uses the unified keyset pagination contract:
default `--limit 50`, capped at 500, ordered `created_at DESC, run_id
DESC`. The JSON envelope carries `schema_version`, `items`,
`next_cursor` (opaque — round-trip it verbatim), and `has_more`.

### `--fix`

`libra review --fix` requires an active, authorized `libra code --control
write` session for the same worktree. It reuses that session's loopback-only,
token-authenticated controller path to submit one fixed trusted request to the
existing serialized AgentRuntime; it does not create another mutation queue.
Before submission, the Code session must be idle, with no pending interaction
or plan-execution repair state. Otherwise the bridge returns `LBR-AGENT-040`
without submitting a new request. Resolve any old interaction in its Code
session; if a repair projection remains, start a fresh authorized Code session
before retrying.
While the request is active, the foreground command holds the existing
automation-controller lease and relays only the user's explicit terminal
choices or form answers back to that runtime. Reviewer findings, transcripts,
environment values, and other external seeds are never included in the request.

`--fix` conflicts with `--agent`, `--since`, and `--checkpoint`: those options
select a reviewer or input for a read-only review run and are never silently
ignored by controlled execution.

The active Code runtime remains the only writer. It asks the user for the normal
intent/plan choices, user-input forms, network choices, tool approvals, sandbox
approval, and ACL checks; the CLI never auto-approves one. The four controlled
outcomes are explicit:

Choosing `modify` or `cancel` at intent review, or `modify`, `back`, or `cancel`
after plan review, hands control back to the active Code session immediately and
ends this foreground bridge with `LBR-AGENT-040`; continue the requested revision
in that session.

- a denied tool approval returns `LBR-AGENT-039` before a patch is reported;
- a denied sandbox approval returns the same stable code before a patch is
  reported;
- a tool failure returns the runtime's existing deterministic repair state as
  `execution: "repair_required"`, never a silent patch success;
  `patch_applied` truthfully reports whether the runtime projected a partial
  patch before the failure;
- success is reported as `execution: "patch_applied"` and
  `patch_applied: true` only after the existing runtime completed all applicable
  gates and projected a new patchset.

If no authorized session is available, the command fails closed with
`LBR-AGENT-010`. Malformed interaction responses, incomplete execution, or a
patch observed before a later denial return `LBR-AGENT-040` without claiming a
clean success; inspect both the active Code session and worktree before retrying.
An untrusted seed is rejected before the control endpoint is discovered with
`LBR-AGENT-011` by the shared helper. The current `review --fix` CLI has no seed
parameter, so that error is not a reachable CLI outcome yet.

## Examples

```bash
# Review the last commit with one reviewer
libra review --agent codex

# Fan the same review out to two reviewers concurrently
libra review --agent codex --agent claude-code

# Review everything since a revision
libra review --agent codex --since v1.2.0

# Review a captured checkpoint's transcript/metadata (read-only input)
libra review --agent codex --checkpoint <checkpoint_id>

# Structured run result (terminal state, per-reviewer outcomes)
libra review --agent codex --json

# Prepare and execute a review-fix through an already running Code session.
# The terminal asks for every applicable plan, approval, sandbox, and ACL choice.
libra code --control write
libra review --fix

# List runs, then fetch the next page
libra review list
libra review list --limit 10 --cursor <token>

# Inspect one run (state, manifest summary, sanitized findings)
libra review show <run_id>
libra review show <run_id> --json

# Cancel a running review (same cleanup as Ctrl-C)
libra review cancel <run_id>

# Remove run directories
libra review clean --run <run_id>
libra review clean --all
```

## Concurrency

`review` and `investigate` share one run-level concurrency budget across the
repository. At most `agent.max_concurrent_runs` runs (default `2`) execute at
once; a run started while the budget is saturated waits in a queue (blocking
the foreground process — `Ctrl-C` cancels the wait and advances the queue for
others). If the wait queue is already at its cap (10), the new run is refused
fail-closed with the stable code `LBR-AGENT-014` (exit 128) rather than
overrunning the budget. Raise the limit with
`libra config set agent.max_concurrent_runs <N>`.

## Exit Status

- `0` — the run reached `success`, `partial`, `timeout`, or `cancelled`
  (the terminal state is reported in the output); subcommands succeeded; or
  `--fix` reports either an observed patch (`patch_applied: true`) or the
  explicit runtime repair state (`execution: "repair_required"`).
- non-zero — usage errors, a run that ended in the `error` terminal
  state, unknown run ids, an unknown or non-materializable
  `--checkpoint` (refused before any run is created), `--fix` without an
  authorized active Code session (`LBR-AGENT-010`), denied runtime approval or
  sandbox gates (`LBR-AGENT-039`), an incomplete controlled execution
  (`LBR-AGENT-040`), or a full run queue (`LBR-AGENT-014`).

## See Also

- `libra agent` — external-agent capture, checkpoints, and hooks
- `docs/development/commands/review.md` — architecture and security notes
