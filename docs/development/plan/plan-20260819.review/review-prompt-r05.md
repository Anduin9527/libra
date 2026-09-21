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

## R30-r03 附加要求（第三次複審）

本輪是 r02 修訂後的複審。r02 結果：Codex `0×P0 + 7×P1`、Claude `1×P0 + 7×P1 + 12×P2`；修訂決策見 `fix-checklist-r02.md` 與計畫「修訂歷史」2026-09-21 r02 行。請：

1. 逐條核對 r02 的 findings 是否已關閉（給出實際核對命令/行號）。
2. 全量複審，特別核對：`TerminalEvidenceRefV1`（plane/kind/oid/locator/digest）與 `parent_intent_root_id`/`related_run_ids` 是否貫穿 ADR-M2-05/07、payload 表、M2-07/08/09/10/16C/16D1、冪等指紋；`libra memory record` 補錄路徑是否閉環；`terminal_status` 四態與 `completion_status` 映射；observer 表不再承諾 `libra/intent`；M2-16B 縮為 `memory/model.rs` 後 `M207→M216B→M208` 的 DAG/依賴/所有權；M2-16D1 的 `DEFER-M2-09` 墓碑與收口集合；T-1/T-4 觸發與 `tests/SERIAL_REGISTRY.tsv`/`.config/nextest.toml` 基建；M2-16C 錯誤碼與 ER-06a；PR→squash→tag 的 D-01 流程；三張 migration 卡的 `EX-M2-01` AC 拆分與計數。
3. 輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r04 附加要求（第四次複審）

本輪是 r03 修訂後的複審。r03 結果：Codex `0×P0 + 9×P1 + 4×P2`、Claude `1×P0 + 10×P1 + 13×P2`；修訂決策見 `fix-checklist-r03.md` 與計畫「修訂歷史」2026-09-21 r03 行。請逐條核對 r03 findings 是否關閉，並全量複審。重點：

1. `bridge_event` source plane（`(bridge_session_id,event_seq)` + `payload_sha256`）是否貫穿 ADR-M2-05、M2-07、M2-16C、故障矩陣；bridge host 僅 `deepseek-harness` 的表述是否正確。
2. `related_run_ids` 是否進入 Task/Intent/record 冪等指紋與 M2-16D1 重放判據；status/evidence mismatch 回歸是否在 bridge、`observe_terminal`、`memory record` 三處都有具名 VER。
3. `EX-M2-01` waiver 表是否符合模板 8 列格式、任務列 = M2-02/02F/02R、三卡「判據規範（非計數正文）」塊是否齊備；`AC=9/8@EX-M2-01` 與審計表一致。
4. VER 計數（宣稱 vs 實際）與 G-03 計數口徑；`sqlite_fts5_release_capability` 唯一 target；`--test-threads` 零殘留；零命中守衛是否為模板 exit-code 形式。
5. DAG（`M216D→M216E`、D1 條件邊）與各卡 `Dependencies`/審計表/Granularity 一致；Phase 0 不含 M2-16B。
6. 模板 v2.8 的 ER-14 範圍（T-2 全量、其餘卡 nextest focused）、ER-13/完成判據的 `.env.live-test`、`web`/`worker` 注記是否自洽。
7. 事實基線錨點（`ai/mod.rs`、`client_storage.rs`、`base.yml`、`config.rs`）是否在核對日 commit 上成立。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r05 附加要求（第五次複審）

本輪是 r04 修訂後的複審。r04 結果：Codex `0×P0 + 6×P1 + 3×P2`、Claude `0×P0 + 2×P1 + 13×P2`；修訂決策見 `fix-checklist-r04.md` 與計畫「修訂歷史」2026-09-21 r04 行。請逐條核對 r04 findings 是否關閉，並全量複審。重點：

1. 關係欄位完整冪等指紋：`parent_intent_root_id` + canonicalized `related_run_ids` 是否貫穿 ADR-M2-05/07、M2-08 AC 與具名回歸（Task/Intent/record），僅關係變化必須產生新 generation。
2. M2-13 `memory record` 的受驗證 envelope 是否同時支援 `TerminalObservationV1` 與 host `EpisodeProposalV1`（`model_label`），且無 model/無 proposal 時 job 保持 pending；CLI 補錄→Episode E2E 與 mismatch 回歸是否具名。
3. ER-14 focused 證據映射：§0.5、測試矩陣與模板是否明確「卡內 `cargo test` 為診斷、C 組以等價 `cargo nextest run` filter 重跑」。
4. M2-16C 粒度（bridge 校驗 vs M2-08 語義分離）、M2-16B 的 `tests/INDEX.md`、`EpisodeProposalV1` 首建歸屬 M2-07。
5. 里程碑 M0/M2 與 DAG/Phase 一致；`EX-M2-01` waiver 表 8 列、Approver/證據、三卡判據規範塊。
6. 模板 v2.8 自身（ER-13/ER-14/完成判據的雙 env、`web`/`worker` 注記、縮排）與 plan-status（r04/r05、DEP-M2-ENV-01、DEFER-M2-01..09）。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。
