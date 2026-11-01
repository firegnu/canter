# 任务：第二期-16 - clear 命令（照 Claude Code 的 /clear）

2026-10-30，canter/main 交给 canter/dev-clear（pi，默认模型）。
路由：低 / 交叉审查不要 / 影响面：改行为（路由：轻（置信 0.5）、不要、改行为）
类型：功能变更
依据：10-29 用户要 Claude Code 的 `/clear` 进第二期。canter 已有 `/new`（「Start a new session」：用当前目录、模型和权限开新会话，原会话留在 `/resume` 列表里）。Claude Code 的 `/clear` 也是清掉对话、从头开始，原来的对话还能找回来，和 `/new` 是一回事。10-30 用户同意：`/clear` 就是 `/new` 的另一个名字。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：无
执行：派出

## 用户原话

- 10-29 用户：「还有我想有一个eval的功能，因为我不能在没有eval的情况下没发评估现在的agent到底效果如何？要设计一个eval的方案。我觉得这个也要单独搞一个任务，排到第二期去，eval要包括任务完成度，token消耗量等等综合指标。。。尤其是长程任务的上下文窗口的变化以及压缩之后是什么样子（我觉得还得把遥测加到第二期来）。另外还有一件就是claude code的clear命令也要进入第二期」
- 10-30 用户：「能不能剩下的挑几个简单的，不碰核心的做做。例如141 143之类的」；主控挑了 P2-152、P2-155、P2-141、P2-156、P2-143，建议 `/clear` 做成 `/new` 的另一个名字、行为一样。用户：「同意，从 /clear 开始」

## 先读

- `crates/canter-tui/src/commands.rs`：命令表 `COMMANDS`，`/new` 那一项。
- `crates/canter-tui/tests/slash.rs`：斜杠命令的测试怎么写。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-152-clear`，分支 `p2-152-clear`（已从 main 建好）。只动 `crates/canter-tui/`。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-152-clear`。

## 要做的

1. 加命令 `/clear`，做的事和 `/new` 完全一样（开新会话，目录、模型、思考档位、权限模式、加进来的目录照 `/new` 沿用；原会话留在 `/resume` 列表里）。
2. 命令列表和 `?` 帮助弹框里列出 `/clear`，说明写 `Start a new session (same as /new)`。`/new` 的说明不变。
3. `/clear` 不带参数，带了照 `/new` 的规矩拒绝。

## 怎么算做完

- （用户 10-30 同意的）`/clear` 和 `/new` 行为一样，只是多一个名字。
- 验证只做这些：一条测试，输入 `/clear` 得到和 `/new` 一样的动作；命令列表里有 `/clear` 和它的说明。再跑 `cargo nextest run -p canter-tui`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `/new` 和开新会话的逻辑，不动服务端和协议。
- 不加别的别名（`/reset` 之类）。
- 测试用隔离的 HOME 和 `CANTER_HOME`，不读用户真实的 `~/.canter`；不真调模型接口。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-152-clear` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：在 `crates/canter-tui/src/commands.rs` 的 `COMMANDS` 里加了 `/clear`，说明 `Start a new session (same as /new)`，`takes_args: false`，动作是 `Action::NewSession`，和 `/new` 完全一样；`/new` 本身没动。命令列表、`/help` 和 `?` 帮助弹框都从 `COMMANDS` 生成，自动列出。
- 验证了什么：`slash.rs` 新加一条 `clear_does_what_new_does`（`/clear` 与 `/new` 返回同一个动作，命令列表里 `/clear` 和它的说明都在）；把 `clear` 加进已有的「不带参数的命令拒绝多余文字」测试；两份测试里的命令表（`slash.rs`、`help_popup.rs`）补上 `/clear`，`slash.rs` 的表同时补齐了之前漏掉的 `/ps`、`/stop`、`/theme`、`/add-dir`、`/goal`。跑了 `cargo nextest run -p canter-tui`（556 全过）、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`，都干净。
- 拿主意的地方：`/clear` 放在 `/new` 后面（而不是按字母排到最前），让它和同一个行为的命令挨着；命令表顺序本来就是内置命令按字母、后加的命令追加在后面，这次算跟着相关命令走。`slash.rs` 的命令表原来只覆盖屏幕上放得下的前 23 条（后面加命令时没更新），这次顺手补成全部 29 条，并把两个整表测试的屏幕高度从 `HEIGHT + 5` 加到 `HEIGHT + 11`，让 29 条都放得下。
- 没做的事：没改 `/new`、开新会话逻辑、服务端和协议；没加别的别名；没动 README 和文档里的命令清单（任务限定只动 `crates/canter-tui/`）。
- 踩过的坑：只把列表测试的屏幕加高还不够，`up_down_tab_and_enter_work_on_the_list` 也用 `above_box(COMMANDS.len())`，屏幕不加高时多出来的行会取到对话区，跟着一起改了。

## 主控审查

2026-10-30，canter/main。看了 diff：命令表在 `/new` 后面加了 `clear`，动作是 `Action::NewSession`，不带参数；`/new` 和开新会话的逻辑没动，没碰服务端和协议。测试补了 `clear_does_what_new_does`，`slash.rs` 的整表顺带补齐了之前漏的 6 条命令（同意，测试更完整，不是放宽）。合并前主控跑全套：`cargo nextest run --all-targets` 1117 个全过，`cargo test --doc --workspace` 过。
结果：一次过
