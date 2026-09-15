# `libra ls-remote`

列出远程仓库通告的引用，不下载对象，也不更新本地引用。

```bash
libra ls-remote [OPTIONS] <repository> [patterns...]
```

在 Libra 仓库内运行时，`<repository>` 可以是已配置的远程名称，也可以是 URL，或本地 Git/Libra 仓库路径。

## 选项

| 标志 | 说明 | 示例 |
|------|-------------|---------|
| `--heads` | 只显示 `refs/heads/*` 分支引用 | `libra ls-remote --heads origin` |
| `-t`, `--tags` | 只显示 `refs/tags/*` 标签引用 | `libra ls-remote --tags origin` |
| `--refs` | 省略 `HEAD` 和以 `^{}` 结尾的 peeled 标签引用 | `libra ls-remote --refs origin` |
| `--symref` | 在对应 OID 行之前打印 symbolic-ref 目标（如 `ref: refs/heads/main\tHEAD`）。远端通告的 `symref=` capability 优先；缺失 capability 时（尤其本地 Libra 源），使用与 fetch 相同的 HEAD OID / 分支 tip 解析器合成 `HEAD`。 | `libra ls-remote --symref origin` |
| `patterns...` | 匹配完整引用名或尾部路径组件；`*` 和 `?` 遵循 Git 风格 glob 行为，并且可以匹配 `/` | `libra ls-remote origin main 'refs/heads/*'` |

## 人类可读输出

每个匹配引用按如下格式打印：

```text
<object-id>	<refname>
```

示例：

```text
4f3c2d1a...	HEAD
4f3c2d1a...	refs/heads/main
```

## JSON 输出

使用 `--json` 时，输出使用标准命令信封：

```json
{
  "ok": true,
  "command": "ls-remote",
  "data": {
    "remote": "origin",
    "url": "https://example.com/repo.git",
    "heads_only": false,
    "tags_only": false,
    "refs_only": false,
    "patterns": [],
    "entries": [
      {
        "hash": "4f3c2d1a...",
        "refname": "refs/heads/main"
      }
    ]
  }
}
```

## 示例

```bash
# 列出具名远程的所有引用
libra ls-remote origin

# 直接列出 URL 的所有引用（不需要注册远程）
libra ls-remote https://example.com/repo.git

# 限制为匹配模式的分支
libra ls-remote --heads origin main

# 面向代理的结构化 JSON 信封，仅标签
libra --json ls-remote --tags origin
```

`libra ls-remote --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 说明

- `ls-remote` 只执行协议发现（对本地 Git 仓库等价于 `git-upload-pack --advertise-refs`）。
- 它不会写入对象、远程跟踪引用、配置或工作树文件。
- `--heads` 和 `--tags` 可以组合使用，以同时显示分支和标签引用，同时排除 `HEAD`。

## 畸形 HTTP(S) discovery 响应

在 HTTP(S) 引用发现（discovery）期间，Libra 会拒绝零字节广告和畸形
pkt-line 帧，包括不完整或非十六进制标头、小于四的帧长度以及截断的 payload。
合法的 `0000` flush 与未收到响应有明确区别；合法的空仓库广告仍受支持。
不支持的 object-format capability 使用固定错误消息
`Unsupported object format capability`，不回显远端提供的值。
请确认 URL 指向 Git smart HTTP 服务，并检查代理是否截断或替换了响应，然后重试。

## pkt-line 错误归类

检测到的 pkt-line 帧格式错误返回 `LBR-NET-002`（退出码128），包括空的 HTTP(S)
discovery 广告。普通连接失败、连接重置和超时返回 `LBR-NET-001`（退出码128）。
协议错误发生时请核对 Git 服务及代理响应。discovery 帧错误的提示为
`check that the remote serves Git data and that a proxy has not altered the response`。

此归类用于引用 discovery；认证失败与本地配置读取错误保持原有错误码。

## SSH 广告错误处理

SSH advertisement 长度 `0001` 至 `0003`、不完整标头（包括零字节 EOF）或截断
payload 均作为 pkt-line 协议错误返回 `LBR-NET-002`。固定协议原因与 marker 保留，
该协议错误不会插入捕获到的 SSH stdout/stderr。

未收到完整广告也可能是 SSH 在 Git 协商前因连接、主机信任、认证或仓库访问失败。
本版本仍将这类不完整广告报告为 `LBR-NET-002`；当能观察到本地 SSH 非零退出状态时，
消息仅追加 `SSH exited with status N` 与固定指引，提示检查 SSH 连接、可信主机键、
ssh-agent 认证及远端仓库权限。协议诊断不展示原始 SSH stderr；这条路径目前尚未
提供针对具体 host-key 故障的分类指引。

必需标头不完整时，Libra 先给 SSH 最多100毫秒回报退出状态，再请求终止仍在运行的
子程序；其它广告读取错误立即请求终止。状态观察与直接子程序清理共用两秒总预算。
清理失败不会替换主要协议原因；这不承诺回收任意后代程序。

普通 IO 与超时保留传输错误分类。清理可能终止仍在运行的子程序，因此报告的退出
状态与可用诊断长度可能改变；本地清理警告会附在已收集的程序结果之后。交互 stderr
继承及其它 SSH 子程序诊断保留既有行为，本改动不代表抑制全部 SSH 终端消息。

`git://` 取对象阶段已经将这些帧归类为 `LBR-NET-002`，Git discovery 仍可能返回
`LBR-NET-001`。非 ASCII/非 hex 标头保留既有分类，HTTP(S) 行为不变。
