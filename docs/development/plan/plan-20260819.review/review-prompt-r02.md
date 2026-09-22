# plan-20260819 计划评审提示（R30 双评审，只读）

你是一名严格的计划评审员。请在**只读**模式下完整评审：

- 计划：`/run/media/genedna/data/libra-main/docs/development/plan/plan-20260819.md`
- 模板：`/run/media/genedna/data/libra-main/docs/development/plan/plan-template.md`（版本 `v2.7`）
- 仓库：`/run/media/genedna/data/libra-main`（libra-tools/libra，`main` 分支工作树）

## 背景（必须自行核对实际仓库，不要只信计划自述）

1. `origin/main`（0.23.x 拆除计划）已删除 Code 时代 AI SCC：`src/internal/ai/providers/*`、`src/internal/ai/context_budget/*`、`src/internal/ai/client.rs`、`src/internal/ai/prompt/`、`src/internal/ai/mcp/`、`src/internal/ai/orchestrator/`、`src/internal/ai/web/`、`src/internal/operation_wrapper.rs`、`tests/operation_wrapper_test.rs`；Code-era `TaskEventKind`/`IntentEventKind` 生产者也已消失。
2. main 仍保留：`git-internal 0.10.2` 物件模型（Task/Intent/Plan/TaskEvent/IntentEvent/RunEvent/Decision/Evidence/PatchSet）、`src/internal/ai/agent_run/**`（含 `AgentRunStatus::{Completed,Failed}` 与 `AgentRunEventStore`）、session/JSONL 捕获、`agent_bridge`、hooks/observed_agents、`completion` trait 层、operation v2、vault/encrypted config。
3. PR #456（`Anduin9527:codex/memory-m2-core-draft-pr`，head `bc1be587e4`）是 memory 模块实现草案，距 main 296 commits，与 main 合并有 39 档冲突（11 UD + 28 UU）且依赖上述已删模块，不能直接 merge。
4. main 最新迁移 tip 为 `2026091901_operation_boundary_claim_columns`；#456 的 memory 迁移 `2026090701..03` 低于该 tip。
5. 计划声明 R30 已把整份计划迁到模板 `v2.7`（ER-08 每卡 `patch + 1` + `gh release`；ER-14 nextest 全量门），并要求 Codex 与 Claude 双字面 `VERDICT: PASS` 后才可开工。

## 重点检查项

1. **可执行性**：逐卡核对所有 `file:line`、命令、路径、迁移号、feature 引用在该仓库中是否存在或确实由卡片创建；依赖 DAG 是否无环、无遗漏；每卡的 `Release boundary` / `Version increment` / `Release write set` / `C/D coverage from` / `Full-suite trigger` 是否符合模板 v2.7（ER-08、ER-13、ER-14、G-07/G-07a/G-08）；卡片字段、粒度审计表、实施顺序 DAG、发布分组、追溯表、里程碑、风险表、完成判据是否互相一致（含卡数与任务列表）。
2. **架构自洽**：ADR-M2-05/07/10 的改写与新增 ADR-M2-12/13/14/15、§0 的 seam（`EpisodeCompilerModel`、`MemoryContextBundleV1`、`TerminalObservationV1`、迁移重编、#456 正向移植）是否足以替代被删表面；全计划是否还残留对 `providers`/`context_budget`/`prompt`/`client`/`mcp`/`orchestrator`/`web`/`operation_wrapper`/`test-provider` 的**执行性**依赖（历史说明与禁止清单除外）。
3. **任务卡质量**：AC/VER 上限、writeset、依赖、证据锚点、测试命令与零命中守卫是否自洽；`(new)` 标记与 `tests/INDEX.md` 同步是否闭环；M2-16A..E 与既有 19 张卡的依赖关系（M2-16A→M2-01→M2-01K→…、M2-16B→M2-09、M2-16C→M2-12、M2-16D→M2-08、M2-14C/16D→M2-16E→M2-15）是否正确写入所有相关字段。
4. **安全与数据完整性**：keyed digest、redaction/trust gate、ref 保护（operation v2）、receipt retention、迁移重编的 fail-closed 语义是否有漏洞或未测试假设；host 未上报终态时的补录路径是否完整。
5. **文档与兼容**：`memory.md`、`mainline.md`、bridge protocol、CLI/error codes、`COMPATIBILITY.md`、`tests/INDEX.md` 的同步是否遗漏；#456 收口映射是否可验证。

## 输出要求（严格遵守）

- 只读评审：不得修改仓库任何文件。
- 按 `P0` / `P1` / `P2` 分组输出。每条包含：**位置**（`file:line` 或章节名）、**问题**（what）、**为何重要**（why）、**最小修复**（minimal fix）。
- P0/P1 必须是会导致计划无法执行、架构不成立、数据/安全风险或模板规则违规的具体问题；不要把风格偏好或可选优化列为 P1。
- 若发现计划自述与仓库事实不符，必须给出你实际核对的命令/工具与结果。
- 最后一行必须且只能是 `VERDICT: PASS` 或 `VERDICT: FAIL`。PASS 要求字面 `P0=0` 且 `P1=0`。

## R30-r02 附加要求（複審）

本輪是修訂後的複審。r01 的 findings 清單與修訂決策在 `docs/development/plan/plan-20260819.review/fix-checklist-r01.md`（Codex r01：0×P0 + 11×P1；Claude r01：4×P0 + 13×P1 + 9×P2）。請：

1. 逐條核對 r01 findings 是否已關閉，給出你實際核對的證據（命令/文件行）。
2. 全量複審修訂後的計畫，包括同一變更中的 `plan-template.md` v2.8（版本面集合權威改以 `compat_version_surface_sync` 為準）、`plan-status.md` §3.3 與新增審計文件。
3. 特別核對：版本面集合與模板 v2.8 一致性；遷移所有權（M2-16A 只移植領域層，`2026092101/02/03` 分別由 M2-02/02F/02R 擁有）；GC-13 角色與隔離斷言；M2-08 的 `observe_terminal`/`TerminalObservationV1`；model 交付閉環（bridge host proposal 或進程內 registry）；Episode root opaque 與 `terminal_status` 佐證；M2-16D/D1 拆分；每卡 `Full-suite trigger` 值；`DEP-M2-ENV-01`；M2-13 的 EXAMPLES 三道守衛與 ER-06a。
