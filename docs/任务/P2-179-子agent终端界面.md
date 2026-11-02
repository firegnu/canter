# 任务：第二期-13.3 - 子 agent：终端界面

2026-11-02，主控按 P2-78 拆出。
2026-11-02，canter/main 交给 canter/dev-subagent-tui（Claude Code，opus[1m] / xhigh）。
路由：高 / 交叉审查不要 / 影响面：改行为（路由：重（置信 0.72）、拿不准、拿不准）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
执行：派出
依据：`docs/调研/P2-78-子agent调研.md`；P2-177 做了子会话和协议，P2-178 做了模型工具、批准转发和叫醒。本件做终端里的子 agent 列表、打开子会话、停掉子 agent、对话里子 agent 工具的卡片。不做钩子和统计（P2-180）。
依赖：P2-178

## 用户原话

- 11-02 主控提议把 P2-78 拆成子会话和协议、模型工具和批准、终端界面、钩子和统计 4 件。用户：「先把任务开了吧。之后咱们按照顺序一个个讨论。讨论完一件做一件」
- 11-02 用户：「开始讨论 P2-179 吧，先给我看线框。我觉得可以使用chrome来展示，不要在终端展示」。主控给了线框（`docs/任务/P2-179-线框.html`，4 个问题各列 2 种样子并标推荐）。用户：「都按你的建议，记进HANDOFF」

## 先读

- `docs/任务/P2-179-线框.html`（用浏览器打开）：每题橙框的是用户选定的样子。
- `docs/DESIGN.md`：「子 agent 的子会话（P2-177）」「子 agent 工具和批准（P2-178）」、第 8 条协议、P2-117 后台命令那一行（`▸ N background commands`，点它等于 `/ps`）、P2-74/P2-75 弹框、P2-101 后台叫醒。
- `docs/任务/P2-178-子agent模型工具和批准.md`（含完成记录、主控审查）。
- `crates/canter-tui/src/background.rs`、`app.rs` 里画 `▸ N background commands` 和 `/ps` 列表的代码，`approval.rs` 的来源行，`cells.rs` 的工具卡片。
- `crates/canter-core/src/agents.rs`、`agent_tools.rs`，`crates/canter-server/src/agent_sessions.rs`。

## 在哪里干活

- worktree：`../canter-worktrees/p2-179-subagent-tui`，分支 `p2-179-subagent-tui`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-179-subagent-tui`。

## 要做的

界面上的字一律英文，照下面写的原文。

### 1. 子 agent 列表：输入框上方一行，可展开

- 主会话有子 agent 时，在输入框上方、和 `▸ N background commands`、待办清单同一处，显示一行：
  - 有在跑的：`▸ 2 sub-agents running · 1 done`（没有已停下的就不写 ` · N done`）；
  - 都停了：`▸ 3 sub-agents`；
  - 单数写 `sub-agent`。
- 点这一行，或输入 `/agents`，照后台命令列表的做法在原位置展开，行首变 `▾`：
  - 标题 `▾ 3 sub-agents · 2 running`；
  - 每个子 agent 一行：状态符号、名字、灰色 `<type> · <status> <用时>`，`<type>` 是 `explorer` / `general`，`<status>` 是 `running` / `completed` / `failed` / `closed` / `interrupted`，用户停掉的写 `stopped by you`；
  - 最后一行灰色 `↑↓ select · enter open · s stop · esc close`。
- 做完、关掉、中断的也列，状态写在后面。没有子 agent 时不出现这一行，`/agents` 显示 `No sub-agents in this session.`。
- 子 agent 开、停、做完时这一行和展开的列表跟着变；主会话空闲时也照样显示。
- `/agents` 加进 `/` 命令列表和 `?` 帮助。

### 2. 打开一个子 agent：整屏切到子会话，能发话

- 列表里回车，整屏切到这个子会话（P2-177 已能按 ID 打开），记录、工具卡片、滚动、搜索照主会话的画法。
- 顶上固定两行：
  - `Sub-agent · <name> · <type> · <status>`；
  - 灰色 `from: <主会话名字> · /agents to go back`。
- 输入框空着时的灰字是 `Message this sub-agent`；在这里发的话交给这个子 agent（现有输入接口），等于用户替主 agent 说了一句。子 agent 之后做完，照常通知主 agent。
- 在子会话里输入 `/agents`：列表最上面多一条 `← Main session · <主会话名字>`，回车回主会话；其余几行照第 1 节，可以直接切到别的子 agent。
- 在子会话这一屏，主会话和别的子 agent 的批准照常弹出（带 `Requested by sub-agent <name>` 那一行）。
- 在子会话这一屏按 Esc 打断子 agent，按第 3 节「停掉」处理。

### 3. 停掉一个子 agent

- 列表里选中在跑的子 agent 按 `s`：它这一轮停下，它起的后台命令一起停，状态 `interrupted`，已有结果保留，列表里写 `stopped by you`。选中的不在跑时 `s` 不做事。
- 不叫醒主会话。主 agent 下一次发模型请求时带一句英文说明：这个子 agent（ID、名字）被用户停了，除非用户要求，不要重开同样的子 agent。主会话在忙时同样附到下一次请求。
- 停掉后用户又在子会话里给它发话，它接着跑，之后做完、失败照常按 P2-178 通知或叫醒主 agent。
- 「停掉」「不叫醒」「下次请求时告诉主 agent」由服务端做，客户端只调接口（以后桌面和网页客户端用同一套）。用现有子会话的打断接口带标记，还是新加接口，由你定；协议有加的，写进 DESIGN 第 8 条，和 Codex 不一样的地方写清为什么。

### 4. 对话里四个子 agent 工具的卡片

- `spawn_agent`：`● Spawned <type> · <name>`（名字加粗），下一行灰色 `└ <任务第一行>`。
- `wait_agent`：`● Waiting for <name>, <name> · <用时>`；有结果后每个子 agent 一行 `└ ✓ <name> completed`，它的最终回复灰色、折叠，`ctrl+o` 展开（照现有工具输出折叠）；还在跑的写 `<name> still running`；`failed` / `interrupted` / `closed` 照写状态。
- `send_input`：`● Sent to <name>`，下一行灰色 `└ <消息第一行>`。
- `close_agent`：`● Closed <name>`。
- 子 agent 做完、结果自动交给主 agent（P2-178 的完成通知）时，对话里留一行灰色 `◇ Sub-agent <name> completed — result sent to the model`（状态照实写 `failed` / `interrupted`）。
- 工具报错（比如超过 4 个、计划模式开通用）照现有失败卡片画红。
- `/export` 里这几种卡片写成同样意思的英文文字。

### 5. DESIGN

新增「子 agent 终端界面（P2-179）」：写上面的规则和用户原话；协议有改的写进第 8 条。

## 怎么算做完

- 用户定的 4 条做到（线框橙框）：
  1. 子 agent 列表放在输入框上方，平时收成一行，点它或 `/agents` 原位置展开；
  2. 回车整屏切到子会话，能看记录、能直接给它发话，`/agents` 选最上面一条回主会话；
  3. 列表里按 `s` 停掉子 agent，不叫醒主 agent，它下次发请求时被告知「被用户停了」；
  4. 对话里四个子 agent 工具的卡片专门画。
- 上面第 1～5 节做到。
- 验证只做这些：
  - 每条行为写针对的测试（终端用现有的假服务端，服务端用假模型回放，不调真实模型）；停掉不叫醒、下次请求带说明这条先写测试，确认改之前失败；
  - 改了哪个 crate 就跑哪个 crate 的全部测试（`cargo nextest run -p <crate>`）；动了发给模型的请求内容的，加跑 `canter-server` 和 `canter`；
  - 加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - 检查 `crates/canter-tui/src` 新加的界面字符串里没有中文。
  - 不跑全套，不录屏。觉得不够，在回复里说，不要自己加。

## 不要做

- 不加 `SubagentStart` / `SubagentStop` 钩子，不改 `canter stats`（P2-180）。不做用户自定义子 agent 类型、fork 式子 agent。
- 不改批准框（P2-178 已做来源行）。`canter exec` 不改。
- 后台命令列表里的中文「后台运行中」不在本件改（已记 HANDOFF）。
- 界面和发给模型的文字一律英文，不出现中文。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）。
- 实现、测试和辅助命令不用 Python 或别的脚本语言，只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录、临时端口和临时 socket。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：停自己起的服务用起的时候记下的 PID。
- 不在 Cargo 里引用 ranch、paddock、cairn 的包，不从 Codex、Claude Code 搬代码（只参考）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。包括：要改协议里已有字段的含义；子会话这一屏里某些命令（比如 `/rewind`、`/handoff`、`/new`）和父子关系冲突；停掉和 P2-178 现有中断、叫醒规则冲突。
- 不合并到 main，不推送。只在 `p2-179-subagent-tui` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
