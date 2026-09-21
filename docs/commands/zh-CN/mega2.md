# `libra mega2 browser`

以 HTTP 浏览远端 Mega2 仓库的**单层目录**：既可在终端中交互浏览，也可为脚本与
Agent 输出一次性的机器可读列表。

`mega2` 是 Libra 专属扩展，**没有 Git 等价契约**：它不 clone、不 fetch、不 push
Git 对象，浏览的是远端元数据而非本地 tree 对象。需要检查本地对象时请使用
[`libra ls-tree`](ls-tree.md)。

## 概要

```
libra mega2 browser --server <BASE-URL> [PATH] [--ref <COMMIT-OR-TAG>] [--json|--machine]
```

## 说明

命令在接触终端或网络**之前**会先校验全部输入：

- `--server` 必须是 `https://…`；仅当主机为 loopback（`127.0.0.1`、`::1`、
  `localhost`）时才允许 `http://…`。含 userinfo、query、fragment 或 base path
  的 URL 一律拒绝；客户端固定追加 `/api/v1/tree`。
- `PATH` 必须为 rooted 路径（默认 `/`），不得包含 `.`/`..` 组件、NUL、控制字符
  或平台分隔符。
- `--ref` 可选，用于指定 commit 或 tag；省略时使用服务端默认 revision。

每次导航或刷新只发送一个匿名 `GET /api/v1/tree`：不附带 `Authorization` 头，
不打开本地仓库（可在仓库外直接使用），也不读写任何本地状态（index、对象库、
数据库、配置）。

客户端有界：禁用 redirect 与 proxy，10 秒超时，响应体上限 1 MiB、条目上限
2000，且 `content_type` 只接受 `directory` 或 `file`。名称含 `..`、路径分隔符
或终端控制字符的条目一律 fail-closed；服务端返回的条目 `path` 不作为导航依据。

### 交互模式（默认）

交互模式要求 **stdin 与 stdout 都是终端**。任一不是 TTY 时立即以稳定错误拒绝，
并提示改用 `--json`；拒绝发生在改动终端之前。

浏览期间终端进入 raw mode，由有界状态机驱动：无递归、无后台预取。

| 按键 | 动作 |
|------|------|
| `↑`/`↓` 或 `k`/`j` | 移动选择 |
| `Enter` | 进入选中的目录（对子路径发一次请求） |
| `Backspace` 或 `h` | 返回上级目录（不会高于 `/`） |
| `+` | 在当前目录建立子目录（见下） |
| `r` | 重新加载当前列表 |
| `q`（或 `Ctrl-C`、`Esc`） | 退出 |

所有退出路径（含错误与被处理的信号）都会还原终端状态（raw mode 与备用屏幕）。

### 机器模式（`--json` / `--machine`）

使用 `--json`（或 `--machine`，等价于 `--json=ndjson --no-pager --color=never
--quiet`）时执行**恰好一次**请求，并输出标准 Libra JSON envelope：

```json
{
  "ok": true,
  "command": "mega2 browser",
  "data": {
    "server": "https://mega2.example.com",
    "ref": "v1.2",
    "path": "/src",
    "items": [
      { "name": "pkg", "content_type": "directory" },
      { "name": "main.rs", "content_type": "file" }
    ]
  }
}
```

`items` 顺序确定：目录优先，其后按名称升序。`server` 为校验后 URL 的规范
scheme/host/port origin。

`--quiet` 若不配合机器输出模式会被拒绝：抑制 stdout 会破坏交互渲染与机器消费。

### 建目录（`+`，仅交互模式）

按 `+` 打开单行名称编辑器，在当前路径下建立子目录（位于根目录时发送的
parent 即 `/`）。若当前选中项是**文件**，编辑器拒绝打开（选择项必须是目录或空白
区域）。按 `Enter` 会先用与 wire 相同的规则校验名称（拒绝 `/`、`\`、`.`、`..`、
NUL 与控制字符），通过后才发送**一次** `POST /api/v1/create-entry`
（`is_directory=true`、`skip_build=true`、无 `content`）；按 `Esc` 直接取消，完全不发
请求。

确认建立后恰好重新加载一次当前列表。失败情形（401/403、重名、超时、响应格式
错误）会保留屏幕上最后一次安全列表，并显示**不含秘密**的状态行；终端不会停留在
raw mode，TUI 也永不要求你在备用屏幕上输入原始 token。

写入 token 的解析优先级：`--token-file <path>` → 环境变量 `LIBRA_MEGA2_TOKEN` →
`--token`（会留在 shell history，建议用前两者）。token 相关 flag 仅在交互模式有
效，与 `--json`/`--machine` 同时使用会被拒绝，机器模式永不 POST。

## 选项

| 选项 | 说明 |
|------|------|
| `--server <BASE-URL>` | Mega2 服务端 base URL（必填）。HTTPS，或 loopback HTTP。 |
| `[PATH]` | 要列出的 rooted 目录路径；默认 `/`。 |
| `--ref <COMMIT-OR-TAG>` | 可选的 commit 或 tag。 |
| `--token-file <PATH>` | 仅交互模式：从文件读取写入 token（优先级最高）。 |
| `--token <TOKEN>` | 仅交互模式：内联写入 token（优先级最低；会留在 shell history）。 |
| `--json[=<FORMAT>]` | 全局标志：单次请求 + JSON envelope（`pretty`/`compact`/`ndjson`）。 |
| `--machine` | 全局标志：严格 NDJSON 机器模式，供自动化使用。 |

## 错误

| 情形 | 稳定行为 |
|------|----------|
| 无效调用（URL 非法、路径非 rooted、缺少 `--server`） | 用法错误，不发请求 |
| 交互模式但非 TTY | 改动终端或发请求之前即拒绝，并提示使用 `--json` |
| 网络不可用 / 超时 | 稳定网络错误，不回显响应体 |
| HTTP 4xx/5xx 或重定向 | 稳定错误并标明状态码；绝不打印响应体 |
| 响应格式错误或含恶意条目 | 稳定协议错误；不渲染、不缓存 |

错误绝不回显服务端响应体、凭据、token 或未校验的路径。

## 限制与边界

- 每次导航/刷新一个请求；无递归、无预取、无后台任务、无跨进程缓存。
- 浏览只读且匿名。确认 `+` 后最多一次 `create-entry` POST 加一次重载 GET；本命令
  不含删除、移动或 tag 面。
- 不持久化任何配置或凭据；不写磁盘。

## 示例

```bash
# 交互浏览远端根目录
libra mega2 browser --server https://mega2.example.com

# 直接打开某个 rooted 路径
libra mega2 browser --server https://mega2.example.com src/pkg

# 列出指定 commit 或 tag
libra mega2 browser --server https://mega2.example.com --ref v1.2

# 交互建目录：在 TUI 中按 +，可配 --token-file
libra mega2 browser --server https://mega2.example.com --token-file ~/.mega2-token

# 单次请求 + JSON envelope（无需 TTY，可在仓库外运行）
libra --json mega2 browser --server https://mega2.example.com

# 供自动化使用的 NDJSON
libra --machine mega2 browser --server http://127.0.0.1:8080
```

## 与 `libra ls-tree` 的对比

| 维度 | `libra mega2 browser` | `libra ls-tree` |
|------|----------------------|-----------------|
| 数据来源 | 远端 Mega2 HTTP API（`/api/v1/tree`） | 本地对象数据库 |
| 是否需要仓库 | 否 | 是 |
| 深度 | 每次请求恰好一层目录 | 任意 tree 路径（`-r` 可递归） |
| 鉴权 | 匿名（不带 token） | 本地仓库访问 |
| Git 兼容性 | 无（Libra 专属扩展） | Git 兼容 plumbing |
