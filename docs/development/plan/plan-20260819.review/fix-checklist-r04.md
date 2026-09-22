# R30-r04 修訂清單（Claude r04；待 Codex r04 合併）

Claude r04：P0=0 / P1=2 / P2=13。r03 全部關閉除：`related_run_ids`（部分）、Phase0/里程碑 M0、`<libra-pr507>` 佔位、REL 措辭、模板 `.env.live-test`/縮排、plan-status 過時。

## P1
1. **`related_run_ids` 指紋無 AC/VER**：M2-08 AC3 加 canonicalized `related_run_ids`；把 `observe_terminal_rejects_mismatched_status` 合併為 `observe_terminal_rejects_mismatched_status_and_distinct_related_runs`（單一具名門，避免 VER 超限）；測試矩陣 Job/recovery 行補此用例。
2. **里程碑 M0 未同步**：M0 改「M2-16A + M2-01 + M2-01K reviewer PASS；schema golden 與 keyed-digest 跨重啟 fixture 固定」；M2-16B 移入 M2 里程碑完成條件。

## P2
1. M2-13 AC「CLI Adapter 只調用 …」清單補 `MemoryRuntime::observe_terminal`（record seam）。
2. 測試矩陣 CLI filter 補 `_record`。
3. `memory record` mismatch 具名回歸：E2E filter 改 `memory_search_show_status_rebuild_record_with_mismatch`（維持 VER 8 條）。
4. M2-16A evidence `git -C <libra-pr507>` → 固定路徑可重跑命令（`/run/media/genedna/data/libra-pr456` 共享 clone 上 `git grep … bc1be587e4`）。
5. ADR-M2-05 括注：「bridge 路徑唯一可用持久平面；嵌入式/補錄用 `session_jsonl`/`review_store`」。
6. EX-M2-01：Approver 具名 + 證據改引 `codex-plan-r03.log` P1-3；`fix-checklist-r03.md` 標注「撤銷方案被 Codex waiver 建議取代」。
7. M2-16D1 重複前綴 `- [ ] - [ ]` 修正。
8. §0.4 與寫集對齊：M2-07 寫集補 `policy.rs`；M2-04 寫集補 `admission.rs`；M2-06 寫集補 `diagnostics.rs`。
9. 模板 `:338` ER-13 收口門補 `.env.live-test`。
10. 模板 `:352` 恢復 4 空格縮排（防渲染代碼塊）。
11. plan-status `:195` 同步 r03/r04；`:256` DEFER 改 `DEFER-M2-01..09`。
12. `EpisodeProposalV1` 歸屬：由 M2-07 `compiler/mod.rs` 首建；M2-16B 只定義 trait/request/error/fake 並 import 該型（ADR-M2-12/§0.4 同步）。
13. REL-M2-01 窗口列措辭改回指 §0.5 PR 流程。
