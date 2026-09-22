# R30 評審修訂清單（Claude r01 結果；待 Codex r01 合併）

Claude: P0=4 / P1=13 / P2=9 → FAIL。逐條決策如下（Codex 結果到達後合批套用）：

## P0
1. **M2-08 整卡重寫**：Description/evidence/AC/write set 全部改為 R30 seam（`TerminalObservationV1` + `observe_terminal` + bridge record；CPU 只保留 Memory ref→父 Intent 依賴喚醒）；刪 `orchestrator/persistence.rs`、`ai/runtime/`；evidence 改引 `agent_bridge/ingress.rs:77-81`、`export_job.rs`。
2. **M2-09/10 去 `test-provider`**：刪所有 `--features test-provider` / `LIBRA_ENABLE_TEST_PROVIDER`；指名 M2-16B 的 `#[cfg(test)]` in-memory fake。
3. **Episode root 事實面**：§0.1/§0.2 增列「Task/IntentSpec 物件在 main 無 in-repo producer」；`TerminalObservationV1.root_id` 定義為 host 提供 opaque 標識（僅 code anchor + source ref 必須可解析）；改 GC-M2-02、M2-07 AC#3、M2-10；新增 `GAP-M2-15` 與風險行。
4. **terminal_status 佐證**：M2-07 新增 AC（status 必須與 `source_ref_oid` 可解析記錄一致，否則 Quarantined + reason code）；M2-16C AC#3 同步；刪「機械映射」措辭；故障矩陣/M2-09 AC#5 同步。

## P1
1. **版本面**：全計劃改「版本面三處（`Cargo.toml`/`install.sh`/`install.ps1`；集合以 `compat_version_surface_sync` 為權威）+ `Cargo.lock` + artifact」。
2. **Full-suite trigger 賦值**（ER-13；`none`/`T-n`/`N/A`）：M2-16A=`T-1: sql/**`+`T-3`（若遷移留在本卡）、M2-16B=`none`、M2-16C=`none`、M2-16D/E=`N/A`、M2-01=`none`、M2-01K=`T-1: docs/error-codes.md`、M2-02/02R=`T-1+T-3`、M2-02F=`T-1: workflows+Cargo.toml`+`T-3`、M2-03=`none`、M2-04=`T-1: error codes`、M2-05=`none`、M2-06=`T-1: error codes`、M2-07..12=`none`、M2-13=`T-1: cli+error codes`、M2-14=`none`、M2-14C=`T-1: workflows`、M2-15=`T-2: release`。
3. **M2-15 Verification**：`cargo test --all`→ER-14 nextest；刪 `LIBRA_SKIP_WEB_BUILD=1`（M2-15、測試矩陣 FTS 行）。
4. **M2-15 Release write set**：`N/A`→`Inherited`（版本面三處 + lock + artifact）。
5. **M2-01 evidence**：#456 歷史證據化，刪 passed 數字。
6. **M2-02**：`2026081301`→`2026091901`；write set 遷移名→`2026092101_memory_core{,_down}.sql`。
7. **M2-16A 規模（r02 更新）**：採行「縮範圍」替代方案，不登記 G-04 EX：M2-16A 只移植 I/O-free 領域層（6 檔，prod-files=7 ≤ M），其餘 #456 檔案按下游行為軸分派；rollback 因此由 `forward-only` 改為 `revert`（無 schema/對象交付）。決策記錄見 R30-r02 修訂歷史行。
8. **依賴一致性**：M2-09 卡內 deps/granularity 補 `M2-16B`；M2-13 三處補 `M2-16C`；M2-16A 三處 rollback 統一 `forward-only`。
9. **GC-13/ER-13/14 條款**：§約束改 `GC-01..GC-13`、`ER-01..ER-14`；三張 migration 卡加 DB role=Repository-only AC + `LIBRA_CONFIG_GLOBAL_DB`/`LIBRA_CONFIG_SYSTEM_DB` 隔離 VER。
10. **M2-16D 拆分**：M2-16D 純 audit（只產決策記錄，no code）+ 新 M2-16D1（條件性 implementation，`independent`/`patch`）；DAG/審計/追溯/風險/卡數（25）同步。
11. **M2-16C VER**：`<bridge-target>`→新建 `tests/agent_bridge_memory_test.rs`（(new)+INDEX 登記）。
12. **M2-13**：加 `MEMORY_EXAMPLES` 與 3 條 compat VER；ER-06a 後端網站文檔判定。
13. **plan-status.md**：同步 R30（M2-16A..E,D1；M2-01 pending；起點 M2-16A）。

## P2
1. op prune 錨點→`src/command/op.rs:1144`。
2. `history.rs` 錨點→`:1026`、`:2227`。
3. M2-11 write set `libra_vcs.rs`→實際 ancestry helper（依 #456 applicability.rs 實際 import 定）。
4. CodeQL 錨點→`codeql.yml:21`（analyze）與 `:44-46`（matrix）。
5. `(new)` 標記逐條區分；M2-16D 的 `internal::ai::agent_run` 為既有。
6. `.env.test`/`.env.live-test`：加「乾淨 worktree 先 `cp .env.test.example .env.test`；`.env.live-test` 由 L3 環境或主 checkout 提供」設定註記，保留模板 ER-14 字面命令。
7. 自審表「18 卡 + 1 發布點」加 R30 覆寫說明。
8. GAP 表補 `GAP-M2-15`（root 物件無 producer）、`GAP-M2-16`（host 未接 record）。
9. M2-01 加 AC：`memory.md` §4.2.1 的 `memory_entity_index`/`memory_taxonomy_node` 引用同步刪除或標為後續切片（首切不建這兩張表）。

## 自行發現
- §0.4「`MemoryAnchorConfidence`→`EpisodeTrustLevel`」錯：改 `MemoryConfidence`（Low/Medium/High），與既有 `MemoryTrust` 分離（#456 實證）。
- M2-16A write set 補：`linear_ref.rs`、`completion/progress.rs`、`agent_bridge/memory.rs`、`.config/nextest.toml`、`tests/fts5_capability_test.rs`、`tests/helpers/memory_cli.rs`、`tests/fixtures/memory/**`。
