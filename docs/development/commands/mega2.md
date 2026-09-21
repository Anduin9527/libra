# `libra mega2 browser` 开发设计

## 命令实现目标

`libra mega2 browser` 是本计划（plan-20260912）唯一公开的 Mega2 远端浏览入口：
把已校验的远端 listing 以「可恢复终端生命周期的交互浏览」或「单次请求的机器
可读输出」两种模式呈现给用户与自动化。它是 Libra 专属扩展，重点是**有界、
无秘密、可测**的读取面，不对应任何 Git 原生命令。

唯一行为轴是「可被使用者与自动调用者消费的 Mega2 browser public surface」；本卡
（MB-03）只注册 `browser` 一个子命令，不预留、不注册、不文档化第二个子命令。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。远端 Mega2 metadata browser，Git 无等价
  契约；本地对象检视请使用 `libra ls-tree`。
- 浏览只读且匿名：`GET /api/v1/tree` 不携带 `Authorization`；写入（建目录/删除/
  移动/tag）属于后续卡（MB-04/05、MB-07/08、MB-10/11），不在此命令。
- `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md` 均登记为
  Libra-only、无 Git 等价面。

## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands::Mega2`；`command_preflight` 归类为
  `CommandPreflight::none()`（不打开仓库数据库/对象存储），`command_scope` 归类为
  `CommandScope::ReadOnly`，因此 `operation_class_for_command` 得到
  `MutationClass::ReadOnly`。命令可在仓库外、任意目录中运行。
- 源码分层：
  - CLI/参数与输出：`src/command/mega2.rs`（`Mega2Args`、`Mega2Subcommand::Browser`、
    `BrowserArgs`、`execute_safe`、机器 payload `BrowserData`/`BrowserItem`）。
  - 交互状态机与终端生命周期：`src/command/mega2_browser/`（`BrowserState`、
    `Key`/`parse_key`、`render`、`sanitize`、`ensure_tty`/`tty_required`、`run`；
    Unix 采用 `terminal_unix.rs` 的 termios RAII guard，Windows 采用
    `terminal_windows.rs` 的 windows-sys console guard）。
  - 有界传输与 wire 校验：`src/internal/protocol/mega2_tree.rs`（URL/path/name
    校验、`Mega2TreeClient`/`Mega2TreeSession`、`ListingCache`、上限常量）。
- 执行路径：
  1. `execute_safe` → `validate_server_url` + `normalize_path`（都在终端/网络之前）；
  2. `output.is_json()` 为真 → `Mega2TreeSession::new` + `fetch`（恰好一个请求）→
     `emit_json_data("mega2 browser", …)` 输出 `{ ok, command, data }`；
  3. 否则 `mega2_browser::run`：`ensure_tty` → 终端 guard → 首屏 fetch → 事件循环
     （每个导航动作恰好一个请求）→ 退出时强制还原终端。
  4. `--quiet` 与交互模式组合被视为不相容调用（会破坏交互/机器消费），以
     `StableErrorCode::CliInvalidArguments` 拒绝并给出 `--machine` 提示。
- 输出与错误：所有失败映射为稳定 `LBR-*` 码（用法 `LBR-CLI-002`、网络
  `LBR-NET-*`、协议 `LBR-NET-002`、不可用 `Unsupported`/`LBR-CLI-003` 等）；错误
  信息不回显响应体、URL credentials、token 或未校验路径。
- 流程图：

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands::Mega2"] --> B["参数与输出<br/>src/command/mega2.rs"]
    B --> C["输入校验<br/>validate_server_url / normalize_path"]
    C --> D{"output.is_json()"}
    D -- 是 --> E["单次请求<br/>Mega2TreeSession::fetch → /api/v1/tree"]
    E --> F["JSON envelope<br/>emit_json_data(mega2 browser)"]
    D -- 否 --> G["TTY 门与终端 guard<br/>mega2_browser::run"]
    G --> H["事件循环<br/>每次导航一个请求"]
    H --> I["终端还原<br/>termios / console mode"]
    C -->|失败| J["稳定用法/网络错误"]
```

## 测试

- 单元：`cargo test --lib command::mega2`（canonical server、机器 payload schema、
  参数解析）与 `cargo test --lib command::mega2_browser`（状态机、渲染消毒、
  TTY 门、pty 还原始末状态）。
- 集成（真实二进制 + loopback mock）：`cargo test --test command_test
  mega2_browser_cli` 覆盖默认值、`--ref`/path 查询编码、JSON 与 NDJSON schema、
  非 TTY 拒绝且零请求、URL 四类拒绝、HTTP 500 与坏 schema（无响应体泄漏）、
  仓库外零本地写入、help 面（仅 `browser`，无 mkdir）。
- 架构守衛：`compat_agent_architecture_guard` 保证不引入
  ratatui/crossterm/`internal::tui`；`compat_matrix_alignment` 保证
  `COMPATIBILITY.md` / `docs/development/commands/README.md` 与 CLI 同步；
  `compat_command_docs_examples_section` 保证用户文档含 Examples 段落。

## 边界与延后

- 不读 blob、不递归、不带 token、不持久化配置；不修改 mega2。
- 终端渲染不引入第三方 TUI 依赖（G-05）。
- 建目录、目录删除/移动/tag 属后续卡；本命令的 `--json` 永远是「一次 GET」，
  不会因后续卡变成写入面。
