# R30-r03 修訂清單（Claude r03；待 Codex r03 合併）

Claude r03：P0=1 / P1=10 / P2=13 → FAIL。已關閉：r02 的觀測模型/關係/補錄/observer/觸發/PR 流程大部分。

## P0
1. **bridge 無可用 source_plane**：`TerminalEvidenceRefV1.source_plane` 增 `bridge_event`（locator = `(bridge_session_id, event_seq)`；digest = `agent_bridge_event.payload_sha256`；可解析 = 行存在且 digest 匹配）；同步 ADR-M2-05、GC-M2-02、M2-07 AC、M2-16C AC3、故障矩陣。

## P1
1. **EX-M2-01 未按模板登記（r04 更新）**：原決議「合回一條 AC、撤銷 EX」被 Codex r03 P1-3 的建議取代——改為按模板 8 列規格新增 waiver 表 + 三卡「判據規範（非計數正文）」塊，保留 `AC=9/8@EX-M2-01`。
2. **M2-16A/16C `exception=EX-M2-01`** → `N/A`。
3. **model registry 無承接卡**：M2-08 AC5 加「`MemoryRuntime::with_compiler_model` 註冊；未註冊且 record 未帶 proposal 時 job pending + 穩定 reason code」；VER 加 `job_pending_without_compiler_model`（VER 7/8）；M2-16C AC3 改「複用 M2-08 語義」。
4. **DAG 缺 `M216D --> M216E`**：補邊。
5. **審計表 M2-08 deps** 加 `M2-16B`。
6. **bridge `source` CHECK 固定 `deepseek-harness`**：ADR-M2-05 host 措辭改「bridge host 當前僅 `deepseek-harness`；其餘 host 走嵌入 API 或 `libra memory record`」；M2-16C Out of scope 寫「放寬 source 需獨立遷移卡」。
7. **VER 計數**：M2-02/02F/02R 實為 8 條 → `8/8`（合回 AC 後）；M2-05 的 4 個 `test -f` 合併為單一 for-loop 門 → `8/8`。
8. **`sqlite_fts5_release_capability` 雙 target**：唯一歸屬 `tests/fts5_capability_test.rs`；M2-02F VER 改 `cargo test --release --test fts5_capability_test sqlite_fts5_release_capability`（與既有 release 行合併）。
9. **M2-09/10 7 處 `-- --test-threads=1`** 刪除（無 env/cwd 互斥需求）。
10. **事實基線 4 處錨點**：`ai/mod.rs:53-160→53-68`、`client_storage.rs:1645-1742→2023-2100`、`base.yml:14-532→11-422`、`config.rs:1381-1504→44-74,218-290`。

## P2
1. §0.4 首建/擴展標注：`compiler/mod.rs`（M2-07 介面 / M2-09/10 adapter）、`policy.rs`（M2-04 首建 / M2-07 擴展）、`limits.rs`（M2-16A 首建 / M2-07 擴展）；`delivery.rs` 加入 M2-12 寫集。
2. §「適用範圍」`memory` 命令清單補 `record`。
3. `(new)` 例外句並列 `compat_serial_registry`。
4. 模板標籤漂移：計畫 `:7`「遷移政策（v2.7）」→ v2.8；`plan-status.md:128` 首句 v2.7→v2.8。
5. ContextFrame 措辭：改「libra 側 `context_budget/*` 落點已刪；git-internal 的 `ContextFrame` 物件類型仍在，R30 不使用」。
6. M2-01 AC `memory.md` 範圍改「全文刪除或標注為後續切片」。
7. REL-M2-01/發布窗口順序同步 PR 流程（回指 §0.5/DEP-M2-CI-02）。
8. DEP-M2-CI-01 補「M2-14C 落地後判據擴為三步同 matrix SHA」；M2-02F AC8 補 nextest pinned 安裝判據。
9. M2-08/M2-12 改 `agent_bridge/protocol.rs` 的 trigger 統一為 `T-1`（或具名說明）。
10. `scripts/`/`docs/benchmarks/` 布局：M2-14C 寫集補 `AGENTS.md`；`docs/benchmarks/` 描述改「尚不存在」。
11. `GC-M2-14` 與 `GC-M2-13` 順序對調。
12. M2-13 `record` 補獨立判據（併入 `rebuild` AC 或錯誤碼 AC）。
13. 模板 v2.8 自身：`plan-template.md:328`、`:702` 補 `source .env.live-test &&`；`web/`/`worker/` 行加「0.23.x 已拆除，僅供歷史計劃」注記。
