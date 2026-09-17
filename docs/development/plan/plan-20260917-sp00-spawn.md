# SP-00: `command_test` 高并行 spawn 签名

> plan-20260917 附件。2026-09-17 本机复现。不改生产、不改 nextest 成员。

## 复现命令

本机没有 `.env.test`（只有 `.env.test.example`）。`command_test` 是 L1，缺 L2/L3 变量会 skip 而不是红。实际命令：

```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test -- --test-threads=32
```

计划写的 `source .env.test && …` 因文件不存在未执行；与 L1 失败签名无关。

| 项 | 值 |
|---|---|
| 主机 | `nproc=32`，`loadavg` 开工时 `0.49 0.81 0.99` |
| 墙钟 | 2026-09-17T21:29:43+08:00 → 21:39:27+08:00 |
| 测试墙钟 | `finished in 568.61s` |
| cargo 退出 | `101`（测试失败，**不是** 整二进制 SIGKILL） |
| 结果 | **3454 passed; 14 failed** |
| 日志 | `~/libra-test-scratch/plan-20260917/sp00_command_test.log`（不入库） |

## 失败列表

13 条 `merge_test` + 1 条 `notes_test`。全部炸在 `tests/command/mod.rs:176` `assert_cli_success`（只拼 stderr）。

| 分类 | 条数 | 代表 |
|---|---|---|
| stderr 为空（上下文像 `commit file: ` / `theirs: `） | 11 | `merge_rename_conflict_summaries_match_across_both_walks`、`notes_test::boundary_json_list_multiple_entries` |
| stderr 非空，`LBR-NET-002` / exit 128 | 3 | 见下一节 |

没有出现「读到宿主 `.libra` / `config_kv` 缺失 / 错二进制」这类隔离泄漏文案。`base_libra_command` 仍是 `env_clear` + 每测独立 HOME / 全局·系统 config.db。

历史对照（同机、同分组终态 `9a24b06`）：`cargotest.log:7757` 整进程 `signal: 9, SIGKILL`；隔离重跑 merge 五条绿。本轮没有再 SIGKILL 测试二进制，但空 stderr 签名复现，并且第一次抓到非空失败的真实协议错。

## ExitStatus（至少一次完整）

`assert_cli_success` 丢掉 `status` / stdout，所以空 stderr 那 11 条只能知道 `success()==false`，**code 与 signal 未知**。这本身是 SP-01 诊断缺口。

三条非空失败给出完整子进程报告（stderr 里的 JSON envelope）：

```
ok=false
error_code=LBR-NET-002
category=network
exit_code=128
severity=fatal
message=operation storage failed: working-copy I/O worker failed:
        worktree I/O protocol error: status io worker frame ended before its payload
```

代表用例 `merge_gitlink_cleanliness_gate_ignores_a_submodule_but_not_a_plain_file`，断言上下文 `stage the base gitlink:`：

| 字段 | 值 |
|---|---|
| `ExitStatus` | 退出码 **128**（JSON `exit_code`；不是 signal） |
| stderr 长度 | 约 360 B（human `fatal: …` + `Error-Code` + 一行 JSON） |
| stdout 长度 | 断言未打印；未知（SP-01 必须补） |

同签名另外两条：`merge_rename_conflict_1to1_merges_cleanly_with_the_base_carried_over`（`failed to add tracked file`）、`merge_rename_conflict_is_presented_at_the_new_path`（`commit file`）。

空 stderr 11 条：stderr 长度 **0**；stdout / signal 未知。与 2026-09-17 下午 `rerun_command_test.log` 的 `add file:` / `commit file:` 空后缀同类。

## 同时存活 CLI 子进程

### 进程级（本轮实测）

在 `command_test` pid `4064329` 跑 merge 波次时，每秒 `pgrep libra`：

| 指标 | 峰值 | 样本 |
|---|---|---|
| 全部 `libra` 进程 | **56** | `sp00_pgrep_libra.tsv` ts `1789652066` |
| 其中 `--libra-internal-status-io-worker` | **26** | 同秒 |
| 非 worker 父 CLI | **29** | 同秒 |
| `<defunct>` | 峰值 7（另秒） | 同文件 |

32 个测试线程几乎每个都 `output()` 一个 CLI，且 status/add/commit 还会再拉一个 I/O worker，瞬时活进程 ≈ 2× 父 CLI。

### 单测内峰值（源码，未改代码）

| 用例 | 同时存活、经 `base_libra_command` 的 child | 证据 |
|---|---|---|
| `registry_mutators_serialize_on_worktrees_lock` | **3**（持 `worktrees.lock` 后同时 `worktree add`） | `tests/command/worktree_isolation_test.rs:5231-5263` |
| `spawn_service` + 后续 `run_libra_command` | **2**（service 占 1 槽，再 `output()` 另一个） | `tests/command/service_test.rs:45-75` |
| 双 worktree rebase / stash pop 屏障 | **2** | `worktree_isolation_test.rs` `Barrier::new(2)` |

SP-01 默认上限 **必须 ≥ 3**，否则 nextest 一测一进程会在三路 `worktree add` 上死锁。

## go/no-go

**go = 资源压垮 → 执行 SP-01 限流。**

不是隔离泄漏，不停、不立 `FIX-SP-*`：

- 失败簇在 merge/notes 的 CLI `add`/`commit`/`branch`/`checkout`，助手已 `env_clear`。
- 非空失败是工作区 I/O worker 帧被截断（`LBR-NET-002` / 128），与「读到宿主 config.db / 错仓库」不符。
- nextest 一测一进程下同套件已绿；本计划不得把那次绿当成「已修」——cargo-test 32 线程本轮仍 14 红。
- 整进程 SIGKILL 本轮未再现，归 `DEFER-SP-01`（是否 oomd / PDEATHSIG）。限流后若再 SIGKILL 再立 FIX。

## SP-01 默认上限

写入常数 **8**（SP-01 实测后改为 **4**，见下）：

- 下限 3（三路 `worktree add`）。
- 本轮进程级 29 个父 CLI / 56 个 `libra` 时出现 I/O worker 截断；把父 CLI 压到 8，瞬时进程大约 16，低于已失败的 50+ 带。
- `LIBRA_TEST_CLI_SPAWN_LIMIT` 可覆盖。未做 4/8/16 A/B；若 8 仍剥落，SP-01 应**下调**父 CLI 上限（资源压垮），禁止加 `#[serial(cli_spawn)]`。

### SP-01 回写（2026-09-17）

`DEFAULT_CLI_SPAWN_LIMIT=8` 下 `command_test --test-threads=32` 一轮 3469 绿 / 1 红（`test_commit_honors_cleanup_and_verbose_config`：`create_committed_repo_via_cli` 的 `libra commit` `signal=Some(9)`），一轮 3470 全绿。诊断已带上 signal。默认改为 **4**（仍 ≥ 3）。

限流只包 `base_libra_command` 的 `spawn`/`output`。`--libra-internal-status-io-worker` 是 CLI 自己拉的，不占测试信号量；减父 CLI 即可减 worker。

## 非结论

- 没有证明历史 SIGKILL 的内核根因（`DEFER-SP-01`）。
- 没有改 `tests/command/mod.rs`、merge 用例、`.config/nextest.toml`。
- 没有跑 nextest 全量（本卡不要求；权威门仍绿不构成修复）。
