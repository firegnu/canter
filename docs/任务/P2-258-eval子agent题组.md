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
- 主会话最后的回答原文：`docs/任务/P2-258-第九次回答.md`。
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
- 什么时候退出：插话该发的都发了（或判定不发），所有子 agent 都结束，主会话也处理完了它们的报告、空闲了。第 1 轮没开子 agent 时，照样发第 1～4 句，第 5 句不发，主会话空闲就退出，不能一直等。另设一个总时限（主控提议 30 分钟），到了就退出，结果里记一笔「超时」。
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
  2. `dispatch-turn-quick`：主会话第 1 轮 60 秒内结束（第九次 15 秒；60 是主控提议）；
  3. `no-wait-in-chat`：插话开的那几轮里没调 `wait_agent`；
  4. `answer-facts`：主会话最后一条回复里，上表 6 条事实都提到了。中英文、`10 秒`/`10s`/`10000 ms`/`10_000` 这类写法都要认；怎么判写在题目注释里。`docs/任务/P2-258-第九次回答.md` 要能过；
  5. 只记数：第 1 轮用了几秒、子 agent 请求次数合计、子 agent 被拦命令数合计、主会话从第 2 轮到最后一轮之前读文件和跑命令的次数、第 5 句发没发。
- 「谎称报告到了」（子 agent 报告还没送到，主会话就说到了）这一版不写成检查，主控跑完人工看。
- README 题目组列表加一行。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控用 `subagent` 跑 3 次（DeepSeek flash），结果表用户看过觉得能用；每次的主会话回复主控人工看一遍有没有谎称报告到了，记进结果表后面。老题照旧：主控用合并后的版本重出已提交的结果表，和原来的一字不差。
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
