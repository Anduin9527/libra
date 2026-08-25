# `libra review` 开发设计

## 命令实现目标

`libra review`（plan.md Task A7 / AG-22）交付 read-only 的外部 agent review
workflow：把一段 spotlighting 定界的固定 review prompt 扇出给首批三个外部
reviewer CLI（`claude-code`、`codex`、`opencode`），在隔离 workspace 中以最小
权限只读形态运行，reviewer 输出经有界 sink + redaction 落盘为可审计的 run
目录，并保证每个 run 恰好收敛到五个 terminal state 之一。`--fix` 不消费外部
review findings，而是把一条固定可信请求交给已运行的内部 AgentRuntime，并逐项
转发用户对既有 plan/approval/sandbox/network/ACL gate 的明确决定。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Libra read-only agent review
  extension (AG-22), not a Git command。
- 该命令是 Libra AI 扩展；重点是隔离执行、可审计 run wire、结构化输出与
  fail-closed 错误，而不是 Git 同形。

## 设计方案

- 入口与分发：顶层命令 `src/cli.rs::Commands::Review`（CLI 面固定为顶层命令，
  与 `agent` 平级归入 ROOT_AFTER_HELP 的 "AI And Automation" 组）。实现文件为
  `src/command/agent/review.rs`——放在 `command/agent/` 下是为了复用
  `checkpoint.rs` 中 `pub(super)` 的 AG-20 keyset 分页助手
  （`resolve_page_limit` / `encode_page_cursor` / `decode_page_cursor`）。
- 引擎分层：`src/internal/ai/review/`：
  - `store.rs` —— run 目录 store（`ReviewRunStore`）：创建/加载/枚举/取消标记/
    清理，keyset 排序（`created_at DESC, run_id DESC`）；
  - `launcher.rs` —— §0.3.2 生产 argv builder + 最小 allowlist spawn 骨架；
  - `sink.rs` —— 64 KiB 有界捕获、redaction、控制字符清洗、
    `render_untrusted_findings`（ANSI/控制序列剥离）；
  - `runner.rs` —— 并发 fan-in→串行 sink 的 run loop、五个 terminal state、
    共享 cancel/cleanup、`agent.review.run` span。
- 参数模型：`ReviewArgs`（`subcommand_negates_reqs` +
  `args_conflicts_with_subcommands`）：裸 `review --agent <slug>...
  [--since <rev>] [--checkpoint <id>]` 运行只读 review；`review --fix` 运行
  受控修复，并与 `--agent` / `--since` / `--checkpoint` 冲突；子命令
  `list [--limit] [--cursor]`、`show <run_id>`、`cancel <run_id>`、
  `clean [--run <id>|--all]`。全局 `--json` 输出结构化 envelope。
- `target_scope` 推导（纯函数，单测钉死）：默认 `HEAD~1..HEAD`；
  `--since <rev>` → `<rev>..HEAD`；`--checkpoint <id>` → `checkpoint:<id>`
  （PD-02 已落地：命令层先经 `checkpoint.rs::resolve_checkpoint_input_spec`
  解析并验证 checkpoint——不存在/树非法/blob 不在本地对象库时在创建任何 run
  之前 fail-closed；随后 runner 把 checkpoint 的整个 inner tree 以只读文件
  物化到 `<run_dir>/checkpoint-input/`（`internal::ai::checkpoint_input`，
  单文件 64 MiB / 总量 256 MiB 上限，路径组件消毒防逃逸），并把该目录作为
  reviewer 的**全部** workspace——完全不物化 worktree 快照，scoped prompt
  明确声明这是捕获的 transcript 而非仓库快照，transcript 内容按不可信数据
  对待。物化产物在 run 目录内，与 run 共享 retention/orphan-release 面）。
  scope 只是记录在 state/manifest 中的人类可读标签；prompt 用 spotlighting
  定界把它作为数据（非指令）注入固定指令文本。
- 输出与错误契约：全部经 `OutputConfig` / `emit_json_data` / `CliError`；
  `list`/`show`/`cancel`/`clean`/run 的 JSON envelope 均带 `schema_version`；
  `list` envelope 为 `{schema_version, items, next_cursor, has_more}`（统一
  分页契约，默认 50 / cap 500 / 不透明 keyset cursor）。

### `--fix` 受控执行

- `runtime::fix_bridge` 只负责固定请求、trusted/untrusted provenance 与稳定错误；
  `runtime::fix_control` 复用同 worktree 的 loopback/token/controller 协议；
  `runtime::fix_protocol` 在 HTTP 边界把 session/interaction/patch/repair 投影解析为
  强类型；`runtime::fix_execution` 驱动唯一状态机，不创建第二条 mutation queue。
- 提交固定请求前必须观察到 `idle`、无 pending interaction、无 plan-execution repair；
  任何遗留状态都在 submit 前以 `LBR-AGENT-040` fail-closed，避免把旧 approval 或
  repair 错认成本次请求的结局。
- 前台 responder 只呈现 runtime 的 typed interaction，并把用户原样决定送回；敏感
  问题隐藏输入，终端文本先剥离控制序列，单次输入与整次响应都有大小上限。
- automation controller lease 在模型规划和用户等待期间定期以同一 client id 续期；
  token 发生替换说明旧 lease 已过期/接管，立即以 `LBR-AGENT-040` fail-closed，并在
  返回前尝试 detach。
- denial 只有在 runtime 接受决定且未观察到新 patchset 时返回 `LBR-AGENT-039`；
  即使随后 detach 失败，已确定的 denial 仍保持 039；只有原本成功的 execution 会因
  detach 失败转为 `LBR-AGENT-040`。
  tool failure 返回既有 `repair_required`，并如实携带是否已有部分 patch；只有 runtime
  回到 `idle` / `completed` 且出现新 patchset 时才报告 clean `patch_applied`。
- 无 session/无授权仍为 `LBR-AGENT-010`；外部 seed 在发现 endpoint 前为
  `LBR-AGENT-011`。任何中途传输失败、异常 token、indeterminate state 或 patch-before-
  denial 都是 `LBR-AGENT-040`，提示用户同时检查 Code session 与 worktree。

### Run 目录布局（E8-libra run wire）

```text
.libra/sessions/agent-runs/<run_id>/
  state.json          # schema_version、agents（逐 reviewer outcome）、scope、terminal_state、cancel_requested
  manifest.json       # E8 精确键集：schema_version、run_id、kind、agents、starting_sha、
                      #   target_scope、terminal_state、created_at、updated_at、
                      #   findings_oid、redaction_report、manual_attach
  findings.md         # raw-redacted、spotlighting 定界、provenance=untrusted
  cancel.requested    # 跨进程取消标记（存在即请求；runner 每 200ms 轮询）
  reviewers/<slug>.stdout.redacted.log
  reviewers/<slug>.stderr.redacted.log
```

`manual_attach` 由 `libra review attach <run_id> <file>`（A0-06）填充：外部文件
字节先经 `redact_untrusted` 脱敏，再内容寻址对象化（object_index `o_type =
agent_findings`），manifest 追加 `{oid,name,provenance:"manual",size,attached_at}`
条目（只存 basename，防路径泄露）。`findings_oid` 在 finalize 时由 findings.md
内容寻址写入；`libra agent doctor` 覆盖 `missing_findings_object` /
`missing_findings_object_index` 两类扫描与修复。

### Terminal states 与 cancel

五个 terminal state：`success`（全部 reviewer exit 0）、`partial`（部分成功）、
`timeout`（无一成功且至少一个超时）、`cancelled`（取消路径）、`error`
（基础设施失败或无一成功且无超时）。聚合真值表在 `store.rs`
`aggregate_terminal_state` 单测钉死。

cancel 是**一条**共享 cleanup 路径（`ReviewCancelHandle`）：

1. 前台 run 的 SIGINT/SIGTERM（`tokio::signal`，`service run` 模型）→
   `cancel()`；
2. `libra review cancel <run_id>` 写 store 的 `cancel.requested` 标记，
   live runner 轮询到后 → `cancel()`；CLI 侧等待最多 3s（15×200ms）确认
   live runner 收敛；无人认领（orphaned run）时直接
   `store.mark_cancelled`（同一 terminal 记账收敛点）。

两条路径都最终执行：杀 reviewer 进程组（进程树）、join reader task、释放
workspace lease、`cancelled` 落盘。

### §0.3.2 spawn 形态（冻结；上游 CLI 变化时按 §0.3.2 复核）

| slug | argv |
|---|---|
| `codex` | `codex exec -C <workspace> --skip-git-repo-check --sandbox read-only --json -o <file> <prompt>` |
| `claude-code` | `claude -p --permission-mode plan --output-format stream-json --verbose --include-hook-events --max-budget-usd <small> <prompt>` |
| `opencode` | `opencode run --dir <workspace> --format json --title <name> <prompt>` |

禁用 flag（argv 中绝不出现）：codex `--ephemeral`；claude `--bare` /
`--safe-mode` / `--no-session-persistence`；opencode
`--dangerously-skip-permissions`。非首批 slug 是结构化 unsupported 错误，
永不 spawn。

### 安全说明

- **隔离 workspace 必选**：reviewer 一律运行在
  `materialize_isolated_workspace`（`sub_agent_dispatcher.rs` 抽取的 public
  seam）物化的镜像中，绝不 in-place；copy 后端按 ignore 规则排除（`.env.test`
  等 secret 文件不进镜像）；AG-22 钉死 copy 后端（FUSE overlay 需先补
  ignored-file 不暴露证明）。
- **env allowlist**：spawn 先 `env_clear()`，只注入 `PATH`、`HOME`（三个 CLI
  的 auth/config 所需）；provider API key、`LIBRA_STORAGE_*`、`LIBRA_D1_*`
  等一律不进 reviewer 环境。残余风险：read-only sandbox 不阻断 reviewer 的
  网络能力——第一道防线是 workspace 无 secret + env allowlist，redaction 只是
  落盘兜底。
- **redaction**：所有落盘 reviewer 输出走与 seed 相同的
  `Redactor::new_default()` 管线；每流 64 KiB 有界缓冲（刷屏 reviewer 截断
  加标记，不阻塞串行 sink 或其它 reviewer）；codex `-o` 的原始旁路文件在
  finalize 时删除（未经过 redaction，不得幸存）。
- **untrusted findings**：`findings.md` 为 provenance=untrusted 的
  raw-redacted 文本，spotlighting 定界；`review show`（人类与 JSON 输出均是）
  必经 `render_untrusted_findings` 剥离 ANSI/终端控制序列后才渲染——绝不输出
  原文，防 reviewer 伪造终端输出。
- **`--fix` provenance / writer boundary**：命令不接收 `--agent`、scope 或外部 seed，
  固定请求不含 findings/transcript/env；唯一 writer 仍是现有 AgentRuntime，所有 mutation
  必须通过其 serialized plan execution、hardening、approval、sandbox、network 与 ACL
  gate。denied 路径在补丁前 fail-closed；部分写入或状态不确定时绝不伪装 clean success。

### 可观测性

引擎每次 run 发出一个 `agent.review.run` span（`agent.md` §6）：必带
`run_id`、`agent_count`、`terminal_state`、`duration_ms`；reviewer raw
stdout 为禁止字段。

## 当前状态

- 公开状态：已公开；`src/cli.rs::Commands::Review` + `command::agent::review`。
- 用户文档：`docs/commands/review.md`（zh-CN 同步页
  `docs/commands/zh-CN/review.md`）。
- Synopsis：`libra review --agent <slug>... [--since <rev>]
  [--checkpoint <id>] [--json]` 或 `libra review --fix [--json]`；`list` /
  `show` / `cancel` / `clean`。
- compat 接线：`COMPATIBILITY.md` 顶层矩阵行（intentionally-different）、
  ROOT_AFTER_HELP "AI And Automation" 组行、`REVIEW_EXAMPLES` +
  `after_help`、`tests/compat/help_examples_banner.rs` VISIBLE_COMMANDS 行。

## 还未实现的功能

- `investigate fix` 尚未接入同一 controlled-execution helper；由
  plan-20260824 DF-04 跟踪，不得复制 queue 或放宽 `review --fix` 的 gate。
