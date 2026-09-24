# Session Capture decision

AgentTraces ingest (`ingest_agent_traces_payload_with_scope`) asks
`session_capture::decide` for two facts: the SQLite `agent_session.state`
string, and whether the existing committed or subagent checkpoint writer
should run. `decide` is a pure match over `LifecycleEventKind`. It does not
open a repository, read a transcript, or call `transition_phase`.

`repo_path == None` still skips checkpoint writes after the decision returns
`Committed` or `SubagentBoundary`. Owner filtering for `SessionStart` and
`TurnStart`, and the `stopped_at` column, stay in `runtime.rs`. Import,
`libra agent session` stop/resume, and the coverage gate write state on
their own paths and do not call `decide`.

AgentTraces 的 session_state 与 checkpoint 类别只来自 session_capture::decide。
transition_phase 只服务 HookTarget::AiIntent 的 session_phase，本文件不修改它。
decide 不读取 SessionPhase，也不复制 Entire 的 git session 存储。
本计划不编辑 docs/development/tracing/agent.md。
