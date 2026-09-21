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

## R30-r06 附加要求（第六次複審）

本輪是 r05 修訂後的複審。r05 結果：Codex `0×P0 + 5×P1 + 0×P2`、Claude `0×P0 + 3×P1 + 12×P2`（其中 Claude 的 P1-3 在 r05 修訂中已先行關閉）。修訂決策見 `fix-checklist-r04.md` 與計畫「修訂歷史」2026-09-21 r05 行。請逐條核對 r05 findings 是否關閉，並全量複審。重點：

1. `MemoryRecordEnvelopeV1`：`observation` 必填、`proposal` 可選、proposal-only 拒絕，是否貫穿 ADR-M2-05/12、M2-13、M2-16C、M2-08 與具名回歸。
2. `DEP-M2-SRC-01`：所有 `git ls-tree/grep/-C` 是否只針對只讀 source clone；目標 checkout 的 M2-16A AC 是否已改 `libra status`/`rg --files`。
3. GC-M2-10 的 `reader_state.rs` 是否由 M2-06 交付；M2-14C AC/矩陣/風險行是否一致。
4. M2-05 是否覆蓋 `libra cloud sync/restore` 的 ref/對象排除，且有具名回歸。
5. M2-16C 是否同卡更新 `docs/commands/agent.md` EN/zh 的 20-method 協議契約。
6. M2-16D/E staged C/D（M2-15 D 證據後回填 complete、再關 #456）與 M2-15 AC/依賴/發布分組是否自洽。
7. 全局門（fmt/clippy/release build）是否已從各卡 `Verification` 計數剔除；`EX-M2-02` 是否按模板登記且 M2-13 分子一致。
8. r05 的 12 條 P2（模板 ER-04 第三門、plan-status、doctest、矩陣 Job/recovery、M2-14 寫集、deepseek 守衛 target、job OID 可空、審計表 writeset、M2-16D 守衛、規模等）是否全部收口。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r07 附加要求（第七次複審）

本輪是 r06 修訂後的複審。r06 結果：Codex `0×P0 + 4×P1 + 2×P2`、Claude `0×P0 + 2×P1 + 11×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r06 行。請逐條核對 r06 findings 是否關閉，並全量複審。重點：

1. seam 簽名是否全鏈統一：`MemoryRuntime::observe_terminal(MemoryRecordEnvelopeV1)`（另留 `observe_terminal_observation` 薄封裝），且 §0.3、ADR-M2-05/12、M2-08、M2-13、M2-16C、M2-16D1、故障矩陣無殘留舊簽名。
2. `TerminalObservationFingerprintV1` 是否 Task/Intent/record 復用並含 status/evidence/關係/輸入；status-only、evidence-only、關係-only 變化均有具名回歸。
3. bridge `payload_sha256` 是否僅作 locator 完整性校驗，Memory 指紋/對象/trust gate 只用 `RepositoryKeyedDigest` HMAC。
4. `DEP-M2-DSH-01` 與 bridge「可選 host 面」定位是否閉環（完成判據的 bridge E2E 條件化、M2-16C deps/狀態）。
5. M2-05 `EX-M2-03`（G-04 L-exception、prod-files=14）是否符合模板 waiver 規格，審計表/Granularity/`Estimated scope` 一致。
6. r06 的 P2：跨卡依賴、`writeset` ID、`MemoryRecordEnvelopeV1` 歸屬與版本門、審計表斷行、protocol 20→22 守衛、`EX-M2-02` 措辭、矩陣條件性、裸 `git` 清零、`Dependencies` 空行、無 pattern 的 `rg` AC。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r08 附加要求（第八次複審）

本輪是 r07 修訂後的複審。r07 結果：Codex `0×P0 + 2×P1 + 5×P2`、Claude `0×P0 + 2×P1 + 12×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r07 行。請逐條核對 r07 findings 是否關閉，並全量複審。重點：

1. 全鏈入口語義是否只剩 `MemoryRecordEnvelopeV1`（+ 顯式 `observe_terminal_observation` 薄封裝）：§0.2/§0.3、ADR-M2-05/07/12、M2-08、M2-13、M2-16C、M2-16D1、故障矩陣、完成判據零殘留舊簽名。
2. `DEP-M2-DSH-01` 非阻塞定位與 REL-M2-01 的「獨立發版成員 vs no-release 參與卡」是否閉環（完成判據、DAG、M2-15 AC、狀態機）。
3. `MemoryRecordEnvelopeV1` / `TerminalObservationFingerprintV1` / `TerminalObservationV1` / `TerminalEvidenceRefV1` 的首建歸屬（M2-07 `evidence.rs`）、M2-07 寫集/AC、M2-01 版本門清單。
4. 機械欄位：M2-05 `prod-files=14`+`L-exception:EX-M2-03`、M2-13 `writeset=serialized-at-M2-16C`、審計表/waiver/Granularity 三者一致；審計表末行斷行；25 卡 `Dependencies` 前空行。
5. r07 的 P2：DAG `M208→M216C`、`GC-M2-14` 觸發收窄、`docs/development/commands/agent.md`、M2-05 cloud 文檔、正向 `rg` VER、plan-status DSH 行。
6. 事實基線錨點、版本面集合、CI job 集合、migration tip 是否仍成立；任何自述與實際不符。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r09 附加要求（第九次複審）

本輪是 r08 修訂後的複審。r08 結果：Codex `0×P0 + 5×P1 + 1×P2`、Claude `0×P0 + 2×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r08 行。請逐條核對 r08 findings 是否關閉，並全量複審。重點：

1. 模板 C 組與計劃發布窗口是否一致：feature branch → PR → base.yml+CodeQL 同 head SHA → `gh pr merge --squash` → 以 merge SHA `gh release create --target`（`--verify-tag` 只在 tag 已存在時使用）。
2. M2-01K config 保護（set/unset/remove-section/rename-section/import 對 `memory.keyed_digest.v1` 拒絕或安全跳過、讀/list 脫敏、回歸）與寫集/AC/VER。
3. M2-05 是否納入 `update-ref`/`symbolic-ref` 的 classifier callsite、穩定拒絕、寫集/AC/VER，以及 11 份開發命令文檔同步。
4. M2-16E 是否以 pinned merge-base 全清單（158 路徑）逐路徑 disposition（`port`/`replace`/`already-main`/`intentionally-drop`）並有覆蓋完整性驗證。
5. M2-16B/M2-12 的 `DEP-M2-SRC-01` 依賴與審計表一致。
6. M2-16C/02F/02R/05 的 `prod-files` 口徑、M2-15 VER=2/12、M2-07 `EX-M2-04`、審計表末行斷行是否一致。
7. 事實基線、版本面、CI job 集合、migration tip 與任何自述 vs 實際不符。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r10 附加要求（第十次複審）

本輪是 r09 修訂後的複審。r09 結果：Codex `0×P0 + 5×P1 + 0×P2`、Claude `0×P0 + 2×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r09 行。請逐條核對 r09 findings 是否關閉，並全量複審。重點：

1. M2-05：`EX-M2-05`（G-03 門族）waiver 8 列格式、判據規範塊、`exception=EX-M2-03,EX-M2-05`、`VER=9/8@EX-M2-05` 三處一致；審計表末行確為獨立段落（`(new)` 標記可渲染/可解析）。
2. M2-16E：pinned merge-base 是否為 `2ebd409045b5114707986b66a7a6b42b5a4a4b7e`、pin 驗證與 158 路徑 disposition 覆蓋 VER 是否可執行（不命中表頭/恆 false）。
3. M2-04：local-only 寫入路徑 AC/VER（不觸發 remote tier `put`、repair marker、`object_index`）。
4. `DEP-M2-CI-03`（release.yml 五 job、`gh release create --target <merge-sha>` 觸發、artifact/CDN/Homebrew 判據、失敗前滾）與 M2-15 依賴/§0.5 引用。
5. ER-06a 後端頁面：M2-01K `config.en.md`、M2-16C `agent.en.md`、M2-05 11 頁、M2-13 新建 `memory.en.md`。
6. 事實基線（含 merge-base `2ebd409…`）、版本面、CI job 集合、migration tip、25 卡 AC/VER 與審計表逐欄一致性。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r11 附加要求（第十一次複審）

本輪是 r10 修訂後的複審。r10 結果：Codex `0×P0 + 3×P1 + 2×P2`、Claude `0×P0 + 3×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r10 行。請逐條核對 r10 findings 是否關閉，並全量複審。重點：

1. ER-06a 閉環：M2-01K（`config.en.md`）、M2-16C（`agent.en.md`）、M2-13（新建 `memory.en.md`）的 AC、Docs impact、Implementation write set 三者一致並含 `cf` 提交證據；M2-01/M2-16B 的 N/A 判定有理由；M2-05 的 11 頁。
2. M2-05：`docs/error-codes.md`/`COMPATIBILITY.md` 寫集、穩定 `LBR-*` 錯誤碼（复用或新增）、Display-pin VER、`VER=10/8@EX-M2-05` 與判據規範/審計表一致。
3. M2-07 `exception=EX-M2-04`、M2-16C `landing=4`、M2-01K `prod-files`/Files=5、M2-01 Files 修正。
4. 審計表末行與 `(new)` 標記確為獨立段落（空白行分隔、可渲染、可解析）。
5. 全計劃 `$SRC_CLONE` 無 `<source clone>` 殘留（歷史行除外）。
6. `Release boundary=independent` 卡默認繼承 `DEP-M2-CI-03` 的字段默認值聲明。
7. 事實基線、版本面、migration tip、25 卡 AC/VER 與審計表逐欄一致。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r12 附加要求（第十二次複審）

本輪是 r11 修訂後的複審。r11 結果：Codex `0×P0 + 2×P1 + 1×P2`、Claude `0×P0 + 4×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r11 行。請逐條核對 r11 findings 是否關閉，並全量複審。重點：

1. `EX-M2-05` waiver（10 道門）與 `AC=24/8@EX-M2-05`、`VER=10/8@EX-M2-05`、判據規範塊、審計表四處一致。
2. M2-05 ER-06a：`Docs and compatibility impact`、AC8、`Implementation write set` 三者具名 33 份 EN/zh/開發文檔與 11 頁後端 `*.en.md`。
3. `LBR-MEMORY-<三位數字>` 碼形（001/002）與 `src/utils/error.rs` owner 是否貫穿各卡寫集、`prod-files`、`compat_error_codes_doc_sync` 語義。
4. `DEP-M2-SRC-01` 的 `SRC_CLONE` 初始化 + pin 校驗 + 全部活動卡命令可用 `$SRC_CLONE` 直接執行（歷史行除外）。
5. 審計表末行與 `(new)` 標記之間確有空行；`M2-05` 的 AC/VER/exception、`M2-16C` landing/prod、`M2-01K`/`M2-04`/`M2-06`/`M2-13` prod-files 與審計表一致。
6. 修訂歷史無重複 r10/r11 行；review log 行無筆誤（`r12` 等）。
7. 事實基線、版本面、migration tip、#456 merge-base `2ebd409…`/158 路徑、25 卡逐欄一致性。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r13 附加要求（第十三次複審）

本輪是 r12 修訂後的複審。r12 結果：Codex `0×P0 + 4×P1 + 3×P2`、Claude `0×P0 + 1×P1 + 10×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r12 行。請逐條核對 r12 findings 是否關閉，並全量複審。重點：

1. 審計表末行 M2-15 與 `**Verification 新增标记：**` 之間確有空行（獨立段落；以 `sed -n` 核對物理行）。
2. `EX-M2-02`：waiver 行、M2-13 判據規範塊、卡內 `AC=10/8@EX-M2-02`、`VER=10/8@EX-M2-02`、審計表五處一致，且 10 道門逐條具名。
3. 寫集是否仍有不可機械核對泛稱（`相关 tests`、`output/error mapping`、`bridge protocol 規範`、`相关 unit/integration tests`）；M2-16C 是否具名 `tests/agent_bridge_memory_test.rs` 與 `tests/agent_bridge_protocol_test.rs`。
4. 模板不再含 Code-era `test-provider`/`code_ui_scenarios` 等不可執行示例；lockfile 文案為「其余兩處權威版本面」。
5. M2-04/M2-06 的 Files 計數與 `prod-files`/審計表一致（9）。
6. `mainline.md` receipt owner 現況描述與計劃 AC（待 M2-01 實施時修正）一致。
7. 事實基線、版本面、migration tip、merge-base `2ebd409…`/158 路徑、25 卡逐欄一致。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r14 附加要求（第十四次複審）

本輪是 r13 修訂後的複審。r13 結果：Codex `0×P0 + 3×P1 + 1×P2`、Claude `0×P0 + 1×P1 + 8×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r13 行。請逐條核對 r13 findings 是否關閉，並全量複審。重點：

1. `mod.rs` 註冊所有權：M2-04/06/07/08/11/12/02R/09/10 的寫集是否都含對應 `mod.rs` 且 `prod-files`/審計表同步。
2. 五個 `EX-*`（EX-M2-01 三卡、EX-M2-02、EX-M2-04、EX-M2-05）的「判據規範」塊是否逐條枚舉完整門族與可複製命令；`AC/VER` 分子、waiver 行、審計表一致（EX-M2-02 十門分項、EX-M2-04 VER=10/8、EX-M2-05 AC=24）。
3. `CompilerIdentityV1`：`model_label` 不構成 allowlist 授權的語義是否貫穿 ADR-M2-06/12、M2-07 AC/VER、M2-16C、M2-13；偽造回歸是否具名。
4. M2-13 `command_test` module（非新 target）表述與 `tests/command/mod.rs` 註冊一致。
5. 事實基線、版本面、migration tip、merge-base `2ebd409…`/158 路徑、25 卡 AC/VER/deps/Granularity/審計表逐欄一致；`Dependencies` 前空行與審計表末行 `(new)` 段落格式仍正確。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r15 附加要求（第十五次複審）

本輪是 r14 修訂後的複審。r14 結果：Codex `0×P0 + 4×P1 + 1×P2`、Claude `0×P0 + 4×P1 + 7×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r14 行。請逐條核對 r14 findings 是否關閉，並全量複審。重點：

1. `job_sql` 首建是否完整移交 M2-08（§0.4、M2-02/M2-08 寫集、`prod-files` 12/12、審計表、依賴）。
2. `EX-M2-02`：`AC=41/8@EX-M2-02`、`VER=11/8@EX-M2-02` 是否在 waiver、判據規範（含 V1b/AC 41 分組）、record AC、Granularity、審計表五處一致。
3. `EX-M2-05` 判據規範是否為 25 條 AC（與 waiver/Granularity/審計表一致）。
4. CLI `memory record` identity：不得由 envelope/`model_label` 推導、只接受 runtime registry 或受認證本地來源、`memory_record_rejects_forged_identity` 具名回歸。
5. 審計表末行 M2-15 與 `**Verification 新增标记：**` 之間的空行（以 `sed -n` 物理核對；該處為歷史易回歸點）。
6. 25 卡 AC/VER/deps/Granularity/`prod-files` 與審計表逐欄一致；`Dependencies` 前空行；事實基線/版本面/migration tip/merge-base `2ebd409…`/158 路徑不變。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r16 附加要求（第十六次複審）

本輪是 r15 修訂後的複審。r15 結果：Codex `0×P0 + 6×P1 + 3×P2`、Claude `0×P0 + 4×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r15 行。請逐條核對 r15 findings 是否關閉，並全量複審。重點：

1. **終態觀測持久化**：`memory_terminal_observation`（M2-02 schema、M2-08 依 generation 重放）是否足以覆蓋 observe→compile 崩潰、CLI stdin、bridge、status/evidence/關係變化後重啟；retention/keyed digest 是否有界並有具名回歸。
2. **identity 通道**：`MemoryRecordEnvelopeV1.compiler_identity` 只由認證 ingress/registry 填充、請求體自填拒絕、CLI session 綁定與生命週期；有效 bridge/CLI proposal、跨 session 替換、偽造 `model_label` 三類回歸。
3. **bridge minor 協商**：舊 1.0 peer 僅 20-method、新 peer 22-method；`protocol.rs` 寫集、`allowlist_has_exactly_20_methods`/`protocol_v1_allowlist_is_exactly_20_methods` 更新、舊/新 fixture 與 exact allowlist 斷言；`DEP-M2-DSH-01` 非阻塞不免除 in-repo 門。
4. **`job_sql` 交接**：M2-02 AC/VER/Files 是否只剩 schema 範圍、M2-08 擁有 raw-SQL module 與重放，`prod-files`/審計表一致。
5. **M2-05**：`src/internal/mod.rs`（`pub mod ref_policy;`）是否在寫集/Files/prod/AC/VER/EX-M2-03 依據中；**M2-01K/M2-13** 的 `command_test` 源檔（`tests/command/config_test.rs`、`tests/command/memory_test.rs`）是否入寫集。
6. 新增 `EX-M2-06`（8 列格式、M2-16C VER=9/8、判據規範塊）與審計表/卡內一致；25 卡逐欄一致；`(new)` 段落空行與 `Dependencies` 空行；review log 輪次/SHA 無筆誤。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r17 附加要求（第十七次複審）

本輪是 r16 修訂後的複審。r16 結果：Codex `0×P0 + 6×P1 + 1×P2`、Claude `0×P0 + 4×P1 + 5×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r16 行。請逐條核對 r16 findings 是否關閉，並全量複審。重點：

1. `memory_terminal_observation` 的欄位合同（脫敏 envelope、HMAC principal、`proposal` 僅 digest/引用）、retention（N=8/TTL 30 天/≤10,000 行、原子 prune、未處理不可刪、digest 驗證）與 access owner（M2-08 `job_sql.rs`）是否在表職責、M2-02 AC/VER/Security、M2-08 AC 四處一致；`terminal_observation_snapshot_retention_and_redaction` 具名回歸。
2. M2-05 判據規範首行（10 VER + 25 AC）與 M2-13（11 VER + 41 AC）是否與 waiver/Granularity/AC/審計表一致。
3. `EX-M2-06` 判據規範是否逐條枚舉 M2-16C 的 9 條 VER 門（B0–B11 對應），且 `agent_bridge_protocol_test` 整 target 會執行兩道 exact-allowlist 守衛；區塊前後空行、`**Dependencies:**` 欄位完好。
4. spec 具名命令與實際 VER 名稱一致（`observer_job_schema_migration_transaction`、`memory_record_rejects_forged_identity_and_binds_session_identity`）。
5. M2-13 Files=5/prod=5、M2-12 prod=8 與寫集一致；25 卡逐欄一致。
6. 審計表末行 `(new)` 段落空行、`Dependencies` 空行、review log 輪次/SHA 無筆誤、修訂歷史無重複行。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r18 附加要求（第十八次複審；本輪評審期間凍結計劃檔）

本輪是 r17 修訂後的複審。r17 結果：Codex `0×P0 + 2×P1 + 2×P2`、Claude `0×P0 + 3×P1 + 8×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r17 行。**被審版本 SHA-256 見 log 檔頭；本輪評審期間計劃檔不再變更。** 請逐條核對 r17 findings 是否關閉，並全量複審。重點：

1. **CLI identity**：`--compiler-identity <handle>` 是否由 M2-08 `runtime.rs` 的 `register_compiler_identity` 簽發、M2-07 `trust.rs` 定義契約、`memory_compiler_identity` 表 schema 由 M2-02 定義（含 `principal_hmac/repository_id/worktree/issued_at/expires_at/revoked_at`）；有效/失效/撤銷/跨 session/偽造五類 E2E；**不得再依賴不存在的 `agent session` 簽發路徑**。
2. `EX-M2-06` 判據塊：首行「9 條 VER 門」、B 列表與 VER 一一對應、每門可複製執行、區塊位於 VER 清單之後；`EX-M2-07`（`AC=31/8@EX-M2-07`）8 列格式與審計表一致。
3. retention/prune 是否全在 M2-08（`job_crash_recovery_and_observation_retention_matrix`）；M2-02 `VER=8/8` 且判據塊只列 schema/redaction 合同。
4. M2-12 `Files=8`/prod=8、M2-16C `Files`/prod、25 卡逐欄一致；`(new)` 段落空行與 `Dependencies` 空行。
5. review log 輪次/SHA 無筆誤；修訂歷史無同輪次重複行；plan-status 與計畫同步。
6. 事實基線、版本面、CI job 集合、migration tip、merge-base `2ebd409…`/158 路徑不變。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r19 附加要求（第十九次複審；本輪評審期間凍結計劃檔）

本輪是 r18 修訂後的複審。r18 結果（同一凍結版 sha256 `50bb0b0d…`）：Codex `0×P0 + 4×P1 + 2×P2`、Claude `0×P0 + 6×P1 + 9×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r18 行。**被審版本 SHA-256 見 log 檔頭；本輪期間計劃檔不再變更。** 請逐條核對 r18 findings 是否關閉，並全量複審。重點：

1. **M2-13 identity**：是否只剩 `--compiler-identity <opaque-handle>`（M2-08 `runtime.rs::register_compiler_identity` 簽發、`memory_compiler_identity` 表、M2-07 `trust.rs` 契約）；`agent_session` 表述清零；五態矩陣 VER（有效/過期/撤銷/跨 handle/envelope 自填拒絕）具名。
2. **M2-02**：審計表 VER=`8/8` 與卡內一致；判據規範首行 10 條 AC 並具名 `memory_compiler_identity` 欄位。
3. **`EX-M2-06`**：判據塊恰 9 門、與 9 個 VER checkbox 一一對應、每門可複製執行、位於 VER 清單後；`protocol_v1_allowlist_is_exactly_22_methods` 命名一致且被整 target VER 執行。
4. **`EX-M2-07`/`EX-M2-08`**：AC 謂詞分組（31/13）與 waiver/Granularity/審計表一致；M2-08 `resolve` 生命週期、admission/backpressure、矩陣測試五態覆蓋。
5. M2-16D/E `prod-files=0`、M2-16B `serialized-at-M2-07`、M2-16E `rg -c || echo 0`、DEP-M2-SRC-01 無 `gh api` fallback。
6. 審計表末行 `(new)` 段落空行、`Dependencies` 空行、review log 輪次/SHA/表格欄數、修訂歷史無同輪次重複行；25 卡逐欄一致；事實基線/版本面/migration tip/merge-base `2ebd409…`/158 路徑不變。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r20 附加要求（第二十次複審；本輪評審期間凍結計劃檔）

本輪是 r19 修訂後的複審。r19 結果（同一凍結版 sha256 `d49d53fa…`）：Codex `0×P0 + 2×P1 + 2×P2`、Claude `0×P0 + 3×P1 + 7×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r19 行。**被審版本 SHA-256 見 log 檔頭；本輪期間計劃檔不再變更。** 請逐條核對 r19 findings 是否關閉，並全量複審。重點：

1. **CLI identity**：`--compiler-identity` 是否只在 envelope 帶 `proposal` 時必填（observation-only 免 identity，job pending）；五態矩陣是否含「observation-only 無 identity 可寫」；是否已無不可獲得的 handle 依賴。
2. **proposal carrier**：是否定義 `proposal_canonical_json`（M2-07 正規化/脫敏、與 snapshot 同事務、keyed digest 綁定、重放前重驗、失效 fail-closed）；CLI 與 bridge 的 observe→crash→restart→Write Episode 回歸具名。
3. **`EX-M2-08` 撤回**：M2-08 是否為 `AC=8/8; exception=N/A`，審計表同步，13 謂詞降為說明文本。
4. 審計表三格（M2-16B `serialized-at-M2-07`、M2-16D `1/0`、M2-16E `2/0`）與卡內一致；`EX-M2-06` 塊是否位於全部 9 個 VER 之後且一一對應；`EX-M2-07` 分組是否綁定 checkbox。
5. M2-02 判據首行 9、`mainline.md` 全文修正、review log/修訂歷史無筆誤或同輪次重複行；25 卡逐欄一致；事實基線/版本面/migration tip/merge-base `2ebd409…`/158 路徑不變。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。

## R30-r21 附加要求（第二十一次複審；本輪評審期間凍結計劃檔）

本輪是 r20 修訂後的複審。r20 結果（同一凍結版 sha256 `a4e0952b…`）：Codex `0×P0 + 2×P1 + 2×P2`、Claude `0×P0 + 2×P1 + 9×P2`；修訂決策見計畫「修訂歷史」2026-09-21 r20 行。**被審版本 SHA-256 見 log 檔頭；本輪期間計劃檔不再變更。** 請逐條核對 r20 findings 是否關閉，並全量複審。重點：

1. **模組可見性（Claude P1-1）**：`memory`/`keyed_digest` 是否與倉庫其餘子模組一致的 `#[doc(hidden)] pub mod`、`EpisodeCompilerModel` 是否 `pub trait`、deterministic fake 是否位於 `#[doc(hidden)] pub mod memory::testing`（非 `#[cfg(test)]`）；`GC-M2-15` 是否覆盖；70 條 `--test memory_episode_test` 門是否可編譯；Rust 語義互斥是否解除。
2. **M2-08（Codex P1-1）**：`EX-M2-09` 行是否為 8 列、`AC=34/8@EX-M2-09` 分子如實、判據塊是否逐條枚舉 34 條謂詞（AC1–AC8）。
3. **proposal fingerprint（Codex P1-2）**：proposal canonical digest 與 identity 版本是否納入 fingerprint；附加/替換 proposal 是否以新 generation 表達；observation-only→proposal、proposal 變化、同 proposal 重放、崩潰窗口是否具名回歸。
4. 審計表空行是否物理存在（`cat -A` 或位元組核對）；`EX-M2-06` 塊是否在全部 9 條 VER 之後；`EX-M2-07` 分組編號是否為 AC4/AC5；review log 是否無 `r25` 類筆誤；修訂歷史是否含 r16 行；M2-02 A1–A9 與 identity 六列斷言；六態 observation-only 回歸；AC 計數口徑聲明；M2-05 `prod-files` 條件分支（18）。
5. 25 卡逐欄一致；事實基線/版本面/migration tip/merge-base `2ebd409…`/158 路徑不變。

輸出規則同前：P0/P1/P2、位置/問題/為何重要/最小修復；最後一行恰為 `VERDICT: PASS` 或 `VERDICT: FAIL`。
