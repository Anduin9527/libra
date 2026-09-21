# M2-16A 移植清單（#456 → R30，唯讀盤點，2026-09-21）

> **歷史文件（r01 收口依據）。** 現行權威為計畫 §0.4 owner 映射、ADR-M2-15 與 `GC-M2-15`；如本文與計畫衝突，以計畫為準。

來源：`/run/media/genedna/data/libra-pr456` 分支 `codex/memory-m2-core-draft-pr`（`bc1be587e4`）；merge-base = `git merge-base 45ad1f7f2 bc1be587e4` = `2ebd409045b5114707986b66a7a6b42b5a4a4b7e`。

## 1. 直接搬移的 memory 模組（34 檔，~28k LOC）

`src/internal/ai/memory/`：`admission.rs` `applicability.rs` `canonical.rs` `compiler/{intent,mod,schema,task}.rs` `delivery.rs` `diagnostics.rs` `domain.rs` `dsh.rs` `error.rs` `evidence.rs` `fts_sql.rs` `job.rs` `job_sql.rs` `job_state.rs` `limits.rs` `mod.rs` `observer.rs` `policy.rs` `projection.rs` `query.rs` `reader.rs` `replay.rs` `runner.rs` `runtime.rs` `selector.rs` `source.rs` `store.rs` `tree.rs` `validation.rs` `view.rs` `writer.rs`

另需搬移/新落點：
- `src/internal/ai/context_budget/{memory.rs,receipt.rs,receipt_store.rs}` → `src/internal/ai/memory/context/`（ADR-M2-10）
- `src/internal/ai/keyed_digest.rs` → 由 M2-01K 承接（同內容但路徑不變，main 尚無此檔）
- `src/internal/ai/linear_ref.rs` → 由 M2-03 承接
- `src/internal/ai/completion/progress.rs` → 由 M2-16B 決定是否保留（模型 seam 相關）
- `src/internal/ai/agent_bridge/memory.rs` → 由 M2-16C 承接

## 2. 被刪模組依賴的精確對映（M2-16A/16B 必改）

| #456 依賴 | 出現位置（節錄） | R30 替代 |
|---|---|---|
| `MemoryAnchorConfidence::{Low,Medium,High}` | `admission.rs:25,239`、`canonical.rs:8,38,138,310`、`domain.rs:6,71,284,308`、`compiler/schema.rs:6,20`、`replay.rs:338,370`、`source.rs:774,1524,1578,2021`、`validation.rs:738,...`、`writer.rs:567,...` | **`memory/domain.rs` 新 `MemoryConfidence`（Low/Medium/High）**。注意：這是 confidence，不是 trust；`MemoryTrust`（Confirmed/Quarantined/Rejected）在 `memory/mod.rs` 已存在，**不要把兩者合併**（R30 §0.4 原寫 `EpisodeTrustLevel` 是錯的，待改） |
| `providers::AnyCompletionModel` | `compiler/{task,intent}.rs:17,35-37` | `EpisodeCompilerModel`（ADR-M2-12） |
| `providers::fake::Client`（測試） | `compiler/{task,intent}.rs:623-628,441-446` | `#[doc(hidden)] pub mod memory::testing` in-memory fake（`GC-M2-15`） |
| `client::CompletionClient`、`providers::deepseek` | `dsh.rs:30-31` | host 注入 model + 移除 provider 選擇；DSH adapter 只接受 `EpisodeCompilerModel` |
| `context_budget::ContextBudget` | `delivery.rs:12,265`、`runtime.rs:42,77,90,106,115,146,162,173,358` | `MemorySegmentBudgetV1`（M2 `memory/context/`） |
| `context_budget::MemoryContextAssemblerErrorKind` | `reader.rs:995,1055` | `memory/context/` 自有 error enum |
| `context_budget` receipt 型別 | `delivery.rs:12`、`reader.rs:580`、`context_budget/{memory,receipt,receipt_store}.rs` | 搬移後路徑 `memory/context/**`，envelope 不變 |

## 3. 整合點（#456 對 main 其他檔的修改）

- CLI/命令：`src/cli.rs`、`src/command/memory.rs`（新）、`src/command/mod.rs`、`src/command/{branch,clone,config,fetch,maintenance,op,push,symbolic_ref,update_ref}.rs`
- AI 內部：`src/internal/ai/{mod.rs,history.rs}`、`completion/mod.rs`、`agent_bridge/{authorization,ingress,mod,protocol,transport,workspace}.rs`
- 遷移（**必須重編**）：`2026090701_memory_core{,_down}.sql` → `2026092101_memory_core{,_down}.sql`；`2026090702_memory_fts_search{,_down}.sql` → `2026092102_*`；`2026090703_context_selection_receipt{,_down}.sql` → `2026092103_*`；同步 `sql/migrations/README.md` 與 `src/internal/db/migration.rs`
- CI：`.github/workflows/memory-portability.yml`（新）、`.config/nextest.toml`
- 測試：`tests/memory_episode_test.rs`（新）、`tests/fts5_capability_test.rs`（新）、`tests/command/memory_test.rs`（新）、`tests/helpers/memory_cli.rs`（新）、`tests/fixtures/memory/**`、`tests/db_migration_test.rs`、`tests/command_test.rs`、`tests/command/mod.rs`、bridge 測試 4 檔、`tests/compat/{agent_bridge_schema_test,serial_registry}.rs`、`tests/{operation_schema_v2,operation_wrapper_test}.rs`
- 文件：`docs/commands/{memory.md,zh-CN/memory.md}`（新）、`docs/development/commands/memory.md`（新）、`docs/development/tracing/memory.md`、`docs/development/gap/mainline.md`、`docs/error-codes.md`、`COMPATIBILITY.md`、`docs/commands/**` 多檔

## 4. 需在 R30 計畫中修正的點（review 回合候選）

1. §0.4「`MemoryAnchorConfidence` → `EpisodeTrustLevel`」→ 改為 `MemoryConfidence`（Low/Medium/High），與既有 `MemoryTrust` 分離。
2. 所有權以 §0.4 為準：`src/internal/ai/linear_ref.rs` → M2-03；`src/internal/ai/agent_bridge/memory.rs` → M2-16C；`src/internal/ai/completion/progress.rs` → M2-16B 決定（現計畫不移植）；M2-16A 僅移植 I/O-free 領域層。測試清單（fts5_capability_test、memory_cli helper、fixtures）、`.config/nextest.toml`。
3. `tests/operation_wrapper_test.rs` 在 #456 被修改；R30 該檔已刪，移植時必須丟棄該檔改動（M2-05 已寫入「刪除」）。
4. `Cargo.toml` 無 `[[test]]` 變動；測試為自動探索，計畫不需新增 `[[test]]` 條目（M2-02F/M2-16A 的 `Cargo.toml` 寫集可縮小）。
