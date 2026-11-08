# 任务：第二期 - 主会话等子 agent 时，用户一发消息就停止等待、先处理用户的话

2026-11-08 写好，canter/main 交给 canter/dev-wait-interrupt（Codex，`gpt-6-astra` / high，难活对比条件用户 11-05 定）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害；并发 0.94、核心规则 0.77）。
类型：功能变更
依据：P2-248「主动开只读子 agent 并行调研」的后续。本轮只让 `wait_agent` 遇到用户新消息时停止等待，并补几句工具说明；不动 Esc、不动 Sub-agents 框。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
对比样本：第 12 件，Kimi 7 件 / Codex 4 件，这件 Codex（主控按轮流提议，用户可改）。
待用户：定下面「要用户定的」两条，确认交给谁。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-08 用户（P2-248 测试时）：「我更加在乎的是主agent派出去之后，我可以继续和他对话，token数之类的我不是很在乎。」
- 11-08 用户实测后：「另外第一个subagent结束之后，我还是无法介入对话」
- 主控提议「你一发新消息，`wait_agent` 马上结束，告诉模型先回复用户，子 agent 照常跑；同时在说明里补两句」，用户：「可以，先写第一件的任务文件」

## 现状（主控查过的，main `68ab815`）

- **等待**：`wait_agent` 在服务端 `crates/canter-server/src/agent_sessions.rs` 的 `wait_agents()`（约 143 行起）。只有这几种情况会结束：某个子 agent 结束（完成、失败、关闭、被中断），等满 `timeoutMs`（最长 600000 ms），或者这一轮被取消（Esc）。用户发来的消息不会让它结束。
- **用户在一轮进行中发消息**（P2-97，`crates/canter-core/src/session.rs` 约 1450～1495 行收输入）：
  - Enter 发 `mode: queue`：放进排队，这一轮结束后开新一轮处理。
  - Tab 发 `mode: steer`：放进 `steers`，下一次请求模型前由 `consume_steering`（`runner.rs` 约 379 行）放进这一轮。
  - 所以主会话在 `wait_agent` 里时，两种消息都要等等待结束才能被模型看到，最长 10 分钟。按 Esc 能打断等待，但会把在跑的子 agent 一起停掉（HANDOFF 已记，本件不改）。
- **子 agent 结果怎么回到主会话**（P2-101、P2-178，DESIGN「子 agent」一节）：交互主会话空闲时，子 agent 结束会在 2 秒合并窗口后叫醒主会话开新一轮；主会话忙时，结果附到下一次模型请求。`wait_agent` 拿到某个子 agent 的终态结果后，那个结果不再另发通知。`canter exec` 单轮运行不叫醒。
- **实测**（11-08，DeepSeek v4.1 flash，`/tmp` 下对比两个项目，两个 explorer）：
  - 派出那一轮 14～24 秒结束，用户接着问别的几秒就答了。
  - 但第一个子 agent 结束、叫醒主会话后，模型想「用户要的是对比，没有另一个结果答不了」，调了 `wait_agent` 等第二个，等了 5 分钟以上，这期间用户插不上话。
  - 另一次用户说「好的，谢谢」，模型没等，但回复说「两个 explorer 的报告到了，我直接把对照结论发给你」，其实一个都没到。
- **相关说明文字**：`crates/canter-core/src/agent_tools.rs` 的 `spawn_agent` 两份说明（内置的 `definitions()` 和带自定义角色的 `definitions_for_roles()`）、`wait_agent` 说明；系统提示词 `crates/canter-core/src/prompt/core.txt` 第 20 行 `[spawn_agent]`。这些文字原样出现在请求样本里：`crates/canter/tests/fixtures/no_mcp_request.json`、`crates/canter-core/tests/sandbox-disabled-request.json`、`crates/canter-core/tests/fixtures/p2-223-main-requests.jsonl`。

## 要用户定的（主控推荐都选 A）

1. **哪种消息让等待停下**：
   - **A（推荐）**：Enter 和 Tab 都算。
     - Tab（插话）：`wait_agent` 马上返回，这条消息照插话的规矩进这一轮，模型在同一次请求里同时看到等待结果和用户的话。
     - Enter（排队）：`wait_agent` 马上返回，这一轮在记下等待结果后直接结束，不再请求模型；排队的消息照现在的规矩开新一轮。模型在新一轮里能看到等待结果和用户的话。这样不改「Enter 是开新一轮」的意思。
   - B：只有 Tab 算，Enter 照现在等到等待结束。改动小，但大家默认按 Enter，多数时候还是插不上话。
2. **提前停下时，`wait_agent` 返回什么**：
   - **A（推荐）**：照常返回每个子 agent 当前的状态和已有结果，末尾加一句英文：`Stopped waiting because the user sent a new message. Reply to the user first. The sub-agents keep running, and their reports will wake you.` 已经拿到终态结果的子 agent，照现在的规矩不再另发通知；还在跑的，结束后照 P2-101 送回、叫醒。
   - B：返回一条只说「用户发了新消息」的短结果，不带状态。模型要再查一次才知道谁跑完了。

## 要做的

1. 交互会话里，主会话在 `wait_agent` 中，用户发来新消息（照第 1 条定的范围）时，`wait_agent` 马上结束，照第 1、2 条定的处理。子 agent 一个都不停。
2. 下面这些照旧，不受影响：
   - `canter exec` 单轮运行：`wait_agent` 照旧等，不因任何输入提前停。
   - 主会话没在 `wait_agent` 里时，Enter 和 Tab 的行为。
   - Esc：照旧打断这一轮（连带停子 agent 那条本件不改）。
   - `/btw`、斜杠命令、`!命令` 不算用户新消息。
   - 子 agent 自己的会话（它们没有 `wait_agent`）。
3. 工具说明和系统提示词补两点，英文，意思如下，措辞你定：
   - 被某个子 agent 的报告叫醒、你开的别的子 agent 还在跑时，不要等：用一句话告诉用户哪个回来了、说了什么要点，然后结束这一轮；剩下的回来会再叫醒你，最后一个到了再合并。
   - 没看到子 agent 的报告，不要说它到了。
   - 加在 `wait_agent` 说明、`spawn_agent` 两份说明，以及 `core.txt` 的 `[spawn_agent]` 那一行。只在有这个工具时出现，照现有写法。请求样本照新文字更新。
4. `docs/DESIGN.md` 子 agent 一节写清新行为（哪种消息让等待停下、停下后返回什么、`exec` 不变），注明 P2-254 和用户原话。

## 怎么算做完

- 交互会话里，主会话在 `wait_agent` 中、子 agent 还在跑时，用户发一条消息，模型马上处理这条消息（照第 1 条定的范围），不用等子 agent 结束；子 agent 继续跑，跑完后照常把结果送回主会话。
- `canter exec` 下 `wait_agent` 行为不变。
- 主会话没在等待时，Enter、Tab、Esc 的行为不变。
- 第 3 条的说明加上了，请求样本更新了。
- 验证只做这些：
  - 针对这件活的测试：先写测试，确认它因为「消息等到等待结束才处理」而失败，再改。至少覆盖：等待中 Enter 发消息；等待中 Tab 发消息（第 1 条选 A 时）；`exec` 下不提前停；提前停下后，还在跑的子 agent 结束时照常送回、叫醒。
  - 改了哪个包，就跑那个包的全部测试：`cargo nextest run -p <包> --all-targets`。至少跑 `canter-core`、`canter-server`、`canter`：后两个有请求回放和样本测试，提示词或请求内容变了会在那里失败（P2-251 漏跑过 `canter`，三条回放测试坏了）。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-254-wait-interrupt`。
  - 觉得不够，在回复里说，不要自己加。

## 在哪里干活

- worktree：`../canter-worktrees/p2-254-wait-interrupt`，分支 `p2-254-wait-interrupt`（已从 main 建好）。
- 只动 `crates/canter-server`、`crates/canter-core`、上面列的三个请求样本、`docs/DESIGN.md` 和本任务文件。需要动 `canter-protocol` 或 `canter-tui` 的，先停下问。

## 不要做

- 不改 Esc 连带停子 agent 的行为（HANDOFF「已知没修的问题」里那条，用户说先不做）。
- 不改 Sub-agents 框（P2-255 另做）。
- 不改 `canter exec` 的单轮语义（P2-178），不改 P2-101 的叫醒和合并窗口，不改 P2-97 的 Enter 排队、Tab 插话规矩（第 1 条 A 里 Enter 让这一轮直接结束，是本件唯一的例外）。
- 核心逻辑不按模型名、`base_url`、请求字段或缓存行为分支，不绑定 DeepSeek；DeepSeek 只是用户实测时碰巧用的模型。
- 测试不调真实的模型接口，用假服务端回放。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录。停自己起的服务用起的时候记下的 PID。
- 碰到要拿主意的事（比如结束等待和子 agent 正好同时结束、同一时刻进来多条消息、轮次结束和排队消息开新一轮之间的先后），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-254-wait-interrupt` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
