# `libra op`

Inspect and restore command-level operation history.

## Synopsis

```bash
libra op log [OPTIONS]
libra op show [OPTIONS] <OP_REF>
libra op restore [OPTIONS] <OP_REF>
libra op reconcile [OPTIONS]
```

## Description

`libra op` provides a command-line surface over the Operation v2 graph.

It currently supports these subcommands:

- `op log`: list recorded operations with pagination and optional command filter.
- `op show`: inspect one operation and, optionally, the captured restore view.
- `op restore`: move HEAD and branch refs back to a previously captured view.
- `op reconcile`: converge concurrent operation heads when their states are
  provably unambiguous.

## Operation References

`<OP_REF>` may be either:

- A concrete operation id, for example `019e3f00-8ee5-7e62-a54c-0ab1f1bba0f9`
- A reflog-style index, for example `@{0}` for the newest operation or `@{1}`
  for the previous one

Indices use one newest-first history across Operation v2 records. Entries such
as `external.snapshot`, undo, redo, and reconcile appear in that same history.
The `index` in `op log --json` is the zero-based index for the complete history;
command filters and pagination do not renumber it. Thus `op show @{n}` and
`op restore @{n}` target the operation displayed at index `n`.

## `libra op log`

List operation history.

```bash
libra op log [--page <N>] [-n <PER_PAGE>] [--command <NAME>] [--verbose]
```

### Options

### `-n, --number <PER_PAGE>`

Number of operations to show per page. Defaults to `50`.

```bash
libra op log -n 20
```

### `--page <N>`

Page number to display. Defaults to `1`.

```bash
libra op log --page 2 -n 20
```

### `--command <NAME>`

Filter operations by exact command name, such as `branch` or `op restore`.

```bash
libra op log --command branch
libra op log --command "op restore"
```

### `--verbose`

Show one operation as a multi-line block with actor, status, and timestamp.

```bash
libra op log -n 5 --verbose
```

## `libra op show`

Inspect a single operation.

```bash
libra op show [--view] <OP_REF>
```

### Options

### `--view`

Print the captured restore view, including HEAD target and refs.

```bash
libra op show @{0} --view
```

## `libra op restore`

Restore the supported HEAD/ref state from a previously captured operation view,
not arbitrary working-tree or nested-repository contents. HEAD and the
captured branch refs are reset to the target view, and local branches that are
absent from that view are pruned, so the restore reproduces the operation's
exact local-branch set. The restored HEAD branch is always kept; remote-tracking
refs and Libra-owned internal refs (the locked `main`/`intent`/`traces`
branches and the reserved `libra/` namespace, e.g. the AI history branch
`libra/intent`) are never pruned.

New operation snapshots omit the repository-local Memory authority. When an
older snapshot already contains `libra/memory/repo`, restore skips it, reports
the skipped name, and leaves both its current object ID and projection
watermark unchanged.

```bash
libra op restore [--force] [--dry-run] <OP_REF>
```

### Options

### `--force`

Allow restore to proceed even if the working tree is dirty. This does not add
restore capabilities or turn a `Partial` capture into a complete snapshot.

```bash
libra op restore @{0} --force
```

### `--dry-run`

Show the target HEAD and refs without writing a new restore operation.

```bash
libra op restore @{0} --dry-run
```

## Examples

```bash
# List the newest ten operations
libra op log -n 10

# Show only branch operations on page 2
libra op log --command branch --page 2 -n 5

# Inspect the latest operation and its view snapshot
libra op show @{0} --view

# Restore to the previous operation view
libra op restore @{1}

# Preview a restore without changing repository state
libra op restore @{1} --dry-run
```

## `libra op doctor`

Diagnose operation object closure, heads, unfinished journals, and the
workspace pointer. Read-only by default; `--fix` performs journal recovery and
pointer rebuild, `--dry-run` only reports the planned repairs.

```bash
libra op doctor [--fix] [--dry-run]
```

`--fix` recovers interrupted operations that never reached a terminal state.
A command that already published its head (the operation completed its mutation
before the process died) is advanced to `success` and, when it is the current
head, the workspace pointer is rebuilt to its captured view. A globally
orphaned running operation (crash before head publication) is failed closed;
the next mutation boundary records any on-disk drift as an external snapshot.


## Notes

- `op restore` records a new `op restore` operation on success.
- `op restore --dry-run` does not write a new operation.
- Restore resets HEAD and the branch refs captured in the target view, and
  prunes local branches that are absent from that view (the restored HEAD branch
  is always kept; remote-tracking refs are left untouched).
