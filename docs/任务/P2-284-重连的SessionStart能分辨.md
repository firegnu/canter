# 任务：钩子能分辨重连那次 SessionStart 不注入

2026-11-11，cairn 主控提出、用户让转给 canter。
执行：主控
依赖：P2-281

## 用户原话

- 11-11（cairn 那边）：断线重连的 SessionStart 分不出来，用户选 A「写一份需求交 canter」。
- 11-11（canter 主控问怎么表示、谁来做）：选「加一个字段」「主控在 main 上直接修」。
- 11-11（cairn 主控转达）：需求文件在 cairn 主控会话的临时目录里，用户让 canter 自己存一份。

## 需求原文（cairn 主控，照抄）

> # 需求：钩子能分辨“这次 SessionStart 的输出会不会注入”
>
> 来自 cairn 主控（2026-11-11），草稿，用户定了才交 canter。
>
> ## 起因
>
> canter 的 `SessionStart`、`source: "resume"` 有两种情况：
>
> - 打开存盘会话：成功退出时 stdout 的 `hookSpecificOutput.additionalContext` 会注入；
> - 客户端离开已发 SessionEnd 后重连：只通知，输出丢掉。
>
> stdin 里没有字段能区分这两种。钩子不知道自己的输出被丢了：比如 cairn 会记下“这些内容已经给过这个会话”，之后就不再补给它，模型其实没看到。
>
> 这不是 cairn 的特例：任何靠 SessionStart 注入、又要记住注入过什么的钩子都会碰到。
>
> ## 要的结果
>
> - 钩子从 SessionStart 的 stdin 就能分出：这次的输出会注入，还是只是通知。
> - 已有的字段和取值含义不变（打开存盘会话照旧是 `resume`）。
> - 怎么表示由 canter 定，比如重连那次的 `source` 用一个新值，或者加一个字段。
>
> ## 不要做
>
> - 不为 cairn 写特例。

## 用户定的（11-11）

1. **怎么表示**：选「加一个字段」（主控建议）。SessionStart 的 stdin 加 `accepts_additional_context`：新建、打开存盘会话为 `true`，客户端重连为 `false`；`source` 取值不变，不动 P2-172 定的「重连发 resume」。没选的：重连 `source` 改成新值 `reconnect`（会让按 `resume` 判断的现有钩子在重连时不再匹配）；不做。
2. **谁来做**：主控在 main 上直接修。

## 完成记录

- 做了什么：`crates/canter-core/src/hooks.rs` 的 `session_start`（新建、打开存盘会话）给 `accepts_additional_context: true`，`crates/canter-core/src/clients.rs` 的重连通知给 `false`。DESIGN 钩子事件表和 P2-281 那段补上字段说明。提交 `0c5ee97`。
- 验证了什么：在 `reload_tests::hooks::client_exit::repeated_client_reconnections_notify_without_reinjecting_context` 里加断言（首次 startup 为 true，两次重连为 false），改前失败（字段为 Null），改后通过。`cargo nextest run -p canter-server -p canter-core -p canter -p canter-protocol` 1021 过、28 跳过；clippy、fmt、`git diff --check` 通过。没调真实模型。
- 没做的事：没加给 UserPromptSubmit 等别的事件；没改 `source`。
- 踩过的坑：没有。
