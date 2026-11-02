# 任务：第二期-13.4 - 子 agent：钩子和统计

2026-11-02，主控按 P2-78 拆出。
2026-11-02，canter/main 交给 canter/dev-subagent-hooks-stats（Codex，gpt-6-astra / xhigh）。
路由：高 / 交叉审查不要 / 影响面：改行为（路由：重（置信 0.86）、拿不准、拿不准）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
执行：派出
依据：`docs/调研/P2-78-子agent调研.md`；P2-177～P2-179 做了子会话、模型工具和批准、终端界面。本件让 P2-153 的全局钩子认得子 agent（新加 `SubagentStart`、`SubagentStop`），并让 `canter stats` 显示子 agent。是 P2-78 计划内的最后一件。
依赖：P2-179

## 用户原话

- 11-02 主控提议把 P2-78 拆成子会话和协议、模型工具和批准、终端界面、钩子和统计 4 件。用户：「先把任务开了吧。之后咱们按照顺序一个个讨论。讨论完一件做一件」
- 11-02 主控给了选项页（`docs/任务/P2-180-选项.html`，3 个问题各列 2 种并标推荐）。用户：「都按你的建议」

## 先读

- `docs/任务/P2-180-选项.html`（用浏览器打开）：每题橙框是用户选定的。
- `docs/DESIGN.md`：「钩子（P2-153）」整节（含 P2-172）、「子 agent 的子会话（P2-177）」「子 agent 工具和批准（P2-178）」「子 agent 终端界面（P2-179）」、遥测和统计（P2-169、P2-170）、第 8 条协议。
- `crates/canter-core/src/hooks.rs` 和调用它的地方；`crates/canter-core/src/agents.rs`、`agent_tools.rs`；`crates/canter-server/src/agent_sessions.rs`。
- 统计：`/v1/sessions/{id}/stats` 的服务端实现，`crates/canter-tui/src/stats.rs`（文字、`--json`、HTML 三种输出）。
- 参考（只读）：[Claude Code hooks 文档](https://code.claude.com/docs/en/hooks) 的 SubagentStart、SubagentStop 和子 agent 里钩子输入的 `agent_id`、`agent_type`。

## 在哪里干活

- worktree：`../canter-worktrees/p2-180-subagent-hooks-stats`，分支 `p2-180-subagent-hooks-stats`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-180-subagent-hooks-stats`。

## 要做的

### 1. 子会话里现有钩子怎么触发（照 Claude Code）

先查清现在子会话里哪些钩子会触发，再改成下面这样：

- **PreToolUse、PermissionRequest、PostToolUse**：子会话里照常触发，作用照旧（PreToolUse 可拦、PostToolUse 反馈追加到工具结果）。stdin 多三个字段：`agent_id`（子会话 ID）、`agent_type`（`explorer` / `general`）、`parent_session_id`。`session_id`、`CANTER_SESSION_ID` 照旧是子会话自己的 ID。主会话里的事件不加这三个字段。
- **UserPromptSubmit**：子会话里只在用户自己（P2-179 子会话一屏）输入时触发，同样带这三个字段；主 agent 交给它的任务、`send_input`、叫醒都算自动输入，不触发（和现在目标续跑、后台叫醒一样）。
- **Stop、StopFailure、Interrupt**：子会话里不触发，换成 `SubagentStop`。
- **SessionStart、SessionEnd**：子会话不触发，包括用户在子会话一屏打开、离开它（P2-172 的客户端离开）。
- 受限打开的会话不跑钩子的规则照旧，子会话按自己的受限状态（P2-178 定的照开的那一刻）。

### 2. 新加两个事件，只通知

| 事件 | 什么时候 | stdin 多的字段 |
|---|---|---|
| `SubagentStart` | 主 agent 开出子 agent，子 agent 第一轮开始前 | `agent_id`、`agent_type`、`parent_session_id`、`task` |
| `SubagentStop` | 子 agent 的每一轮结束：做完、失败、中断（含主会话 Esc 连带停、花费上限停）、被用户停掉（P2-179）、被 `close_agent` 关掉；用户在子会话一屏发话那一轮结束也发 | `agent_id`、`agent_type`、`parent_session_id`、`status`（`completed` / `failed` / `interrupted` / `closed`）、`last_assistant_message`（这一轮最后一条助手消息，没有为空串） |

- 两个都是通知：异步跑，不让子 agent 或主会话等，结果不影响行为；关会话、停服务端时并进现有 10 秒通知预算（照 Stop）。
- `session_id`、`cwd`、`permission_mode` 等共同字段按子会话填。
- matcher 照「其它事件忽略 matcher」。
- 配置校验认得这两个事件名；`canter exec` 下照样触发。

### 3. `canter stats` 显示子 agent

统计由服务端算，终端只负责排版（以后桌面和网页客户端用同一份数据）。

- `canter stats <主会话 ID>`：
  - 有子 agent 时，用量和花费分两行：`Usage (this session): …` 是主会话自己的，`Usage (with sub-agents): …` 是加上全部直接子 agent 的；格式照现有用量那一行。没有子 agent 时和现在一样只有一行。
  - 在概览后面加一节 `Sub-agents`，每个子 agent 一行，列：`Name`、`Type`、`Status`、`Requests`、`In`、`Out`、`Cost`、`Tools`（工具调用次数）、`Time`（从开出来到最后一轮结束或现在）；最后一行灰色 `Details: canter stats <sub-agent id>`。没有子 agent 时不出这一节。
  - 拿不到的值照现有规矩写 unknown，不当成 0；已经删掉的子会话跳过。
  - 后面的 `Requests`、`Tools` 两节照旧只算主会话自己的。
- `canter stats <子 agent ID>`：照普通会话显示，概览里多一行 `Parent: <主会话 ID> · <type>`。
- `canter stats` 的会话列表照旧不列子 agent。
- `--json` 和 HTML 输出带同样的内容；协议加的字段写进 DESIGN 第 8 条。

### 4. DESIGN

钩子那一节的表格加两行和子会话的规则；统计那一节加子 agent 的显示；协议改动写进第 8 条。写上用户原话。

## 怎么算做完

- 用户定的 3 条做到（选项页橙框）：
  1. 子 agent 里照常触发工具类钩子，并标明是哪个子 agent；子 agent 一轮结束发 `SubagentStop`，不发 `Stop`；开出子 agent 时发 `SubagentStart`；
  2. `SubagentStart`、`SubagentStop` 只通知，不拦、不改子 agent 的行为；
  3. `canter stats <主会话>` 的总花费把子 agent 算进去，下面每个子 agent 一行。
- 上面第 1～4 节做到。
- 验证只做这些：
  - 每条行为写针对的测试，用假模型回放，不调真实模型；钩子用测试里写进隔离 HOME 的脚本记下 stdin；
  - 改了哪个 crate 就跑哪个 crate 的全部测试（`cargo nextest run -p <crate>`）；
  - 加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - 不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不让 `SubagentStart` / `SubagentStop` 拦下或改子 agent 的行为，不往子 agent 上下文里加东西；不给现有 `Stop` 加拦下接着干。
- 不做项目级钩子、prompt / http 类型的钩子（第三期）。
- 不改子 agent 的名字（P2-183），不做子 agent 之间对话（P2-182），不改终端界面（统计输出除外）。
- 界面、统计输出和发给模型的文字一律英文，不出现中文。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）；花费照现有按模型价格算的办法。
- 实现、测试和辅助命令不用 Python 或别的脚本语言，只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录、临时端口和临时 socket。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：停自己起的服务用起的时候记下的 PID。
- 不在 Cargo 里引用 ranch、paddock、cairn 的包，不从 Codex、Claude Code 搬代码（只参考）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。包括：现有钩子在子会话里的行为和上面写的冲突、改了会破坏 P2-153/P2-172 已有的行为；统计要改已有字段的含义。
- 不合并到 main，不推送。只在 `p2-180-subagent-hooks-stats` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
