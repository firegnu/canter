# 任务：交互会话里 wait_agent 不真等

2026-11-10，canter/main 交给 canter/dev-wait-agent-no-block（Codex，`gpt-6-astra` / xhigh）。交叉审查交 Claude Code（`opus[1m]` / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.86）、要、碰要害）
执行：派出
类型：功能变更
依据：P2-258 验收（`eval/results/subagent-2026-11-10.md`）测出主会话派出子 agent 后有时自己调 `wait_agent` 干等，等的时候用户插不上话。现在只有「子 agent 报告叫醒」的那一轮有代码兜底，其余轮次只靠工具说明。本轮把兜底扩到交互会话里用户开的轮次；`canter exec` 照旧要等。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-08（P2-248 测「主动开子 agent」）：用户更在乎派出后能接着聊，「派出后结束这一轮、不调 wait_agent，结果自己回来叫醒」。
- 11-10（看完 P2-258 三次跑的结果）：「那这次跑了这四趟。要改什么呢？」主控：canter 让 `wait_agent` 在交互会话里不真等，子 agent 回来本来就会叫醒主会话，不等也不丢结果；`canter exec` 照旧要等。用户：「所以说就是改这两处。不用记进248了？」「可以，一件件来。」
- 11-10：「能修好吗？我的理解是不要跑三次就行了。跑一次通了就行了。」主控：改的是代码，模型调了也等不了；自动测试覆盖派出那一轮和用户发消息的轮次，主控真跑一次。

## 背景

- P2-258 三次里两次干等：第 2 次派出子 agent 的第 1 轮，主会话自己调了两次 `wait_agent`，260 秒才结束；第 1 次用户插话「好的，继续等另外一个subagent回来」那一轮调了 `wait_agent`，等了约 2 分钟。
- 现有兜底：`crates/canter-core/src/agent_tools.rs` 约 308～313 行，交互会话、且这一轮是报告叫醒的（`active.auto_wake`）时，`wait_agent` 不等，直接回 `WOKEN_WAIT`（约 222 行）。
- 子 agent 交报告会叫醒主会话；用户不输入时最多连续叫醒 `auto_wake_limit` 次（默认 10，用户输入重置；0 是关掉自动叫醒），见 `docs/DESIGN.md` 约 358 行。

## 先读

- `crates/canter-core/src/agent_tools.rs`：`WOKEN_WAIT`、`execute` 里 `woken` 那一段、`wait_agent` 的工具说明（约 75 行）和相关测试（约 470～490 行）
- `crates/canter-core/src/background.rs`：报告怎么叫醒主会话、`auto_wake_limit`
- `crates/canter-core/src/goal.rs`：`/goal` 自动续跑开的轮次
- `docs/DESIGN.md` 子 agent 一节（搜 `wait_agent`、P2-248）

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/wait-agent-no-block`，分支 `wait-agent-no-block`（已从 main 建好）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/wait-agent-no-block`。

## 要做的

- 交互会话里，用户开的轮次（用户直接发消息、插话、排队，包括派出子 agent 的那一轮）和报告叫醒的轮次，调 `wait_agent` 都立刻返回，不等。返回给模型一句英文：子 agent 还在跑，报告回来会叫醒你，先回复用户、结束这一轮。已经结束的子 agent，照旧返回它们的状态和最后回复（不用等就有的结果照给）。
- 照旧要等的：`canter exec`（不会被叫醒）；自动叫醒关掉（`auto_wake_limit` 为 0）的会话，报告叫醒不了主会话，等是唯一拿到结果的办法。
- `/goal` 自动续跑开的轮次：先读代码弄清楚，这种轮次结束时子 agent 还在跑，续跑会不会马上开下一轮。如果会，`wait_agent` 在续跑轮次不等就会让模型原地空转，这种轮次照旧要等；如果续跑会等子 agent，照用户开的轮次处理。看不清楚就停下问。
- `wait_agent` 的工具说明照新行为改，写清交互会话里会立刻返回、报告会叫醒你；`canter exec` 的那句不变。
- `docs/DESIGN.md` 子 agent 一节写上新规则和理由（先改 DESIGN 再改代码）。

## 怎么算做完

- 验收（用户 11-10）：「跑一次通了就行了」。主控用 `subagent` 题真跑一次（DeepSeek flash），`dispatch-turn-quick`、`no-wait-in-chat` 都过。
- 验证只做这些：
  - 先写测试确认改前失败：交互会话里用户开的一轮（派出子 agent 后、子 agent 还在跑）调 `wait_agent`，立刻返回、不等；
  - 照旧要等的情况各一条测试：`canter exec`、`auto_wake_limit` 为 0，以及按你读 `/goal` 的结论该等的那种；
  - 已结束的子 agent 立刻拿得到最后回复；
  - 原有的子 agent、`wait_agent`、`canter exec` 测试照过；工具说明快照照新说明更新；
  - `cargo nextest run -p canter-core -p canter-server`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `spawn_agent` 的说明和系统提示词；不改报告叫醒、`auto_wake_limit` 的行为；不改 `canter exec` 的行为。
- 不按模型、按服务商写分支：所有模型一样处理。
- 不改 `eval/`。不读、不改 `~/.canter`、`~/.canter-eval`，不调真实模型接口。
- 不用 Python。不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的进程用记下的 PID。
- 碰到要拿主意的事（包括 `/goal` 续跑看不清楚），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。
- 不合并到 main，不推送。只在 `wait-agent-no-block` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 主控补充（11-10，开工后加）

用户 11-10 同意（主控提：P2-270 改工具说明，提示词缓存反正冷一次，把 P2-248 记着的 rg 那句一起加）：「加。」

- `shell` 的工具说明（`crates/canter-tools/src/lib.rs` 约 263 行）加一句英文：rg 默认就递归搜目录，它的 `-r` 是 `--replace`，不是递归（意思照这句，措辞你定，越短越好）。只读子 agent 被拦时的提示（约 895 行）已经有这句，不改。
- 验证：工具说明快照照新说明更新，不另加测试。
- P2-248、P2-268 记的这件由本件做掉。

## 完成记录

- 做了什么：先在 DESIGN 子 agent 一节写明新规则，再扩展 `wait_agent`。交互用户轮（直接输入、排队、插话及派出当轮）和报告叫醒轮，在 `auto_wake_limit > 0` 时只取一次状态与可用结果，有仍在运行的子 agent 就附英文提示先回复用户、结束本轮。核心设置内部 `no_wait` 标记（模型参数不能设置），服务端复用原来的校验、状态读取、最后回复及终态结果确认，只跳过等结果的循环；工具说明和三份请求快照同步更新。
- 验证了什么：先加真实会话／工具路径回归，保持子 agent 运行并请求等待 600000 ms。未改实现时，`interactive_dispatch_wait_returns_while_the_child_is_running` 因 2 秒内没有返回而失败；最小实现后同一测试通过。新增 6 项测试覆盖派出当轮、直接／排队／插话、exec、关闭叫醒、goal 自动续跑和已完成子 agent 的最后回复；原报告叫醒测试保留并更新提示断言。相关 9 项先通过，随后 `cargo nextest run -p canter-core -p canter-server` 为 716 passed / 28 skipped；`cargo nextest run -p canter --test exec --test server -E 'binary(exec) or test(mcp::no_mcp_request_matches_pre_search_bytes)'` 为 23 passed / 61 skipped。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。所有 Cargo 命令使用任务指定的独立编译目录，命令均在前台等到退出。
- 拿主意的地方：`runner` 在轮末调用 `schedule_goal`，后者约 10 毫秒后安排续跑，`next_goal` 不检查子 agent 是否还在运行；因此按任务给定分支，保留 `active.goal_continuation` 轮次的等待，避免空转。普通用户轮不因会话设过 goal 就保留等待。无待主控决定的实现问题。
- 没做的事：没有调用真实模型（0 次），没有改 `eval/`，没有读取或修改用户的 canter 状态目录，没有修改 `spawn_agent` 说明、系统提示、报告叫醒或目标调度。没有跑工作区全套测试，没有再派 agent，没有合并或推送；交叉审查及一次 DeepSeek flash 的 `subagent` 真实验收由主控执行。
- 踩过的坑：首次编译测试替身时把 `Message::ToolResult` 字段误写为 `call_id`，改为实际的 `tool_call_id` 后重新运行，取得上述等待超时的有效 RED；编译错误没有当成 RED。初次检索用了不存在的路径和无匹配的 zsh 通配符，随后用 `rg` 定位到实际文件。没有其它实现返工或环境异常。
- 主控补充（11-10）已完成：按主仓库任务文件的「主控补充」，shell 工具说明新增 `rg searches directories recursively by default; -r means --replace, not recursion.`，同步四份快照，`cargo nextest run -p canter-tools`（243 passed / 22 skipped）、受影响的三项请求快照测试、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过，未新增测试、未改只读子 agent 的拦截提示，所有命令均使用指定编译目录并在前台跑完，无新增坑或待主控决定事项。

## 主控审查

- diff 动了 `canter-core` 的 `agent_tools.rs`（判定、`INTERACTIVE_WAIT`、`wait_agent` 说明）、`canter-server` 的 `agent_sessions.rs`（`no_wait` 跳过等待循环）、`canter-tools` 的 `shell` 说明、四份请求快照、DESIGN 和测试；没碰 `spawn_agent` 说明、系统提示词、叫醒和 exec 的行为。
- `/goal` 续跑：开发读出续跑在轮末 10 毫秒后开下一轮、不看子 agent，续跑轮照旧等，主控同意。
- 交叉审查（Claude Code `opus[1m]` xhigh）：可以合并，0 必须改；建议改 2 条（不等时不计入重复保护、说明措辞）记 HANDOFF，可以不改 3 条同意不改，见 `P2-270-交叉审查.md`。
- 合并前全套（主控）：`cargo nextest run --all-targets` 2097 个全过（50 跳过），`cargo test --doc --workspace` 没有失败。
- 派 Codex（`gpt-6-astra` xhigh）做：一次做完；开工后用户同意加的 rg 那句，主控在它交完主体后转去补上。合并时任务文件两边都在末尾追加（主控补充 / 完成记录），两节都保留。
- 验收（用户 11-10「跑一次通了就行了」）：等用户 `canter upgrade` 后，主控用 `subagent` 题真跑一次。

结果：一次过
