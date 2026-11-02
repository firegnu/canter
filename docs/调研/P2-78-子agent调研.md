# P2-78 子 agent 调研

2026-11-02，canter/main（主控）做。用户：「先调研codex和claude code的子agent怎么做。」看完对比后用户问主体照哪家，主控答「主体照 Codex（v1），几处行为借 Claude Code」，用户：「好的，写调研报告吧」。

本轮只读：没有改 canter 代码、配置或 DESIGN，没有调用模型，没有开别的 agent。**事实**是查到的源码、官方文档或 canter 现有代码，方括号是出处编号（见文末）；**建议**是主控的推荐，待用户逐条定，不是任务书。

## 版本和入口

| 对象 | 依据 |
| --- | --- |
| canter | main `093863b`，工作区干净。[K0] |
| Codex | `rust-v0.166.0`，commit `6ff1e1ca3b30fc8b914720681fcf5fe62e95f856`（2026-10-29），P2-123 按标签克隆的 `/tmp/p2-123-src/codex`，只读。[C0] |
| Claude Code | 官方文档 <https://code.claude.com/docs/en/sub-agents>，2026-11-02 读取。不开源，只看文档。[A0] |

## 结论摘要

- **事实**：两家的子 agent 大体一样：同一个程序里另开一个会话，有自己的上下文窗口，做完只把最终结果交回主会话；权限跟着主会话走；子 agent 要批准时，批准框在主界面弹出并标明是哪个子 agent。
- **事实**：差别主要在三处：什么时候开（Codex 只在用户明说时开，Claude Code 由模型自己判断）、能不能嵌套（Codex 默认 1 层，Claude Code 默认 3 层）、能不能带上主会话的上下文（两家都能，Claude Code 叫 fork 并默认打开）。
- **建议**：canter 主体照 Codex v1：服务端里带父会话标记的子会话，工具 `spawn_agent` / `wait_agent` / `send_input` / `close_agent`，嵌套 1 层。几处行为借 Claude Code：只读查找允许模型自己开、做完叫醒主 agent、批准框标明来源、输入框下方列出子 agent。用户自定义子 agent 文件和 fork 放到以后。

## 1. Codex（`rust-v0.166.0`）

- **两代都在**：`multi_agent`（v1）稳定、默认开；`multi_agent_v2` 稳定、默认关；v2 的动态工具、子 agent 独立环境还在开发中。[C1]
- **工具**：
  - v1：`spawn_agent`、`send_input`、`resume_agent`、`wait_agent`、`close_agent`。[C2]
  - v2：`spawn_agent`、`send_message`、`followup_task`、`wait_agent`、`list_agents`、`interrupt_agent`。v2 的 `wait_agent` 等的是「收件箱有更新」，不直接返回内容。[C2]
- **`spawn_agent` 的参数**（v1）：`message`（任务）、`agent_type`（角色）、`fork_context`（true 时把当前整段对话复制给子 agent，不填就只带任务那句话）、`model`、`reasoning_effort`。[C3]
- **什么时候开**：工具说明里写死「Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents, delegation, or parallel agent work.」，并说「仔细查查」「深入分析」都不算授权。说明里还教模型先自己列计划，只把不挡住下一步的旁支活派出去，挡路的活自己做。[C4]
- **内置角色**：
  - `default`；
  - `explorer`：回答具体的代码问题，鼓励并行开几个，结果直接信，不重复查；
  - `worker`：写代码，要讲清楚负责哪些文件，并告诉它「不是一个人在改这个仓库，别改回别人的改动」；
  - `awaiter`（专门等长命令）已被注释掉。[C5]
- **上限**：v1 同时最多 6 个，v2 每个会话 4 个；嵌套默认 1 层，即子 agent 不能再开子 agent。[C6]
- **权限继承**：子 agent 的配置从主会话当下的设置生成，包括模型、思考档位、批准档位、沙箱、工作目录，再叠上角色的覆盖。[C7]
- **批准**：子 agent 是服务端里的另一个会话，来源记为「由某父会话派生」。它的批准请求照常发给客户端，终端界面认出是自己会话派生的，就在主界面弹出。[C8]
- **界面**：`/agents` 打开子 agent 列表，可以切过去看。[C9]
- **其他**：Codex 的审查（review）本身就是开一个子 agent 跑，并关掉它的多 agent 功能。[C10]

## 2. Claude Code（官方文档，2026-11-02）

- **跑在哪**：文档只说子 agent 在同一个会话里工作，每个子 agent 有自己的上下文窗口、系统提示词、工具和权限；没说是不是同一个进程。[A0]
- **怎么定义**：Markdown 文件加 YAML 头，放在 `.claude/agents/`（项目）或 `~/.claude/agents/`（用户）。必填 `name`、`description`；可选 `tools`（白名单）、`disallowedTools`、`model`、`permissionMode`、`maxTurns`、`skills`、`mcpServers`、`hooks`、`isolation: worktree`、`background` 等。[A0]
- **内置子 agent**：
  - `Explore`：只读，不能写文件，专门查代码；
  - `Plan`：计划模式里查资料，只读；
  - `general-purpose`：全部工具。
  - 另有几个专用的。[A0]
- **什么时候开**：模型按任务和各子 agent 的 `description` 自动派；用户也可以点名，或用 @ 提及。[A0]
- **上下文和结果**：默认从空白开始，看不到主会话的对话；只把最终报告交回，查找过程留在子 agent 自己的上下文里。子 agent 自己也会自动压缩。[A0]
- **前台和后台**：从主会话开的子 agent 默认在后台跑，做完后在下一轮以通知的形式交回结果；前台的会挡住主会话直到做完。[A0]
- **嵌套和上限**：默认最多 3 层；同时最多 20 个，一个会话里总共开多少不限。[A0]
- **接着用**：每次调用都是新的实例；要接着聊，按 agent ID 用 `SendMessage` 发话，子 agent 保留完整历史。`Explore`、`Plan` 是一次性的，不能接着用。[A0]
- **权限**：继承主会话的放行规则。主会话在 `bypassPermissions`、`acceptEdits` 或 auto 档位时，子 agent 也跟着用同一档。[A0]
- **批准**：后台子 agent 要批准时，批准框在主会话弹出，标明是哪个子 agent 在问；按 Esc 只拒这一次调用，子 agent 继续跑。文档原话：「No message from any agent counts as your approval for a pending permission prompt.」[A0]
- **fork**：带上整段对话的子 agent，系统提示词、工具、模型都和主会话一样，所以能共用提示缓存、更省钱；fork 不能再开 fork。fork 默认打开。[A0]
- **界面**：输入框下方一行一个子 agent，上下键选，回车打开它的记录并能给它发话，`x` 停掉。[A0]
- **钩子**：`SubagentStart`、`SubagentStop`。[A0]

## 3. 对比

| | Codex | Claude Code |
| --- | --- | --- |
| 跑在哪 | 服务端里另开会话，记父会话 | 同一会话内，各自上下文 |
| 什么时候开 | 只在用户或 AGENTS.md 明说时 | 模型自己判断，或用户点名 |
| 内置角色 | default / explorer / worker | Explore / Plan / general-purpose |
| 自定义 | 配置文件里加角色 | `.claude/agents/*.md` |
| 上下文 | 默认空白，`fork_context` 可带整段 | 默认空白，fork 带整段并共用缓存 |
| 结果 | `wait_agent` 拿最终回复，结束时也有通知 | 只回最终报告；后台的以通知交回 |
| 嵌套 / 同时几个 | 1 层 / 6 个（v2：4 个） | 3 层 / 20 个 |
| 权限 | 继承档位、沙箱、工作目录 | 继承放行规则和档位 |
| 批准 | 在主界面弹，标明来源 | 在主会话弹，标明来源；Esc 只拒这一次 |
| 界面 | `/agents` 列表 | 输入框下方列表，可打开、发话、停掉 |
| 并行改代码 | 靠提示词讲清分工 | 可选单独的 git worktree |
| 钩子 | 无 | `SubagentStart` / `SubagentStop` |

## 4. canter 现在有什么能用上

- **会话和分叉**：canter 已有完整的会话（存盘、重开、压缩、遥测），P2-157 有会话分叉 `POST /v1/sessions/{id}/fork`。子 agent 做成子会话，这些都能直接用上。[K1]
- **只读工具清单**：计划模式已有一份只读工具白名单（`read`、`web_fetch`、`web_search`、`ask_user`、`submit_plan`、`shell_output`、`shell_stop`、`monitor`、`mcp_search`，以及被判为只读的 shell 命令），可以直接当「只读查找」子 agent 的工具集。[K2]
- **叫醒**：P2-101 已有后台事件叫醒：会话空闲时自动开一轮，忙时附到下一次请求，两秒内的事件合并。子 agent 做完叫醒主 agent 可以复用。[K3]
- **钩子**：P2-153 的全局钩子还没有 Subagent 事件，DESIGN 写明「没有实现……Subagent 事件」，留着做子 agent 时补。[K4]
- **协议**：服务端和客户端的协议照 Codex app-server v2 的结构（CLAUDE.md、DESIGN 第 8 条）。子 agent 照 Codex 做，协议也能照它，不同的地方写进 DESIGN。

## 5. 建议（待用户定）

主体照 Codex v1，几处借 Claude Code。

1. **跑在哪**：同一个服务端，子 agent 是带父会话标记的子会话，有自己的上下文窗口和压缩。不另开进程。
2. **工具**：
   - 主 agent 用 `spawn_agent`、`wait_agent`、`send_input`、`close_agent`，照 Codex v1，不做 v2 的收件箱。
   - 内置两种子 agent：「只读查找」用计划模式那份只读工具；「通用」用全部工具，但不能再开子 agent（嵌套 1 层）。
   - 同时最多 4 个。
   - 第一版不做用户自定义子 agent 文件。
3. **批准**：
   - 继承主会话的权限档位、放行规则和沙箱。
   - 子 agent 要批准时，批准框在主界面弹出，标明是哪个子 agent；拒绝只拒这一次。
   - 任何 agent 发来的话都不算用户批准。
4. **和 P2-83 的分界**：
   - 子 agent 是 canter 会话内部的：由模型发起，用完即走，结果回到主上下文。
   - corral 管跨进程、跨不同家 agent、长期开着、各有 worktree 的派活。canter 不调 corral；P2-83 只做 canter 对外的公开接口，corral 去适配。
5. **什么时候开**：「只读查找」允许模型自己开（照 Claude Code，省主上下文正是要子 agent 的理由）；会改文件的子 agent，只在用户或 AGENTS.md 明确要求时开（照 Codex）。
6. **前台还是后台**：开完马上返回，子 agent 在后台跑，主 agent 可以接着干；要等就调 `wait_agent`；子 agent 做完时叫醒主 agent（复用 P2-101）。
7. **上下文**：第一版只从空白开始，带任务那句话。fork（带整段对话、共用缓存）放到第二步。

另外几条跟着上面走，不单独问：

- 子 agent 默认用主会话的模型和思考档位，不按某家模型写特例（DESIGN 第 23 条）。
- 界面：输入框下方一行一个子 agent，能打开看记录、停掉；具体样子到时先画文字示意给用户挑。
- 钩子补 `SubagentStart`、`SubagentStop`。

## 6. 要用户定的事

1. 子 agent 跑在同一个服务端、做成子会话？（建议：是）
2. 第一版内置「只读查找」「通用」两种，嵌套 1 层、同时最多 4 个，不做自定义文件？（建议：是）
3. 批准继承主会话，批准框在主界面弹出并标明来源？（建议：是）
4. 和 P2-83 按第 5 节第 4 条分界？（建议：是）
5. 只读查找模型可自己开、会改文件的要用户明说？（建议：是）
6. 后台跑、做完叫醒主 agent？（建议：是）
7. 第一版不做 fork？（建议：是，放第二步）

定完后主控按决定拆实现任务（估计 3～4 件：子会话和协议、工具和批准、终端界面、钩子和遥测），一件做完再派下一件。

## 7. 本轮没做 / 未知

- **未知**：Claude Code 的子 agent 是否和主会话同一进程、内部怎么排队，文档没写。
- **没做**：没看 Codex v2 收件箱的实现细节、子 agent 的预算和遥测（`agent/control/budget.rs`、`spawn_telemetry.rs`）、恢复子 agent（`resume_agent`）的存盘方式。拆任务时按需再看。
- **没做**：没看 opencode、pi、omp 的子 agent；P2-88 内置工具调研里有过简述（opencode 有 `subagent`，pi 只在示例里有）。

## 出处

- [K0] canter main `093863b`。
- [K1] `docs/DESIGN.md` 第 26、208 行（P2-157 会话分叉）；`crates/canter-core/src/fork.rs`。
- [K2] `crates/canter-tools/src/lib.rs:729` `allowed_in_plan_mode`。
- [K3] `docs/DESIGN.md` 第 46、235 行（P2-101 后台叫醒）。
- [K4] `docs/DESIGN.md` 第 449 行（P2-153 钩子未实现的部分）。
- [C0] `/tmp/p2-123-src/codex`，`rust-v0.166.0`。
- [C1] `codex-rs/features/src/lib.rs:1557–1583`。
- [C2] `codex-rs/core/src/tools/handlers/multi_agents_spec.rs:166–371`。
- [C3] 同上 `:615–640`（`spawn_agent` 参数）。
- [C4] 同上 `:738` 起（`spawn_agent_tool_description`）。
- [C5] `codex-rs/core/src/agent/role.rs:360–400`。
- [C6] `codex-rs/core/src/config/mod.rs:260–270`。
- [C7] `codex-rs/core/src/agent/child_config.rs:103–188`。
- [C8] `codex-rs/tui/src/app/app_server_events.rs:794–830`（`SessionSource::SubAgent(ThreadSpawn { parent_thread_id })`）。
- [C9] `codex-rs/tui/src/slash_command.rs:144`。
- [C10] `codex-rs/core/src/tasks/review.rs:116`。
- [A0] <https://code.claude.com/docs/en/sub-agents>，2026-11-02 读取。
