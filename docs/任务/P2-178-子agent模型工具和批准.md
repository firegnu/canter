# 任务：第二期-13.2 - 子 agent：模型工具和批准

2026-11-02，主控按 P2-78 拆出。
2026-11-02，canter/main 交给 canter/dev-subagent-tools（Codex，gpt-6-astra / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 1.0）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
执行：派出
依据：`docs/调研/P2-78-子agent调研.md`；P2-177 已把子会话做成带 `parent` 的普通会话。本件给模型 4 个工具、两种子 agent、批准转到主界面、做完叫醒主 agent、打断和花费，并修掉 P2-177 交叉审查留下的几条。不做子 agent 列表界面（P2-179）、钩子和统计页面（P2-180）。
依赖：P2-177

## 用户原话

- 11-02 主控提议把 P2-78 拆成子会话和协议、模型工具和批准、终端界面、钩子和统计 4 件。用户：「先把任务开了吧。之后咱们按照顺序一个个讨论。讨论完一件做一件」
- 11-02 主控给出本件设计和 5 件要定的事（中断的用 `send_input` 接着做；两种子 agent 的工具；什么时候开、开几个；批准在主界面弹出并标明来源；Esc 一起停、花费算进目标上限），用户问：「子agent其他的权限呢？」主控逐项列了权限，另提两件（权限档位跟着主会话实时变；子 agent 不读不写记忆）。用户：「都按你的建议。」

## 先读

- `docs/调研/P2-78-子agent调研.md` 第 1、5 节。
- `docs/任务/P2-177-子agent子会话和协议.md`（含完成记录、主控审查），`docs/任务/P2-177-交叉审查.md`「建议改」1～7。
- `docs/DESIGN.md`：「子 agent 的子会话（P2-177）」、第 8 条协议、P2-101 后台叫醒、P2-109 目标续跑和花费上限、P2-103 计划模式、P2-51 和第 80～91 行批准请求（`request.opened` / `request.resolved`）、P2-22 记忆、P2-91 会话临时目录、第 23 条。
- `crates/canter-core/src/agents.rs`、`crates/canter-server/src/agent_sessions.rs`（P2-177 留的接入点 `App::spawn_agent`）。
- `crates/canter-tools/src/lib.rs` 的工具定义和 `allowed_in_plan_mode`。
- 参考（只读，不搬代码）：Codex `/tmp/p2-123-src/codex/codex-rs`（`rust-v0.166.0`）的 `core/src/tools/handlers/multi_agents_spec.rs`（v1 工具和说明）、`core/src/tools/handlers/multi_agents/*.rs`、`core/src/agent/role.rs`（explorer、worker 的说明）。

## 在哪里干活

- worktree：`../canter-worktrees/p2-178-subagent-tools`，分支 `p2-178-subagent-tools`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-178-subagent-tools`。

## 要做的

### 1. 主 agent 的 4 个工具（照 Codex v1）

- `spawn_agent {task, type}`：`type` 为只读查找或通用。开一个子 agent，马上返回它的 ID 和一个短名字（给界面和批准框显示），主 agent 可以接着干。
- `wait_agent {ids, timeoutMs}`：
  - 等列出的子 agent 里任何一个做完、失败、被关或中断，或者超时就返回；
  - 返回列出的每一个的当前状态，已做完的带最终回复；
  - 默认 30 秒，最长 600 秒。
- `send_input {id, message}`：给子 agent 再发一句，让它接着做。对中断的子 agent 也用它接着跑，不另加 `resume_agent`。
- `close_agent {id}`：关掉子 agent。在跑的轮取消，它起的后台命令一起停。已做完的结果保留。
- 交回给主 agent 的最终回复有长度上限，沿用现有工具结果的截断办法。
- 工具说明（英文）写清：
  - 只读查找可以自己开，用来查代码、找资料，省主上下文；
  - 通用只在用户或 AGENTS.md 明确要求子 agent、委派或并行时开（照 Codex 原话的意思，「仔细查」「深入分析」不算）；
  - 开之前先自己列计划，挡住下一步的活自己做；
  - 开通用子 agent 时讲清它负责哪些文件，并告诉它不是一个人在改这个仓库、别改回别人的改动。

### 2. 两种子 agent

- **只读查找**：计划模式那套只读工具（`allowed_in_plan_mode`），去掉 `ask_user` 和 `submit_plan`。`mcp_search` 可以，`mcp_call` 不行。
- **通用**：全部工具，去掉 4 个子 agent 工具和 `ask_user`。所以只有 1 层嵌套。
- 两种都可以用 `update_plan`，记的是子 agent 自己的待办。
- 同一个主会话同时在跑的子 agent 最多 4 个。超了，`spawn_agent` 报错（英文），不开。
- 主会话在计划模式或审查轮里，只能开只读查找；开通用就报错（英文）。
- 子 agent 的系统提示词：
  - 加一小段（英文）：说明它是子 agent、最终回复会交给主 agent、不要问用户、有问题写进最终回复；
  - 不放记忆那段（不读也不写记忆）；
  - AGENTS.md 和技能照常。

### 3. 权限

- **权限档位跟着主会话现在的档位实时变**，主会话改了，子 agent 下一次检查就按新的。这一点改 P2-177「开的那一刻抄一份」的做法，DESIGN 跟着改。
- 其余照 P2-177：
  - 放行规则实时读全局 `rules.json`；
  - 沙箱、工作目录、新增目录、受限打开、环境变量照开的那一刻；
  - 「本会话允许」不继承主会话的；在子 agent 批准框上点「本会话允许」，只对这个子 agent 有效；
  - 「以后都允许」照现在写进全局规则。
- 每个子 agent 有自己的会话临时目录。

### 4. 批准转到主界面

- 子 agent 要批准时，主会话的事件流里要能看到：哪个子 agent、哪条请求、请求内容。请求解决后同样通知。
- 客户端用子会话现有的解决请求接口回答。字段和事件名参考 Codex（它把子线程的请求带着线程 ID 发给同一个客户端），和 Codex 不一样的地方写进 DESIGN 第 8 条。
- 终端：沿用现有批准框，加一行显示来源：`Requested by sub-agent <name>`。回答照常，拒绝只拒这一次，子 agent 接着跑。子 agent 列表那块界面不做（P2-179）。
- `canter exec` 没人批准时，子 agent 和主会话一样自动处理。

### 5. 做完叫醒主 agent

- 子 agent 做完、失败或中断时：
  - 主会话空闲，就叫醒它开一轮；
  - 主会话在忙，就附到它下一次模型请求里。
- 复用 P2-101 的后台叫醒和合并窗口。
- 发给模型的说明用英文，带子 agent 的 ID、名字、状态和最终回复（截断规则同上）。

### 6. 打断和花费

- 用户在主会话按 Esc（打断这一轮）：正在跑的子 agent 一起停，标成中断，不因此叫醒主会话。
- 子 agent 花的钱算进主会话目标（P2-109）的花费；到了上限，主会话怎么停，子 agent 也一起停。
- `/v1/usage`（终端 `/usage`）把子会话的花费算进去：改用包含子会话的列表（P2-177 交叉审查建议 1）。统计页面怎么归到父会话下留给 P2-180。

### 7. 修 P2-177 交叉审查留下的

- **建议 3**：启动补记中断时，单个子会话打开或写入失败，只记日志、跳过，不挡服务端启动；先用便宜的办法挑出在跑的，不要每次完整打开所有子会话。
- **建议 4**：`/v1/sessions/{id}/agents` 读到某个子会话已经不在（刚被删）时跳过这一条，不整个失败。
- **建议 5**：会话状态锁统一先父后子。回退和本件新加的 `send_input`、`wait_agent`、叫醒、Esc 连带停都按这个顺序，不能和删除、回退互相等待。
- **建议 6**：
  - 关掉一个已做完、空闲的子 agent，保留它的最终回复；
  - 子会话里的手动压缩和 `!命令` 不算「最近一轮」，不改状态和结果。
- **建议 7**：回退主会话不还原子 agent 改过的文件。写进 DESIGN，本件不处理。

### 8. DESIGN

更新「子 agent 的子会话（P2-177）」或新增「子 agent 工具和批准（P2-178）」，写上面这些规则和理由。协议差异写进第 8 条。

## 怎么算做完

- 用户定的 7 件做到：
  1. 中断的子 agent 用 `send_input` 接着做，不另加工具；
  2. 两种子 agent 的工具按第 2 节分，主会话在计划模式或审查轮只能开只读查找；
  3. 只读查找模型可自己开，通用要用户明说（写在工具说明里），同时最多 4 个，只有 1 层；
  4. 子 agent 要批准时，批准框在主界面弹出，标明来源，拒绝只拒这一次；
  5. 主会话按 Esc，在跑的子 agent 一起停；子 agent 的花费算进主会话目标的花费上限；
  6. 子 agent 的权限档位跟着主会话实时变；
  7. 子 agent 不读也不写记忆。
- 上面第 1～8 节做到。
- 验证只做这些：
  - 每条行为写针对的测试，用假模型服务端回放，不调真实模型；
  - 边角各一条：
    - 超过 4 个被拒；
    - 计划模式下开通用被拒；
    - 子 agent 拿不到子 agent 工具和 `ask_user`；
    - 只读查找写文件被拒；
    - 主会话改档位后子 agent 下一次检查按新档位；
    - 子 agent 的批准在主会话事件流可见，并能用子会话接口回答；
    - Esc 后子 agent 变中断、不叫醒；
    - 花费达到目标上限时子 agent 一起停；
    - 已做完的子 agent 被关后结果还在；
    - 启动时一个子会话坏了不挡启动；
  - 跑改动涉及的每个 crate 的测试（`cargo nextest run -p <crate>`，改了几个 crate 就跑几个）；
  - 加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不做子 agent 列表、打开子 agent 记录、停掉子 agent 的终端界面（P2-179）。终端只改批准框那一行来源。
- 不加 `SubagentStart` / `SubagentStop` 钩子，不改 `canter stats` 的统计页面（P2-180）。
- 不做 fork 式子 agent、不做用户自定义子 agent 文件、不加 `resume_agent`、不做 Codex v2 的收件箱。
- 不还原子 agent 改过的文件（回退时）。
- 界面和发给模型的文字一律英文，不出现中文。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）；子 agent 用主会话的模型和思考档位。
- 实现、测试和辅助命令不用 Python 或别的脚本语言，只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录、临时端口和临时 socket。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：停自己起的服务用起的时候记下的 PID。
- 不在 Cargo 里引用 ranch、paddock、cairn 的包，不从 Codex 搬代码（只参考）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。包括：上面的规则和现有机制冲突；要改协议里已有字段的含义；锁顺序做不到先父后子。
- 不合并到 main，不推送。只在 `p2-178-subagent-tools` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-11-02，完成于 `p2-178-subagent-tools`。

- 做了什么：接入 spawn_agent、wait_agent、send_input、close_agent，英文说明包含自主 explorer、显式授权 general、先计划、关键路径及文件归属要求；增加两种角色的工具白名单、运行时拦截、单层和最多 4 个并发、子会话独立清单和临时目录。权限档位实时跟父会话，其他执行范围沿用创建快照；子会话不装入记忆、不开放记忆读写，AGENTS.md 和技能正常装入。子请求通过父 SSE 转发、快照保留待答请求，终端显示来源并回答子接口；完成通知复用后台合并窗口，Esc 连带取消并抑制叫醒，子费用计入父目标和 `/usage`，关闭停止后台命令且保留结果。修复启动逐个恢复、列表删除竞态、父先子后的回退锁顺序、压缩和 `!` 轮不覆盖 agent 结果，并更新 DESIGN、schema 和 TypeScript 产物。
- 验证了什么：全部使用隔离目录和本地假模型，真实模型调用 0 次。覆盖四工具、任一终态返回所有状态、超时、32 KiB 截断且原结果保留、中断后继续、关闭后台命令、角色限制、计划/审查限制、实时权限、记忆拒读写、批准转发和拒绝后续跑、终端回答路由、exec 自动处理批准、交互叫醒、exec 下一次输入及重开后交付、Esc、预算、恢复失败跳过、删除竞态和原有家族操作。新增行为先运行对应失败检查再实现；补充测试复现了 explorer 压缩后误补普通计划流程指令，修复后清单和只读工具规则保持一致。
- 验证命令：已执行 `cargo nextest run -p canter-core -p canter-protocol -p canter-store -p canter-tools -p canter-server -p canter-tui --no-fail-fast`，首次 1295 项中 1290 通过，5 项旧工具名单/请求快照/上下文容量夹具需随新增工具更新；原断言保留，修正后相关检查全部通过。protocol、store、tools、tui 的完整 crate 测试在该轮全部通过；core/server 返工后完整复查 428 项全部通过，最后 explorer 压缩修复的 agent、计划模式和压缩相关 63 项回归全部通过。`cargo run -p canter-protocol --bin export_protocol`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过；命令均前台等待退出。没有跑全仓测试或文档测试。
- 拿主意的地方：按主控明确决定 A，`canter exec` 保持单轮，不因子 agent 完成自动起下一轮；结果保留到主会话下一次输入，包括 `exec --resume` 或交互打开后的输入；exec 当轮要结果用 wait_agent，交互主会话空闲时按第 5 节叫醒。该决定已写进 DESIGN。批准新增独立转发事件和 `agentRequests`，保留原 `pendingRequests` 的本会话含义；旧子会话缺角色字段按 general 恢复，未知角色按只读限制。没有剩余需要主控决定的事项。
- 没做的事：没有子 agent 列表/记录/停止界面、SubagentStart/SubagentStop 钩子、统计页面归组、用户角色文件、fork 式子 agent、收件箱或 resume_agent；回退主会话不恢复子 agent 改过的文件。只参考 Codex `rust-v0.166.0` 的 `core/src/tools/handlers/multi_agents_spec.rs`、`multi_agents/` 和 `core/src/agent/role.rs`，独立实现，没有搬代码。没有合并 main、推送、安装或重启用户服务。
- 踩过的坑：exec 单轮与自动叫醒的原设计存在冲突，先停下交主控，收到 A 后继续。新增工具增大了上下文，使两个紧贴容量边界的压缩夹具走了另一条路径；给原预期路径留出容量，并保留所有原行为断言。批准测试最初误用了 `/requests/{id}/resolve`，改回现有 `/requests/{id}`。假模型按 7 字节拆包时，单个 80 KiB SSE 帧未在夹具时限内完成，改成正常的多段流式正文，仍验证超限截断，没有缩短超时或放宽长度断言。新完成通知使旧删除测试存在待叫醒工作，改为等待该轮结束后再验证原有删除断言。

### 返工

2026-11-02，按主控审查和 `P2-178-交叉审查.md` 中用户同意的建议完成。

- 做了什么：修正 canter 的三条 MCP 测试和 `ask_user_second_option_returns_to_the_model_and_continues` 的工具清单、数量、MCP 索引及 `no_mcp_request.json`，补入四个子 agent 工具，保留顺序、描述、参数、结果和完整请求字节断言。验证新请求移除新增四工具后与原夹具逐字节相同。同步 core 的 `sandbox-disabled-request.json` 中 send_input 描述，其他字节不变。
- 结果交付：wait_agent 返回终态时按子会话 ID 和具体工作轮登记已交付；转发任务不再为该轮生成完成通知，已排队但未发给模型的通知从待发队列和上下文撤掉，并取消它的叫醒。交付与撤回存入父会话私有历史，重开后保持；已发送历史不删，其他子 agent、仍在运行的轮和后续轮次继续正常通知。撤回同步调整上下文索引和回退边界，锁顺序保持先父后子。
- 实时权限：将父会话计划模式和当前审查轮的只读状态放入实时权限快照；已经运行的 general 在每次检查、钩子返回后的重查和批准返回后都按当前状态收紧，使用 evaluate_plan，仍允许自己的 update_plan。父会话未打开时恢复其持久化档位和计划模式。DESIGN 已写明这些规则。
- 说明与决定：send_input 英文说明明确 canter exec 下先用 wait_agent 等正在运行的子 agent 停下再发，运行行为不改。继续采用主控决定 A：exec 单轮、不自动续轮，未等待的结果留到主会话下一次输入；交互空闲仍按合并窗口叫醒。建议 4 按用户决定不改，由主控记 HANDOFF；其他未授权建议不扩展。没有剩余需要主控决定的事项。
- 针对验证：先复现原四条 canter 失败。新增两条权限测试在修复前均实际写出了 pending.txt、next.txt 和 shell-write.txt，修复后全部拒绝，读取和子清单仍成功；通知测试修复前 wait 后仍留有第三条上下文消息，修复后撤回且不叫醒，重开不补发，下一子轮仍正常叫醒。另覆盖超时返回 running、混合返回终态与 running、保留其他子通知和已发送历史前缀；工具描述通过实际模型请求检查。子 agent 相关 30 条及原四条 canter 回归均通过。
- 最终验证：使用独立编译目录，前台完成 `cargo nextest run -p canter-core -p canter-protocol -p canter-store -p canter-tools -p canter-server -p canter-tui -p canter --no-fail-fast --test-threads 4`：1487 条全部通过，28 条按现有配置跳过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。测试只用隔离目录及假模型，真实模型调用 0 次；未跑全仓或文档测试，未合并、推送、安装或重启用户服务。
- 踩过的坑：首次七 crate 检查发现 core 的沙箱关闭请求夹具也包含旧 send_input 描述，补齐后保留原字节断言。默认并发完整运行曾出现升级/回退测试服务替换连接失败，以及两条 LSP 同步测试等待诊断超时；三条单独复查全部通过，再以 4 路并发完整跑七个 crate 全部通过，没有改这些实现、断言或超时。新增测试的显式 drop 未通过 clippy 的持锁跨 await 检查，改成明确的块作用域后通过。

## 主控审查

2026-11-02，canter/main。

- 第一次审查：diff 只动了该动的，「不要做」一件没做；exec 单轮和自动叫醒冲突，开发停下来问，主控定 A（exec 不自动续轮，结果留到下一次输入；交互主会话照常叫醒），写进 DESIGN。全套重跑发现 canter crate 4 条测试挂（工具清单和请求夹具没补新工具，开发没跑 `-p canter`），交回返工。
- 交叉审查（Claude Code opus[1m] / xhigh）：可以合并，必须改 0、建议改 4。用户定：返工时一并修建议 1（wait_agent 拿到结果后通知重复交付）、2（已在跑的通用子 agent 跟主会话计划模式和审查轮实时收紧）、3（只改 send_input 说明）；建议 4（父会话打开时子会话自己的目标不记账）不改，记 HANDOFF。
- 返工后复核：四条都改对，夹具没有放宽断言；新提一条建议改（主会话首个输出前按 Esc 撤回一轮，期间到的子 agent 完成通知会丢，`42d7724` 就有），记 HANDOFF。
- 合并前全套（`31cbef8`）：1504 跑、1496 过、8 挂，都是 LSP：7 条 `canter-tools lsp::sync::tests::*` 和 `reload_tests::disabling_then_enabling_lsp_still_waits_for_the_original_turn`，都是等诊断超时；文档测试过。单独复跑一次：reload 那条过，lsp::sync 仍挂 10 条。main 上跑同一组 lsp::sync 同样挂（「Waiting for LSP diagnostics timed out」），机器负载约 6.8，判定和本件无关、是现有的环境问题，记 HANDOFF。
- 结果：返工 1 次
