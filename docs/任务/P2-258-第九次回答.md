# 第九次测试的最终回答（P2-258 判分用的真实例子）

P2-248 11-08 第九次测「主动开子 agent」，DeepSeek flash，canter `aa8877b`，源码 codex `57a808a`、pi-mono `085c84e`（当时在 `/tmp/codex`、`/tmp/pi-mono`）。下面是主会话合并两份报告后的最后一条回复原文。

````markdown
两份报告都到了。我先抽查几处最关键的结论再汇总。
两份报告都到了，关键结论我抽查过（下面标注了哪些是我亲自读过的行）。**猜测也成立**：pi-mono 几乎全在 `bash.ts` 一个文件里，codex 散在 `exec.rs` / `unified_exec/*` / `tools/handlers` / `tools/context.rs`。

## 一句话结论

**超时语义正好相反**：codex 给每个表面都配了默认值（shell 默认 10 秒）但一次性 shell 超时对模型是**隐身的**（返回 `success: true`，只剩 exit code 124）；pi-mono 的 `bash` **根本没有默认超时**，但一旦超时会给模型一条明确文案 `Command timed out after N seconds` 并标 `isError: true`。

## 默认值对比

| 场景 | codex | pi-mono |
|---|---|---|
| 模型 shell 工具 | **10 000 ms**，`codex-rs/core/src/exec.rs:61`（`DEFAULT_EXEC_COMMAND_TIMEOUT_MS`），在 `tools/handlers/unified_exec/preparation.rs:247-252` 取 `timeout_ms.unwrap_or(...)` | **无默认**，只有模型传 `timeout`（秒，可小数），`packages/coding-agent/src/core/tools/bash.ts:41` |
| 旧 shell 执行引擎 | 同样 10 000 ms（`exec.rs:183-186`） | 不适用（只有一套） |
| 交互式 shell / 轮询 | yield 10 000 ms（夹在 250–30 000）；`write_stdin` 250 ms，空轮询 5 s，上限 300 s | 无 |
| MCP tools/call | **300 s**，`codex-mcp/src/rmcp_client.rs:123`；每 server 可配 `tool_timeout_sec`；库级 fallback 无 | **60 s**，`coding-agent/src/extensions/mcp/runtime.ts:50`；库级 fallback 30 s（`packages/mcp/src/client.ts:38`） |
| 用户 `!cmd` | 1 小时（`core/src/tasks/user_shell.rs:47`） | 无超时，只能 Esc |
| 其他工具 | `apply_patch` 无超时、`web_search` 无本地超时 | `read/write/edit/grep/find/ls` 全无超时 |
| 全局 per-tool 超时 | **没有**，也没有 CLI flag | **没有**（`agent-loop.ts:831-867` 只传 turn 的 AbortSignal） |

## 强制机制

- **codex**：deadline + `tokio::select!`，不给进程包 `tokio::time::timeout`。`unified_exec/process_manager.rs:650-675` 到点后置 `timed_out`、`mark_timed_out()`、`terminate_confirmed()`；退出码被伪造成 124（`unified_exec/process.rs:215-218`）。旧引擎走 `exec.rs:1026-1041` 的 `select!` + `killpg(SIGKILL)`。MCP 用 `timeout_at`，且取 `min(server tool_timeout, 调用方)`。
- **pi-mono**：`setTimeout` + 进程组 kill（macOS 先 `SIGSTOP` 再 `SIGKILL`，Windows `taskkill /F /T`），`bash.ts:125-158` + `utils/shell.ts:220-262`；等的是 stdio 关闭而不只是 shell 退出，所以能发现"shell 已退出但后台进程还在写 pipe"的情况。

## 超时后模型看到什么（原文）

**codex — unified exec 一次性路径（模型默认用的）**：`core/src/tools/context.rs:454-465` 恒为 `Some(true)`，回复文本只有表头 + 缓冲输出（`context.rs:545-568`）：

```text
Wall time: 10.0031 seconds
Process exited with code 124
Output:
<到超时为止的输出>
```

没有任何"超时"字样，模型只能自己把 124 解读成超时；且 `oneshot.rs:107-112` 会清掉 `process_id`，因此无法续接。

**codex — 旧 exec 路径**（hooks、`!cmd`、app-server `command/exec`）：`core/src/tools/mod.rs:129-139`，标 `success: false`：

```text
command timed out after 1000 milliseconds
<aggregated output>
```

**codex — MCP**：`core/src/mcp_tool_call.rs:675-678` 包一层，`is_error: true`：

```text
tool call error: timed out awaiting tools/call after 300s
```

**pi-mono — bash**（`bash.ts:399-414`，`isError: true`，`agent-loop.ts:855-863` 转换）：

```text
<截断后的部分输出，尾部 2000 行 / 50 KB>

Command timed out after 1 seconds
```

如果 shell 早就退出、只是后台进程还攥着 pipe（#11613），会追加：`The shell exited with code 3 after 0.2 seconds, but background processes it started kept writing output until the timeout.` 用户取消则是 `Command aborted`。

**pi-mono — MCP**：`MCP request timed out after 60000ms`，`isError: true`，同时给 server 发 `notifications/cancelled`（reason `"Request timed out"`）。

## 值得注意的三个差异

1. **codex 内部不一致**：同样是超时，旧 exec 路径明确告诉模型 "command timed out after N milliseconds"，unified exec 路径却标记成功、只留 124。报告作者也没找到覆盖这一点的测试（只有 `session/tests.rs:4759` 测了旧路径的文案）。这是个真实的盲区。
2. **默认值方向相反**：pi-mono 把 shell 超时完全交给模型决定，不传就永不超时（只剩用户按 Esc）；codex 默认 10 秒兜底，但 MCP 反而比 pi-mono 宽松 5 倍（300 s vs 60 s）。
3. **两边都没有专门的 UI 超时提示**：codex 只让 `ExecCommandEnd` 带 exit 124 + `Failed`（`ExecToolCallOutEnd` 压根没有 `timed_out` 字段，`protocol.rs:3982-4024`），pi-mono 只是把工具框底色换成 error 并显示同一条文案。codex 里有个 `"error: command timed out after {} ms"`（`protocol/src/error.rs:995-1001`）但只给 UI 用、不下发模型。

## 可信度说明

我亲自核对过的行：`bash.ts` 的 schema / `ShellTimeoutError` / 超时 catch 分支 / `timeout:` 抛出点，`preparation.rs:247-252`、`exec.rs:61`、`context.rs:454-465` 与 `545-568`、`rmcp_client.rs:123`。其余（如 `write_stdin` 的 250 ms/5 s、hook 600 s、`process_manager.rs:650-675` 的细节）来自 agent 报告，我没有逐行复核。

````
