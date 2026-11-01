# 任务：第二期-12 - hooks（事件钩子）

2026-11-01，canter/main 交给 canter/dev-hooks（Codex，gpt-6-astra / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.94）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：10-29 用户定从第三期插件系统 P2-21 里拿出来，进第二期：照 Claude Code 的 hooks，某个事件发生时跑用户配的命令（比如改完文件自动跑格式化）。本件只做全局钩子；事件名、配置格式、标准输入的 JSON 照 Claude Code（Codex `rust-v0.163.0` 用的是同一套），事件挑日常用得上的和 corral 以后要用的。
执行：派出

## 用户原话

- 10-29 主控为「日常用、长程任务、拿数据说话」又过了一遍，问 hooks 留第三期还是进第二期；用户：「都同意，hooks 调整到第二期。还有其他的我没想到的要做的功能吗？」
- 11-01 P2-159 项目级配置挪到第三期（用户：「同意，挪到第三期吧」）。本件先只做全局 hooks，项目里的 hooks 等第三期跟项目级配置一起做。
- 11-01 P2-83 接 corral 和 paddock 提进第二期（第二期-26），标题写着「尤其注意hook的问题」：corral 靠 agent 的钩子判断它在干活还是空闲，本件定事件时一并考虑 corral 要用的。
- 11-01 用户：「开始写 hooks 的任务文件吧。写好之后告诉我你怎么设计的。」
- 11-01 主控提了四个建议：钩子只能拦、不能放行（不跳过批准）；Stop 只通知，不能拦下让模型接着干；没信任的文件夹（受限打开）不跑钩子；钩子输出不加进给模型的内容（cairn 那种加上下文等接 cairn 时再做）。用户：「都同意。派吧。」

## 先读

- `docs/DESIGN.md`：第 8 条（协议）、第 17 条（照顾缓存命中）、第 20 条（提示词分段）、第 23 条（功能核心不绑定 DeepSeek）、「文件夹信任（P2-168）」「改配置不用重启（P2-154）」「按命令放行（P2-108）」「按文件名的读写规则（P2-167）」「沙箱（P2-123）」几节。
- `docs/调研/R49-自研agent调研.md` §6.4「做法一」里钩子那一条（corral 以后怎么用钩子；本件不做 corral 那边的事）。
- `crates/canter-server/src/config.rs`（`config.json` 怎么读、校验）和 P2-154 热加载的接线。
- `crates/canter-tools/src/permission.rs`（批准怎么判，钩子插在哪一步）；`crates/canter-core/src/session.rs` 里一轮开始、结束、打断、失败，弹批准框，工具开始和完成的地方。
- 只读参考（不搬代码）：Claude Code hooks 文档（https://docs.anthropic.com/en/docs/claude-code/hooks）；Codex `/tmp/codex-ro/codex-rs/hooks/src/schema.rs`（各事件标准输入的字段）。参考了哪里写进完成记录。

## 在哪里干活

- worktree：`../canter-worktrees/p2-153-hooks`，分支 `p2-153-hooks`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-153-hooks`。

## 要做的

1. **配置**：`~/.canter/config.json` 加 `hooks`，格式照 Claude Code `settings.json` 里的 `hooks`：

   ```json
   "hooks": {
     "PostToolUse": [
       {"matcher": "write|edit", "hooks": [{"type": "command", "command": "cargo fmt", "timeout": 60}]}
     ]
   }
   ```

   - 只认 `"type": "command"`；`timeout` 单位秒，缺省 60。
   - `matcher` 只对带工具的事件（PreToolUse、PermissionRequest、PostToolUse）有效，按工具名整名匹配（正则，`|` 分多个；空或 `*` 匹配全部）。工具名用 canter 给模型的名字：`read`、`write`、`edit`、`shell`、`ask_user`、`web_fetch`、`mcp_call`……
   - 写错时照 P2-154：整份配置沿用旧的、界面一行英文提示，不挡会话；改了下一轮生效，正在跑的这一轮用旧的。
   - 只读全局这一份，不读项目里的钩子（第三期）。

2. **事件**（名字照 Claude Code / Codex）：

   | 事件 | 什么时候 | 标准输入里除共同字段外带什么 | 钩子能做什么 |
   |---|---|---|---|
   | `SessionStart` | 会话打开（新建、恢复） | `source`：`startup` / `resume` | 只通知 |
   | `SessionEnd` | 会话关掉（客户端关、服务端停） | `reason` | 只通知 |
   | `UserPromptSubmit` | 用户的一句话开始处理、发给模型之前 | `prompt` 原文 | **能拦**：这句不发给模型、不进历史，界面一行英文写原因 |
   | `PreToolUse` | 模型调工具，canter 自己的规则（敏感路径、`fileRules`、计划模式、沙箱等）放过之后、弹批准框之前 | `tool_name`、`tool_input` | **能拦**：不弹框、不执行，原因作为这次工具的结果给模型 |
   | `PermissionRequest` | 要弹批准框、等用户点的时候 | `tool_name`、`tool_input` | 只通知（不能替用户批准或拒绝） |
   | `PostToolUse` | 工具跑完（成功或失败） | `tool_name`、`tool_input`、`tool_response` | **能反馈**：原因附在这次工具结果后面给模型（比如格式化、lint 报错）；工具已经执行，不撤销 |
   | `Stop` | 一轮正常结束 | `last_assistant_message`（最后一条回复原文） | 只通知 |
   | `StopFailure` | 一轮出错结束 | `error` | 只通知 |
   | `Interrupt` | 一轮被打断 | 无 | 只通知 |

   - 共同字段：`session_id`、`cwd`、`hook_event_name`、`permission_mode`。字段名照 Claude Code / Codex 的写法（snake_case），canter 没有的字段不硬造。
   - 一轮不管怎么开始的都算：用户送的话、排队的输入、后台命令完成叫醒、目标续跑、计划批准后开的新会话、`/new`、`/handoff`、`canter exec`。`UserPromptSubmit` 只在用户送的话上触发，自动叫醒的那一轮不触发。

3. **怎么跑**：
   - 在服务端跑，`/bin/sh -c <command>`，工作目录是会话的工作目录；事件 JSON 从标准输入给；环境变量加 `CANTER_SESSION_ID`、`CANTER_PROJECT_DIR`（会话工作目录）。
   - 不套沙箱（和 Claude Code、Codex 一样：钩子是用户自己写的命令）。会话临时目录（P2-91）不给钩子用。
   - 同一个事件匹配上的几条钩子并行跑；有一条拦就算拦。
   - 结果：退出码 0 放行；退出码 2 是拦（或 PostToolUse 的反馈），原因取标准错误。也认 Claude Code 的 JSON 写法：标准输出 `{"decision":"block","reason":"..."}`，PreToolUse 的 `hookSpecificOutput.permissionDecision: "deny"` 加 `permissionDecisionReason`。JSON 里的 `allow`、`ask` 和别的字段忽略，不报错。
   - 能拦的事件要等钩子跑完再往下走；一轮被打断时，正在跑的钩子整组杀掉。只通知的事件不让这一轮等它。
   - 一个会话的钩子慢、卡住，不能卡住别的会话、界面和服务端；超时整组杀掉。标准输出、标准错误限长，超了截断。

4. **判断不出来时**：钩子起不来、超时、退出码不是 0 也不是 2、JSON 解析不了，一律**不拦**，照常往下走，界面一行英文提示（哪个事件、哪条命令、原因）。不能因为钩子坏了把用户挡在会话外面，也不能让这一轮卡住。

5. **没信任的文件夹**（P2-168 受限打开）：不跑钩子，和不起 LSP 一样；打开时提示里写明。

6. **钩子输出不进上下文**：除了 PreToolUse 拦下的原因和 PostToolUse 的反馈（都放在那次工具结果里），钩子的输出不加进给模型的内容；系统提示词和环境一段不变（第 17、20 条）。

7. **DESIGN**：写一节「钩子（P2-153）」：上面 1～6 条，事件表，和 Claude Code、Codex 的对照（没做的：项目里的钩子、`prompt` / `agent` / `http` 类型、钩子放行、Stop 拦下让模型接着干、SessionStart / UserPromptSubmit 加上下文、PreCompact / PostCompact、Notification、Subagent 事件、Codex 的钩子信任审核）。协议有变化的照第 8 条写明和 Codex 的不同。

## 怎么算做完

- 用户原话的那个例子能用：`config.json` 配 PostToolUse、`matcher` 为 `write|edit`、跑一条格式化命令，模型改完文件后命令跑了、文件被格式化。
- 上表九个事件都在该触发的时候触发、不该触发的时候不触发，标准输入的字段对；UserPromptSubmit、PreToolUse 拦得住，PostToolUse 的反馈模型看得到；钩子起不来、超时、乱输出时照常往下走、有提示；没信任的文件夹不跑。
- 验证只做这些：先写针对性的测试（假模型服务端、隔离的 HOME 和 canter 状态目录，钩子命令写成测试里的 shell 脚本），确认因为现在没有钩子而失败，再实现。**改了哪个包，就跑那个包的全部测试；改了给模型的内容（工具结果）的，`canter-server`、`canter` 两个包也全跑**（`cargo nextest run -p <包>`），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。不跑全套，不调真实模型。

## 不要做

- 不做项目里的钩子、`prompt` / `agent` / `http` 类型的钩子、钩子放行（`allow` 跳过批准）、Stop 拦下让模型接着干、钩子输出加进上下文、`/hooks` 界面、钩子信任审核。不加上面没写的事件。
- 不为 corral 写特例，代码和文档里不出现 corral（DESIGN 第 1 条）；会话带上客户端环境变量是 P2-83 的事，本件不做。
- 钩子不能放宽 canter 自己的任何限制（敏感路径、`fileRules`、计划模式、审查轮、沙箱、批准档位）。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）。
- 实现、测试和辅助命令不用 Python（也不用 Ruby 等别的脚本语言），只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录和临时目录。测试不调真实模型接口。
- 界面上的文字用英文。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-153-hooks` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：实现全局 command hooks 的配置校验与轮边界热加载、九个事件和共同 stdin 字段。命令使用会话 cwd、服务端环境及两个 CANTER 标识变量，在独立进程组中并行执行；默认 60 秒、stdout/stderr 各保留 32 KiB，超时／取消清理进程组。可拦事件、PostToolUse 反馈及通知事件分别按任务语义处理，坏钩子放行并发英文 warning。PreToolUse 放在既有规则之后、批准之前，并在等待后重新检查权限；ask_user 保持原有提问流程，submit_plan 自身的批准框也通知 PermissionRequest。受限会话不运行 hooks，终端和 exec 的开启提示已更新。
- 输入与协议：被拦原话不写模型历史、不发送、恢复后不回放；插话用新增的 InputRemovalReason::Blocked 移除，不因被拦的晚到插话再发一次模型请求。放行后才发布 userMessage；TurnCompleted 只回填此前为空的 input，保留原协议“不覆盖已有初始输入”的行为。终端可从延后发布的 userMessage 恢复撤回草稿，涵盖另一终端及重连。JSON Schema / TypeScript 已重新生成，DESIGN 第 8 条及「钩子（P2-153）」已写配置、事件、限制和对照。
- 验证：最先写服务端格式化回归，确认未实现时文件仍为 unformatted、测试因缺少 hooks 失败，再实现至通过。后续生命周期／拦截测试先确认缺少事件时失败；晚到插话误续跑、计划批准漏通知、放行输入的快照／撤回分别有针对性 RED→GREEN。最终 `cargo nextest run -p canter-core -p canter-server -p canter-protocol -p canter-tui -p canter --no-fail-fast --status-level fail --final-status-level fail`：1087 通过，23 按既有配置跳过；五个包覆盖所有修改包，也包含工具结果受影响的 server / canter。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。没有跑工作区全套，没有调用真实模型（0 次）。新增服务端测试使用隔离 HOME、状态目录、临时 socket／端口、假模型和 shell 命令，覆盖热加载回退、正常／失败／打断／关闭／恢复、队列／插话、目标续跑／后台叫醒、权限与受限状态、并行、限长、超时和子进程清理。
- 拿主意的地方：主控 11-01 明确选 A，允许排队事件留存，但被拦输入不能进模型历史或恢复回放；优先不留可见空轮，如要大改启动流程可退回 A。采用已授权的空轮退路：现有配置交接等待、Esc、关闭、撤回、exec 都依赖 active turn 和完成信号；先登记不含输入的可取消轮，在锁外运行预检，放行后记录用户消息，拦下则显示原因并完成空轮，避免另建启动／取消协议。插话沿用 InputRemoved，新原因 blocked 已按要求记入 DESIGN。没有其它待主控决定的事。
- 参考：只读参考 https://code.claude.com/docs/en/hooks（任务给的 Anthropic 链接重定向至此）及 `/tmp/codex-ro/codex-rs/hooks/src/schema.rs`，版本 `rust-v0.163.0`；只采用事件名、配置骨架和字段约定，没有搬代码。permission_mode 报告 canter 自己的实际权限档位，差异已说明。
- 没做的事：任务排除的项目 hooks、其它钩子类型、钩子放行、Stop 控制续跑、输出注入上下文、额外事件、信任审核和 /hooks 界面均未做；没有新增客户端环境转发，没有改其它仓库，没有安装、合并 main 或推送。
- 踩过的坑：将交互工具直接接入通用批准判断会给 ask_user 多弹一次批准框，包级回归发现后保留原交互许可；模型已经结束时，仅凭仍有插话决定续跑会在全部插话被拦后多发空请求，改为只在实际消费了输入后续跑。延后用户消息暴露了旧 reducer 不回填 input、撤回草稿只读轮开始输入的问题，补了对应回归；回填必须仅作用于空 input，不能破坏旧完成事件保留原输入的契约。测试夹具的关闭／回答路由、ask_user 参数和重连后先绘制一帧的要求已按现有接口修正；一次文案替换误碰普通 LSP 禁用错误的断言，已恢复，不改变其行为。

## 返工 1 记录

- 做了什么：按 11-01 主控转达的用户决定恢复 main 的审查轮交互。AskUser、SubmitPlan 在 permission() 中先走原交互许可，不再被 review_denied 提前拒绝；PreToolUse 保留，submit_plan 自身的计划批准框仍通知 PermissionRequest。新增服务端回归验证审查轮在 alwaysAsk 下只弹提问框、能够接收回答，未开计划模式的 submit_plan 返回与 main 原说明逐字相同，两个工具的 PreToolUse 均触发。
- 停机上限：选择 3 秒，已写入 DESIGN。关会话和停服务端时，已有通知与 SessionEnd 共用这个预算；到期同时取消剩余通知任务，再等待任务退出，确保命令的进程组清理已执行。沿用服务端并行关闭会话，不按通知数量或会话数量累加等待，不改每条命令正常运行时的 timeout。两个关闭回归各配卡住的 Stop、PermissionRequest、Interrupt、SessionEnd，验证关闭在 3 秒预算加 1 秒 HTTP／进程收尾余量内完成，shell 和 sleep 子进程均已被杀。
- 验证：先运行三个新增回归，确认审查提问框缺失、两条关闭路径超过预算而失败，再作最小修复，三个回归全部通过。`cargo nextest run -p canter-core -p canter-server -p canter --no-fail-fast` 最终 470 通过、23 按既有配置跳过；覆盖所有改动包及要求的 server / canter。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。所有命令均前台等完，测试使用隔离 HOME／状态目录、假模型和 shell 钩子，真实模型调用 0 次，未跑工作区全套。
- 拿主意的地方：审查意见 1 采用用户指定的 a；意见 2 的统一短上限按授权定为 3 秒。SessionEnd 继续只在显式关闭或服务端关闭会话时触发，终端脱离不触发。没有待主控决定的事。
- 没做的事：审查意见 3～7 均未改；原 UserPromptSubmit 的 A／空轮退路继续保留。没有新增协议字段或依赖，没有安装、合并 main、推送或改其它仓库。
- 踩过的坑：首次包级运行中，既有 `startup_failure_returns_the_log_error_without_waiting_for_the_deadline` 触及其 3 秒时限；该用例缺少配置，尚未进入会话／钩子流程。未改代码或断言，单独复跑通过，随后三个包完整复跑也全部通过；不将这次超时认定为已确认的实现缺陷。

## 用户追加记录

- 依据：11-01 用户新增需求，不计返工；指定英文提示已按 DESIGN 第 20 条授权。
- 做了什么：成功 write／edit 后复用工具已校验的写入路径，仅在非受限且有匹配 PostToolUse 命令时读取该文件的钩子前内容；全部钩子结束后比较字节内容，或确认文件已删除。不论钩子退出 0、2、其它错误，观测到变化就在本次工具结果最后追加 `<path> was changed by a PostToolUse hook after this call; read it again before editing it.`，路径沿用本次工具结果中的文件名。提示排在已有钩子反馈及 LSP 诊断后面，不放入系统提示词、环境段或钩子 stdin 的原工具结果。
- 保持原样：不检查其它文件；未配置、未匹配、匹配组没有命令或受限模式时不新增文件读取；内容没变不追加观察提示。没有调整此前 UserPromptSubmit、审查轮交互、SessionEnd 触发范围或 3 秒通知关闭预算，没有新增协议、依赖、配置项。
- 验证：先给既有格式化测试加末尾提示断言，确认因为缺少提示失败，再实现至通过。新增回归覆盖 write／edit 的等长改写、删除、钩子成功／反馈／失败，覆盖同内容重写、只改时间、只改其它文件、未匹配、空命令组；无钩子时逐字断言 main 的原始工具结果，并验证原请求前缀保持不变。四个针对性测试通过；`cargo nextest run -p canter-core -p canter-server -p canter --no-fail-fast --status-level fail --final-status-level fail`：473 通过、23 按既有配置跳过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。使用隔离 HOME／状态目录、假模型和 shell 钩子，真实模型调用 0 次，所有命令前台等完，未跑工作区全套。
- 拿主意的地方：只对本次成功落盘的 write／edit 观察，使用实际文件内容作前后比较；不靠时间戳判断，不检查整个目录。没有待主控决定的事。
- 没做的事：没有自动替模型重读文件，没有把文件内容或新增说明写进提示词前缀，没有安装、合并 main、推送或修改其它仓库。
- 踩过的坑：没有。
