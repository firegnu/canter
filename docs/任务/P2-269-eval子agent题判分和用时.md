# 任务：eval 子 agent 题的答案判分看全部回复，子 agent 用时算到交报告

2026-11-10，canter/main 交给 canter/dev-eval-subagent-fix（Codex，`gpt-6-astra` / high）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.88）、拿不准、拿不准）
执行：派出
类型：Bug 修复
依据：P2-258 验收（`eval/results/subagent-2026-11-10.md`）发现 eval 的两处毛病，本轮只修这两处。canter 的 `wait_agent` 干等是 P2-270，不在本件。
提示：依据证据定位并修复导致问题的原因，保持无关行为不变。

## 用户原话

- 11-10（看完 P2-258 三次跑的结果）：「那这次跑了这四趟。要改什么呢？」主控答：canter 让 `wait_agent` 在交互会话里不真等，eval 修两处判分和统计的毛病。用户：「所以说就是改这两处。不用记进248了？」「可以，一件件来。」

## 背景

- 第 2 次跑（`subagent-2`）：主会话第 1 轮等两个子 agent 回来后就给出了超时的答案；后来又开两个子 agent 查「哪个代码更好读」，最后一条回复答的是这个。`answer-facts` 只看 `session-data.json` 的 `finalReply`，判成缺事实，其实答案在前面已经给了用户。
- 子 agent 表的「用时」取子 agent 会话第一条到最后一条事件。子 agent 交完报告后会话还开着，最后一条事件是整个会话关掉时的 `session.status`。第 1、2 次查 codex 的那两个，表上 5m51s、9m35s，实际交报告（最后一个 `turn.completed`）是 4m49s、3m31s。

## 先读

- `eval/src/session_data.rs`（`final_reply`、`sub_agents`）
- `eval/src/report.rs` 子 agent 一节
- `eval/tasks/subagent.toml` 的 `answer-facts`
- `eval/README.md`「判分能读的会话数据」和结果表一节

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-subagent-fix`，分支 `eval-subagent-fix`（已从 main 建好）。只动 `eval/`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-subagent-fix`。

## 要做的

1. `answer-facts` 看主会话全部回复：用户在这次跑里看到的所有主会话回复合在一起，6 条事实都提到了就算过。`session-data.json` 里加什么字段你定，写进 README；`finalReply` 保留。
2. 子 agent 的用时算到它最后一次交报告（最后一个 `turn.completed`），不算它交完以后会话还开着的时间。`session-data.json` 和结果表子 agent 一节都按这个算。

## 怎么算做完

- 验收（主控提议，用户 11-10 批准）：主控用合并后的版本对 `subagent-2026-11-10` 重新判分、重出表：第 2 次的 `answer-facts` 过了；第 1 次仍不过（真缺 pi MCP 60 秒）；两个「关闭」子 agent 的用时是 4m49s、3m31s。已提交的其他结果表重出一字不差。
- 验证只做这些：
  - 先写测试确认改前失败：答案在前面某轮、最后一条回复说别的，`answer-facts` 过；子 agent 交完报告后会话还有事件，用时算到最后一个 `turn.completed`；
  - 原有测试照过（含第九次回答过、删任意一条事实不过）；
  - `cargo nextest run -p canter-eval`、`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `crates/` 下的代码；不改 `eval/results/` 下已有的表（重出由主控做）。
- 不动 P2-258 交叉审查记下的其他建议改（见 HANDOFF）。
- 不读、不改 `~/.canter`、`~/.canter-eval`，不起真的 canter 服务端，不调真实模型接口。测试只用临时目录和造的事件。
- 不用 Python。不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的进程用记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。
- 不合并到 main，不推送。只在 `eval-subagent-fix` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：`session-data.json` 新增按事件顺序排列的 `replies`，收集主会话全部完成回复；`finalReply` 保留原有含义。`answer-facts` 将 `replies` 用换行拼接后检查原来的六条事实。子 agent 的 `endedAt` 改取最后一个 `turn.completed`，`durationMs` 从第一条事件算到这里；结果表沿用这份统计，不需要改渲染代码。README 补充了字段和结果表用时口径。
- 验证了什么：先运行新判分回归，旧实现因只看最后一条回复而报缺少 codex shell 10 秒；修复后通过，覆盖事实分散在前面两轮、最后一轮谈可读性。用时回归也先失败：最后一次完成在 6000 ms、关闭在 20000 ms，旧实现错误地取后者；修复后 JSON、collect 和结果表均断言为从 2000 ms 到 6000 ms，共 4 秒，且选的是第二次完成。`cargo nextest run -p canter-eval` 全部 61 条通过（含第九次答案、删任意一条事实不过、数字边界和毫秒写法）；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。所有 Cargo 命令使用指定独立编译目录，命令均在前台等到结束。
- 拿主意的地方：字段使用字符串数组 `replies`，完成回复的识别沿用原 `finalReply` 的规则（`item.completed` 的 `agentMessage`，状态为 completed 或旧事件未写状态）。子 agent 没有任何 `turn.completed` 时结束时间和用时为空，结果表显示 `?`。没有需要主控决定的事。
- 没做的事：未改 `crates/`、已有结果表和 P2-258 的其他建议；未读写真实 `~/.canter`、`~/.canter-eval`，未起真实服务端，真实模型调用 0 次。历史轮次重判和重出表由主控合并后验收，本分支没有声称已验证历史 4m49s、3m31s 或其他已提交表逐字不变；未合并、未推送。
- 踩过的坑：一次 README 补丁因上下文文字不匹配未应用，核对原文后缩小补丁上下文重新应用；无实现方案返工。
