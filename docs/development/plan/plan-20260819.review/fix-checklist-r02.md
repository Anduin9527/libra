# R30-r02 修訂清單（Claude r02；待 Codex r02 合併）

Claude r02 結論：P0=1 / P1=7 / P2=12 → FAIL。已實證關閉：版本面權威、遷移所有權、M2-16D/D1、M2-13 三道守衛、DEP-M2-ENV-01、plan-status §3.3。

## P0
1. **關係欄位無事實源**：`TerminalObservationV1` 加受信任可選 `parent_root_id: Option<OpaqueRootId>` 與有界 `related_run_ids`；缺失 → Task `related_intent_ids` 空、M2-08 不喚醒父 Intent、M2-10 對該 Intent 不產 Confirmed（fail-closed）。同步 ADR-M2-07 指紋措辭、M2-08 AC2、M2-09 AC2/5、M2-10 證據與 AC1、GAP-M2-15。

## P1
1. **M2-16B 倒序**：縮為 `memory/model.rs`（trait/request/response/error + `#[cfg(test)]` fake）+ `memory/dsh.rs`；`Dependencies` 加 M2-07；DAG 改 `M207 --> M216B --> M209`；§0.4 所有權把 `compiler/**` 留給 M2-09/10，`dsh.rs` 歸 M2-16B；「compiler 只經 trait」判據移交 M2-09/10；VER 去掉不存在 target/補 `tests/INDEX.md`。
2. **`libra/intent` 殘留**：`memory_compile_observer_state` 表職責與 M2-02 AC4 改為「首切只使用 `libra/memory/repo` 一行；`source_ref_name` 保持通用列」；VER `observer_job_schema_transaction` 加「不產生 `libra/intent` 行」斷言。
3. **`terminal_status` 五態**：收斂為 `{completed, failed, cancelled, partial}`（刪 `unchanged`）；`completion_status` 擴為四值；同步 M2-09 AC7、故障矩陣、benchmark corpus。
4. **nextest 基建零登記**：M2-02/02F/02R（env fixture）與 M2-05/M2-13（cwd）寫集加 `tests/SERIAL_REGISTRY.tsv`、`.config/nextest.toml`，加 AC（registry 行 + `sh tests/NEXTEST_GROUPS.sh` 無 diff）與 VER `cargo test --test compat_serial_registry`，trigger 加 `T-4`；M2-09/10 VER 刪 `-- --test-threads=1`。
5. **M2-16C 錯誤碼**：寫集加 `docs/error-codes.md`；trigger 改 `T-1: 新增 bridge 穩定錯誤碼 + docs/error-codes.md`。
6. **M2-03/05/07 trigger**：改 `T-1: GC-02 共享 helper（HistoryManager CAS / ref_policy / observed_agents redaction）`。
7. **ER-06a**：`docs` 預設補「後端網站逐卡判定或 N/A 證據」；M2-05 加 docs AC + VER；M2-16C AC6「如涉及」改明確 N/A 理由。

## P2
1. M2-02/02F/02R VER 計數 6→7（Granularity + 審計表）。
2. §0.2 `agent_bridge/{...,memory,...}` 移除 `memory`。
3. §0.4 所有權首建修正：`fts_sql → M2-02F 首建/M2-11 擴展`、`job_sql → M2-02 首建/M2-08 擴展`。
4. Code-era 觸發措辭（§line 85/101/103、M2-09 AC7）改 `TerminalObservationV1`/`terminal_status`。
5. `propose_episode` 簽名統一引 ADR-M2-12；補 object-safety（`#[async_trait]` 或 `-> impl Future`）註記。
6. 事實基線抬頭改 R30 核對日 / `45ad1f7f2` / `0.23.36`。
7. `(new)` 標記為 M2-13 三道既有守衛加例外說明。
8. 模板版本漂移：計畫標頭「遷移政策（v2.7）」→ v2.8；`plan-status.md` v2.7→v2.8 與 r01 雙 FAIL 記綠。
9. M2-11 寫集去重 `applicability.rs`；`CodeHistory` 改「新建 memory/applicability.rs 內部 ancestry walker」。
10. M2-16E `Dependencies` 顯式寫 `M2-16D1（條件性）`。
11. M2-02F I 寫集移除 `Cargo.lock`。
12. 三張遷移卡 GC-13 判據拆成獨立 AC（9/8@EX-…）並在 waiver 表登記門族型例外；Granularity/審計行同步。
