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

## 完成记录

### 做了什么

- 服务端：新接口 `POST /v1/sessions/{id}/agents/{agentId}/stop` 和 feature `subagentStop`。在父会话里停：取消子 agent 当前轮、停它的后台命令，这一轮不发完成通知、不叫醒；子会话记 `agent.stopped`，快照 `stoppedByUser`；父会话私有历史加一条英文说明，空闲时随下一次输入、忙时随当轮下一次模型请求带给模型。父会话在子 agent 变化时发 `agent.updated`；`AgentSummary` 加名字、类型、`stoppedByUser`、`startedAt`、`durationMs`。四个子 agent 工具改为产出 `collabAgentToolCall` 项目（开始时 inProgress，结束时带各 agent 的状态和交给模型的回复）；完成通知的 `backgroundEvent` 带上 `agent`。TypeScript 和 JSON schema 已重新生成。
- 终端（新文件 `crates/canter-tui/src/agents.rs`）：输入框上方的 `▸ N sub-agents …` 一行，点它或 `/agents` 原位置展开（`▾` 标题、每行状态符号/名字/灰色类型状态用时、按键行），↑↓、回车、`s`、Esc；没有子 agent 时 `/agents` 弹 `No sub-agents in this session.`。`/agents` 进了 `/` 列表和 `?` 帮助。回车整屏切到子会话：顶上两行、输入框灰字、`/agents` 最上面多 `← Main session · <名字>`；这一屏另订阅主会话事件流，主会话和别的子 agent 的批准照常弹出，回答发往各自会话；Esc 在子 agent 在跑时调停止接口。四种卡片、灰色 `◇ Sub-agent <name> <status> — result sent to the model`、失败红卡片，`/export` 对应英文。
- DESIGN：第 8 条加 P2-179 协议一条（含和 Codex `CollabAgentToolCall`、`turn/interrupt` 的不同和原因），新增「子 agent 终端界面（P2-179）」一节。

### 验证了什么

- 先写的停掉测试：`user_stop_does_not_wake_and_tells_the_parent_at_its_next_request`、`user_stop_while_the_parent_works_reaches_its_next_request`，改之前跑是红的（`a stop by the user must not wake the parent`，模型请求 2 次、应为 1 次），改完变绿。
- 新加的针对测试：服务端 `sub_agent_tools_leave_cards_naming_their_agents`、`a_result_sent_to_the_model_names_its_agent`（假模型回放）；终端 `run_agents_tests.rs` 四个（收起行和展开列表、`s` 只停在跑的、列表随事件变；没有子 agent 的提示；子会话一屏的两行、灰字、主会话入口、主会话批准弹出和回答走向、Esc 调停止；四种卡片、粗体名字、折叠和 ctrl+o 展开、◇ 一行），`agents.rs` 计数单测，`export.rs` 的 `sub_agent_cards_export_as_what_they_say`（现有假服务端）。
- 改过的 crate 全部测试：`cargo nextest run -p canter-protocol -p canter-core -p canter-tui -p canter-server` 和 `-p canter` 都过（canter-server 231、canter 183）。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 干净；`crates/canter-tui/src` 新加字符串里没有中文。
- 没有调真实模型，没有在真终端里用真模型走一遍。

### 拿主意的地方

- 停掉用新接口，不给现有打断加标记：列表不知道子 agent 当前的轮 ID，「不叫醒、下次请求带说明」要经父会话做；普通的子会话打断照 P2-178 不变。
- 列表靠父会话的新事件 `agent.updated` 更新，客户端不用订阅每个子会话。
- 在子会话一屏，主会话自己的批准不加 `Requested by sub-agent` 那一行（`approval.rs` 只改了判断来源的一个条件，批准框本身没改）；只转批准类请求，主会话的计划批准和提问不搬到子会话一屏。
- Esc 只在子 agent 正在跑时等于 `s`，否则照普通打断（比如 `!` 命令）。带 `parent` 的会话（包括从 `/resume` 打开的）都按子会话一屏画。
- `wait_agent` 卡片各状态都写 `Waiting for`（照线框）；结果折叠到 3 行，同 MCP 工具输出；用时用 canter 现有的 `42s` / `2m 13s`。
- 子会话一屏跟主会话的事件流断线时只重连订阅，不替主会话恢复。
- 现有切换会话的测试会多收到一次 `/agents` 列表请求，已在测试里应答。

### 没做的事

- 任务说的不要做的都没做：钩子和统计（P2-180）、自定义子 agent 类型、批准框、`canter exec`、「后台运行中」。
- `/rewind`、`/new`、`/handoff` 在子会话一屏照 P2-177 原样，没有改，也没碰到冲突。
- 子 agent 名字仍是 P2-178 的 `agent-xxxxxx`。

### 踩过的坑

- 终端测试的假服务端在时刻 0 画第一帧，回答批准后输入有 300 ms 的防误触，按键要放在之后才生效；在跑的用时要把 `startedAt` 设成负数才有值。
- 父会话多了 `agent.updated` 事件后，`running_child_blocks_trash_and_rewind_without_changing_parent` 取「之前」的快照太早，改成等这条事件到了再取。
- `crates/canter/tests/tui.rs` 读输入框上方状态行的辅助函数读到了新加的子 agent 行，改成跳过它。

## 主控审查

2026-11-02，canter/main。

- diff 只动了该动的，「不要做」一件没做；用户定的 4 条和第 1～5 节逐条做到。停掉走新接口 `POST /v1/sessions/{id}/agents/{agentId}/stop`，加 `agent.stopped`、`agent.updated` 事件和 `collabAgentToolCall` 项目，DESIGN 第 8 条写了和 Codex 的差别，同意。`approval.rs` 那处（子会话一屏里主会话自己的请求不加来源行）对。新加的界面字符串没有中文。
- 合并前全套（`08c6118`）：1493 跑、1493 过、49 跳过；文档测试过。负载约 7.5～8.9。
- 建议改（不挡合并，交用户定）：1. 子 agent 名字仍是 P2-178 的 `agent-xxxxxx`，列表、卡片、批准框都显示它，几个一起跑时不好认；线框画的是按任务取的短名字。2. 子会话一屏只转批准，主会话的 `ask_user` 提问和计划批准不转，要回主会话才看到。
- 结果：一次过
