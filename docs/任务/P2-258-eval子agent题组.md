# 任务：eval 子 agent 题组

2026-11-09，canter/main 交给 canter/dev-eval-subagent（canter，`canter --yolo`，用 canter 服务端配置的模型）。交叉审查交 Codex（`gpt-6-astra` xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.38）、要、碰要害）
执行：派出
类型：功能变更
依据：P2-151 方案的第二组题（第一组长程 P2-257、第三组记忆 P2-259/P2-264 已做完）。把 P2-248 里手工测了 9 次的「主动开子 agent」调研题写成能自动重放、自动判分的 eval 题。只改 `eval/`，不改 canter 本身，老题照旧。
依赖：P2-257、P2-264
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（对 P2-151 讲解页 6 个问题）：「1. 同意 2. 就这三组题（但是你要说清楚这三组题的来源）3. 我要在场，但是我不会干预 4. 先只用deepseek flash 5. ……6. 放eval」
- 11-09（问 258 要做什么、工作量多大，主控提了三点建议：4 句插话「主会话派完那一轮结束后，等上一句回完再发下一句」；「谎称报告到了」第一版先人工看；要交叉审查）：「照你的建议，写任务单给我看.」
- 11-09（看过任务单，讨论 `rg -rn` 先不修、258 做完后统计被拦；主控请用户定验收、60 秒和 30 分钟、要不要单记 `rg -r` 被拦次数，并建议加）：「所以还是要在eval上继续叠加eval，对吧？另外接受你的建议」

## 三组题的来源

| 题组 | 来源 | 状态 |
|---|---|---|
| 长程 | P2-81 那件：canter-test-ts 连做 12 件、三句插话、压缩线 60000 | P2-257 做完（`eval/tasks/long.toml`） |
| 记忆 | P2-256（Anthropic 的 agent 记忆文章）并进 P2-151 第 8 节：连着做几件活、比记忆保留和清空 | P2-259（项目记忆）、P2-264（全局记忆）做完 |
| 子 agent | 本件。P2-248 11-08 测「主动开子 agent」时，同一个调研问题用户手工测了 9 次（DeepSeek 和 GPT），主控每次手工翻记录；判什么见 `docs/任务/P2-151-讲解.html` 第 7 节 | 本件 |

## 背景：第九次测试（照它写题）

- 会话目录 `/private/tmp`（没有 AGENTS.md、CLAUDE.md），源码在 `/tmp/codex`（`57a808a`）、`/tmp/pi-mono`（`085c84e`）。
- 开跑提示：`对比 /tmp/codex 和 /tmp/pi-mono 这两个项目是怎么处理工具调用超时的：各自在哪些文件、默认超时多少、超时后怎么告诉模型。`
- 主会话第 1 轮开了 2 个只读子 agent，15 秒结束这一轮。子 agent 在跑时用户发了 4 句：`好的，谢谢`、`/tmp/codex 的最新提交是哪天的？`、`你觉得这两个项目哪个代码更好读？`、`等他们调查回来吧`，每句 3～23 秒回完，都没调 `wait_agent`。第一个子 agent 的报告叫醒主会话，3 秒说了要点并结束；用户再发 `好的，继续等另外一个subagent回来`；第二个报告叫醒后主会话合并两份（32 秒）。派出到合并约 6 分 40 秒。
- 数字：主会话 19 次请求、0 次读文件、15 条命令；两个子 agent 92 次、44 次请求，命令被拦 6 次、4 次（大多是 `rg -rn`、通配符写在中间段，P2-268 已决定不单独修）。P2-248 记录里写的「子 agent 被拦 0 次」只看了主会话，以这里为准。
- 主会话最后的回答原文：`eval/tests/fixtures/subagent-answer-9.md`。
- 源码里核对过的事实（判分用）：

  | 事实 | 位置 |
  |---|---|
  | codex shell 默认超时 10 秒（`10_000` ms） | `codex-rs/core/src/exec.rs:61` |
  | codex MCP 工具调用默认 300 秒 | `codex-rs/codex-mcp/src/rmcp_client.rs:123` |
  | codex 超时后退出码报成 124 | `codex-rs/core/src/unified_exec/process.rs:217` |
  | pi 的 bash 没有默认超时 | `packages/coding-agent/src/core/tools/bash.ts` |
  | pi 超时告诉模型 `Command timed out after N seconds` | `bash.ts:407` |
  | pi MCP 工具调用默认 60 秒 | `packages/coding-agent/src/extensions/mcp/runtime.ts:49` |

## 先读

- `eval/README.md`（整份不长，重点「跑一轮」「多个会话的题」「题目文件」「结果表」）
- `eval/tasks/memory-global.toml`（`[[repos]]` 的写法）
- `eval/src/task.rs`、`eval/src/steer.rs`、`eval/src/server.rs`、`eval/src/prepare.rs`、`eval/src/grade.rs`、`eval/src/collect.rs`、`eval/src/report.rs`、`eval/src/paths.rs`
- `docs/任务/P2-151-讲解.html` 第 7 节
- 协议：`crates/canter-protocol/src/commands.rs` 里 `POST /v1/sessions/{id}/input`（主会话空闲时发就开一轮）、`crates/canter-protocol/src/stats.rs`（`subAgents`）。只读，不改。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-subagent`，分支 `eval-subagent`（已从 main 建好）。只动 `eval/`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-subagent`。

## 要做的

### 1. 主会话在一个空目录里做，源码是固定提交的 worktree

- 两份源码各写成题目的一个仓库（`[[repos]]`，`local.json` 的键 `codex`、`pi-mono`，起点 `57a808a`、`085c84e`，没有准备命令）。
- 主会话的目录照第九次的 `/tmp`：不在任何仓库里、没有 AGENTS.md 和 CLAUDE.md。主控的建议：用这次跑放别的仓库 worktree 的那个目录（`worktrees/<轮次>/@repos/<次>/`，里面正好只有 `codex/`、`pi-mono/`）。题目怎么写（比如允许不要主仓库，或者会话写一个目录选项）你定，写进 README；老题不变。
- 开跑提示和插话里能写占位符，换成某个仓库 worktree 的绝对路径；写法你定，写进 README。
- `prepare`、`trust`、`steer`（找主会话）、`finish`、`collect`、`clean` 都要认这个目录：`prepare` 打印的 `--cwd` 是它；`trust` 后开会话不弹信任框；`clean` 照旧只删这次建的。

### 2. 新的插话方式：主会话空闲时按顺序发

- 题目里写一串插话，每句写什么时候发。只要两种时机：
  - 主会话第 1 轮结束以后：主会话空闲时发一句，等它这一轮结束再发下一句（第 1～4 句）；
  - 某个子 agent 的报告叫醒过主会话以后：前面的都发完、至少有一个报告叫醒过主会话、主会话空闲时发（第 5 句）。这时所有子 agent 都已经回来了，就不发，记下来没发。
- 发的时候主会话正忙（比如报告刚好叫醒它），等它空闲再发，不用 steer 模式（按 `POST /v1/sessions/{id}/input` 空闲时开一轮的办法发）。
- 什么时候退出：插话该发的都发了（或判定不发），所有子 agent 都结束，主会话也处理完了它们的报告、空闲了。第 1 轮没开子 agent 时，照样发第 1～4 句，第 5 句不发，主会话空闲就退出，不能一直等。另设一个总时限 30 分钟，到了就退出，结果里记一笔「超时」。
- 每句发出的时间、对应哪一轮、没发的原因，记进这次跑的目录；重开 `steer` 不重复发。
- 现有的 `after_commits` 插话照旧，长程题不受影响。README 写清这道题的跑法：`steer` 退出以后再 `corral wait` 到 idle。

### 3. 判分能读会话数据

- 判分命令要能拿到：主会话每一轮的开始和结束时间、这一轮是谁开的（开跑提示、第几句插话、子 agent 报告叫醒）、每一轮调了哪些工具；主会话开的每个子 agent 的请求次数、被拦的命令数、开始结束时间；主会话最后一条回复的原文。
- 怎么给（eval 工具整理成文件放在 `RUN_DIR` 下、环境变量指路径之类）你定，写进 README；判分命令用 shell 和 `jq` 能写出来就行。
- 结果表：这道题的花费、用时、请求要能看出子 agent 那部分（算进总数还是另列，你定）。没有子 agent 的老题，表一字不差。

### 4. 新题 `eval/tasks/subagent.toml`

- `id = "subagent"`，名字写明是子 agent 调研题。`reasoning`、`auto_compact_tokens` 同 `long.toml`。
- 开跑提示和 5 句插话照上面「背景」原文，只把 `/tmp/codex`、`/tmp/pi-mono` 换成占位符。
- 判分（第 1～4 条判过不过，其余只记数）：
  1. `spawned-subagents`：主会话第 1 轮开了至少 1 个子 agent；
  2. `dispatch-turn-quick`：主会话第 1 轮 60 秒内结束（第九次 15 秒）；
  3. `no-wait-in-chat`：插话开的那几轮里没调 `wait_agent`；
  4. `answer-facts`：主会话最后一条回复里，上表 6 条事实都提到了。中英文、`10 秒`/`10s`/`10000 ms`/`10_000` 这类写法都要认；怎么判写在题目注释里。`eval/tests/fixtures/subagent-answer-9.md` 要能过；
  5. 只记数：第 1 轮用了几秒、子 agent 请求次数合计、子 agent 被拦命令数合计、其中 `rg` 带 `-r`（含 `-rn` 这类合写）被拦的条数（P2-248 以后在 shell 说明里加「rg 默认递归，`-r` 是替换」，拿它看效果）、主会话从第 2 轮到最后一轮之前读文件和跑命令的次数、第 5 句发没发。
- 「谎称报告到了」（子 agent 报告还没送到，主会话就说到了）这一版不写成检查，主控跑完人工看。
- README 题目组列表加一行。

## 怎么算做完

- 验收（主控提议，用户 11-09 同意）：主控用 `subagent` 跑 3 次（DeepSeek flash），结果表用户看过觉得能用；每次的主会话回复主控人工看一遍有没有谎称报告到了，记进结果表后面。老题照旧：主控用合并后的版本重出已提交的结果表，和原来的一字不差。
- 验证只做这些：
  - 插话：用假 unix socket 服务端和造的事件测：第 1～4 句按顺序、每句等上一轮结束再发；主会话正忙时等它空闲；第 5 句在报告叫醒以后才发，子 agent 都回来了就不发；没开子 agent 不卡住；总时限到了退出；重开不重发；`after_commits` 的老测试照过；
  - 目录：两份源码的 worktree 建在主会话目录下、检出各自起点；`--cwd`、`trust`、找会话认这个目录；`clean` 删掉这次建的、别的不动；
  - 判分：读题测试（插话的句数和时机、占位符换成了路径）；每条新检查一个过、一个不过的例子，`answer-facts` 用第九次回答过、删掉任意一条事实不过；
  - 老题：现有测试照过，没有子 agent 的结果表输出不变；
  - `cargo nextest run -p canter-eval`、`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `crates/` 下的任何代码；canter 的协议或事件里缺判分要的东西，停下来问。
- 不改 `eval/tasks/` 下已有的题目文件，不改 `eval/results/` 下已有的表。
- 不按模型、按服务商写分支：题目和工具里不写死 DeepSeek 或别的模型，换模型照样能跑。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话），不碰 `/tmp/codex`、`/tmp/pi-mono`，不起真的 canter 服务端，不调真实模型接口。测试只用临时目录、造的仓库和假服务端。
- 不用 Python。不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的进程用记下的 PID。
- 碰到要拿主意的事（包括判不出一轮是谁开的、报告叫醒的那一轮看不出来），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。
- 不合并到 main，不推送。只在 `eval-subagent` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

做了什么：

- 目录：题目可以不写 `repo`、`base`（`Task.repo/base` 变成 `Option`，两个一起写或一起省），只写 `[[repos]]`。这种题的主会话目录是 `<worktrees>/<轮次>/@repos/<次>/`，`prepare` 建它、打印 `--cwd`，各仓库的 worktree 直接建在它下面；`trust` 把它和每个 worktree 都设为信任；`steer` 按这个目录找会话；`finish` 不写 HEAD（没有主仓库）；`collect`、`grade` 在它里面跑不带 `repo` 的检查；`clean` 删完 worktree 后把它也删掉（里面还有别的东西时留着并打 note）。没有新增 `State` 字段：无主仓库时 `repo_name` 为空，`worktree` 就是会话目录。
- 占位符 `{repo:<名字>}`：`prepare` 渲染开跑提示，`steer` 渲染插话，换成那个仓库 worktree 的绝对路径；名字不在这次跑里就报错。
- `[[chat]]`：`after = "turn"` 等上一轮（第一句等开跑提示那轮）结束、主会话空闲再发；`after = "report"` 要见过 `clientName == "background"` 且 input 为空的轮次，等空闲再发，所有子 agent 都回来了就跳过并记原因，没开子 agent 就记「没有子 agent 的报告」。发送用 `POST /v1/sessions/{id}/input`（`mode: queue`），主会话忙就等。进度写在 `$RUN_DIR/steers.json`（`sent`、`chats`、`timedOut`），重开不重发。退出条件：插话都解决、主会话空闲、子 agent 都结束、再空闲满一小段（`poll*3`，最少 150ms）；会话关闭也退出；默认总时限 1800 秒，`--timeout` 可改，到了记 `timedOut: true` 后正常返回。`[[chat]]` 与 `[[steers]]` 互斥，只能用于单会话题；`after_commits` 照旧。
- 会话数据：有 `[[chat]]` 的题，`grade` 先跑 `canter stats <主会话 ID> --json`，把 `session-data.json` 写到 `$RUN_DIR`（失败只打 note，不中断），里面有每轮的起止、`openedBy`（`prompt`/`steer{index}`/`report`/`other`）、按工具名计数的 `tools`、`finalReply`、`subAgents[]`（`requestCount`、`blockedCommands`、`blockedRgReplace`、起止、token、花费）、`chats[]`、`timedOut`。老题不写这个文件。
- 结果表：只有这次跑有子 agent 时才多一节「## 子 agent」——先一行主会话与子 agent 的用时/请求/花费分开算，再每个子 agent 一行。`RunResult.sub_agents` 空时序列化也省掉，没子 agent 的老题表不变。
- 新题 `eval/tasks/subagent.toml`：4 条判分 + 6 条只记数，判分命令只读 `$RUN_DIR/session-data.json`；README 加题目行、跑法、`[[chat]]` 和占位符写法、会话数据表、结果表那一节。

验证了什么：

- `cargo nextest run -p canter-eval`：55 个全过（原 46 个 + 9 个新的），新增测试覆盖：插话按顺序等上一轮、忙时等空闲、report 句在叫醒后发、子 agent 都回来时跳过、没开子 agent 不卡住、总时限退出、`after_commits` 老行为；无主仓库的 prepare/trust/finish/grade/clean 全流程；每条判分各有一个过和一个不过的例子（`answer-facts` 用第九次回答过，删 6 条事实的任一条都不过）；`grade` 为 chat 题写 `session-data.json`；`collect`、`report` 的子 agent 数和表样；老题的 report 测试照样过。
- `cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 全过。

拿主意的地方：

- 无主仓库时不加 `State` 字段，用空 `repo_name` 表示，`main_repo()` 返回 `Option`，减少对现有测试构造的改动；会话目录由 `session_dir_path` 统一定义。
- 判分数据由 `grade` 调真实 `canter stats` 整理，而不是另存事件解析；判分命令只看一份 JSON，shell + jq 就够。
- 结果表把子 agent 另列一节（主表口径不改），总数行同时给出「主 / 子 / 共」，因为子 agent 和主会话的用时、花费是并行的，混进主表会改老题口径。
- TOML 字段叫 `[[chat]]`（Rust 侧 `chats` + `rename = "chat"`），和 `[[steers]]` 一样按用途命名；子 agent 的请求次数直接用 stats 的同名字段 `requestCount`。
- `finish` 的「已记录」判断从 HEAD 文件改成 `memory.json`，否则无主仓库的会话没有 HEAD 就永远能重复记录；有仓库的会话两个文件仍然一起写。
- `rg -r` 的识别看 `rg` 后的短旗标里有没有 `r`（`-rn`、`-nr` 都算），`--replace` 不算（题目里说的是 `-r`）。

没做的事：

- 「谎称报告到了」没有写成检查，按任务要求留给主控人工看。
- `no-wait-in-chat` 在一轮插话都没开时是空过（`add // 0 == 0`）；任务没让加前置条件，没自己加，请主控定要不要补一条「第 2 轮起至少有一轮插话」。
- 没有调真实模型、没有起真的 canter 服务端；3 次 DeepSeek flash 的验收跑留给主控。
- 没有跑全套 `cargo nextest run --all-targets` 和 `cargo test --doc`（按约定只在合并前由主控跑）。

踩过的坑：

- `[[repos]]` 后面写 `prompt` 会被当成那个仓库的字段，题目加载直接报未知字段；把 `prompt` 挪到 `[[repos]]` 之前。
- serde 默认把 `chats` 写成 `chats`，题目里 `[[chat]]` 认不出来；加 `rename = "chat"`。
- 假服务端的 `folderTrust` 响应固定带 `path`，`trust` 用它去重，第三次请求被当成重复跳过，测试少一次请求；假响应改成不带 `path`，回落到请求里的目录。
- 测试造假事件的时间戳比 `steers.json` 里的 `sentAt` 小，按「发之后开始的轮次」匹配不上；改成按插话原文匹配轮次、每轮只用一次。
- `match_turns` 只在发送或跳过时落盘，补上 `turnId` 那次没保存；改成返回「有没有变化」再写。
- `SubAgentRow` 字段叫 `requests`，序列化出来是 `requests`，判分和 README 写的是 `requestCount`，测试读成 0；字段改名 `request_count`。
- `answer-facts` 的「删一条事实」测试里，第九次回答同一事实有几种写法（`没有默认超时`、`无默认`、`无超时`、大小写不同的 `command timed out after`），只替换一种仍然过；测试改成把各种写法都换掉，检查本身没放松。
- macOS 临时目录是符号链接，`pwd -P` 和 `$WORKTREE` 字面不同；测试检查改成两边都 `pwd -P` 再比。
- 自己用 `rg -rn 'pattern' path` 想搜内容，`-r` 被当成替换参数，输出全被改了；搜内容用 `rg -n`。

## 返工记录（第 1 次，按交叉审查的主控判断）

改了审查意见里「交回开发方改」的：第 1、2、5、7、8 条，以及 `no-wait-in-chat` 要求至少有一轮插话。第 3、4、6、9、10 条按主控判断不改。

做了什么：

- 第 1 条（`eval/src/steer.rs`）：`ready(Report)` 先看有没有子 agent、是不是都结束了。没有就跳过「没有子 agent 的报告」；都结束就跳过「所有子 agent 都回来了」，不再要求出现过 background 叫醒轮；只有还有子 agent 在跑、确实要发第 5 句时才看 `report_woke`。退出仍要求插话都解决、主会话空闲、子 agent 都结束并空闲满 grace，报告晚到也会等它处理完。
- 第 2 条（`eval/src/session_data.rs`、`eval/src/steer.rs`、`eval/tasks/subagent.toml`）：`turns[]` 新增 `spawnedAgents`，只数 spawn 结果里非空 ID 的个数；`spawned-subagents` 改看它，失败的 spawn 仍记在 `tools.spawn_agent` 里。`View` 登记子 agent 时丢掉空 ID，失败调用的占位项不再挡住跳过和退出。
- 第 5 条（`eval/src/steer.rs`）：chat 题的总 deadline 在找主会话之前建立，找会话、发送、轮询共用；找会话耗尽预算就把 `timedOut: true` 落盘后退出。`after_commits` 的老路径仍然没有这个 deadline。
- 第 7 条（`eval/tasks/subagent.toml`）：数字都加左边界（`110 秒` 不算 10 秒），`124` 用独立数字匹配（`1124` 不算）；毫秒认 `10 000 ms`、裸 `10_000`、`10000 ms`、`10000ms`（`millis` 里把秒数乘 1000 再找，另认空格千分位和下划线常量）。按主控判断没有按项目、按句子绑定数值。
- 第 8 条（`eval/src/report.rs`）：汇总行的总花费改成主会话加子 agent 的和，unknown 也合并；用时的「共」改成「各会话相加」，说明是并行会话时长直接相加。
- `no-wait-in-chat`（`eval/tasks/subagent.toml`）：要求至少有一轮 `openedBy.kind == "steer"`，没有就不过；仍要求这些轮里没调 `wait_agent`。
- README 和题目注释同步：`spawnedAgents`、总花费口径、总时限包括等主会话出现。

验证了什么：

- 先加测试、确认每条在改之前失败（`--no-fail-fast` 跑出 7 个失败）：
  - 报告在插话轮里处理完、没有 background 轮：`chat_steering_skips_the_report_line_when_children_finish_without_a_wake` 原来等到超时、第 5 句没记跳过；
  - 失败 spawn 的空 ID 占位：`chat_steering_ignores_a_failed_spawn_placeholder` 原来一直等、记超时；
  - 找会话时总时限：`chat_steering_stops_at_the_time_limit_before_the_session_appears` 原来不返回，被外层 3 秒超时打断；
  - `counts_only_spawns_that_returned_an_agent` 原来数成 0；
  - `answer_facts_checks_number_boundaries_and_millisecond_forms` 原来把 `110 秒` 当成 10 秒；
  - `collect_and_report_show_sub_agents` 原来总额是 `$0.05（主 $0.05 · 子 $0.01）`、用时写「共」；
  - `subagent_checks_read_the_session_data` 里失败 spawn 能过 `spawned-subagents`；`no-wait-in-chat` 的新断言在旧命令下单独跑过一次，失败在「没有插话轮」这一条。
- 改完 `cargo nextest run -p canter-eval` 60 个全过（原 55 + 5 个新测试）；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 全过。

拿主意的地方：

- `spawnedAgents` 记成计数而不是 ID 列表：判分只要「这轮有没有真开出来」，README 也只加一列；要看是谁另有 `subAgents[]`。
- `View` 只按事件里的 ID 建子 agent，空 ID 直接忽略；没有改「结束不可逆」的累计逻辑（第 3 条按主控判断不改）。
- 找会话超时返回 `None`，由 `steer` 统一落盘「超时」，和 `chat` 内部的超时出口一致；chat 分支的 deadline 用 `expect`，因为只有 chat 题会建它。
- 总数行措辞用「各会话相加」而不是「累计」，避免被读成墙钟时间。
- 事实检查用几个小的 shell 函数（`has_number`、`seconds`、`millis`），题目注释写清认哪些写法；没有引入 jq 之外的依赖。

没做的事：

- 第 3、4、6、9、10 条按主控判断不改（子 agent 被 `send_input` 唤醒、发送后进度没落盘的重复发送、重判分沿用旧数据、`rg -r` 分词、原文回退匹配范围）。
- 第 7 条没有按项目、按句子绑定数值，照主控判断只加边界、补齐写法。
- 没有调真实模型、没有起真 canter 服务端，也没有跑全套测试。

踩过的坑：

- 给 `spawned-subagents` 换字段后，测试 fixture 没给 `spawnedAgents` 时 jq 的 `// 0` 让真样本也判不过；fixture 补上第 1 轮的 2 个成功 spawn。
- `nextest` 默认遇错就取消，第一次只看得到 3 个失败；加 `--no-fail-fast` 才收齐 7 个。
- 新测试第一次改动把原来的 `chat_steering_stops_at_the_time_limit` 函数名换掉了，发现后改回原名，另起一个「找会话就超时」的测试。
- clippy 不让对静态字符串用 `format!`，直接传 `&str`。

## 主控审查

- diff 只动 `eval/` 和本文件；已有题目文件、`eval/results/` 没改。4 张已提交的结果表（baseline、memory、memory2、memglobal）用合并前的最终版本重出，一字不差。「报告叫醒」认 `clientName == "background"` 且没有输入，和第九次测试的真实记录对得上。
- 交叉审查（Codex `gpt-6-astra` xhigh）第一轮 7 条必须改、3 条建议改。主控交回 5 条（第 1、2、5、7、8 条，第 8 条从建议改升为必须改，第 7 条不按项目绑定数值）加 `no-wait-in-chat` 至少一轮插话；第 3、4、6、9、10 条只在少见操作或故障下才碰到，记建议改不挡合并（详见 `P2-258-交叉审查.md`「主控判断」）。复核：可以合并，0 条必须改，1 条建议改（毫秒的逗号千分位 `10,000 ms` 不认）。
- 对方拿的主意（不加 `State` 字段、`grade` 生成一份 `session-data.json`、子 agent 另列一节、`[[chat]]` 命名、`finish` 改用 `memory.json` 作完成标记、只数短 `-r`）都同意。
- 合并前全套（主控）：`cargo nextest run --all-targets` 2090 个全过（50 跳过），`cargo test --doc --workspace` 没有失败。
- 派 canter 做（DeepSeek flash，第一轮 195 次请求）。返工多的原因：插话、子 agent 报告和主会话忙闲撞在一起的时序难；任务单写「报告叫醒过主会话以后」默认报告一定另开一轮，也没让它读 canter 送报告、开子 agent 的代码。
- 验收（主控跑 3 次 DeepSeek flash，用户看结果表）还没做。

结果：返工 1 次

## 验收（11-10，主控跑）

- 跑了 3 次（DeepSeek flash，canter `78ef712`），结果表 `eval/results/subagent-2026-11-10.md`：全过 1/3，平均 2.7/4，共约 $0.22。3 次都没有谎称报告到了。
- 测出的问题：派出后有时干等子 agent（第 2 次派出那一轮等了 260 秒；第 1 次第 5 句那一轮等了约 2 分钟）。现在只有报告叫醒的那一轮有代码兜底。
- eval 自己的两处毛病：`answer-facts` 只看最后一条回复（第 2 次答案在第 1 轮给了，被判缺）；子 agent 用时算到会话关掉。
- 用户 11-10：「可以，一件件来。」先修 eval 这两处（P2-269），再让交互会话里 `wait_agent` 不真等（P2-270），之后重跑。
