# 任务：Shift+Tab 轮换权限模式

2026-10-25，canter/main 交给 canter/dev-shifttab（Claude Code，opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：常规（置信 0.97）、要、碰要害）
类型：功能变更
依据：第二期加塞的第 20 件（`docs/任务/P2-0-第二期需求单.md`）。本轮在终端输入框里加 Shift+Tab，按顺序轮换三种权限模式，像 Claude Code 那样随手切；`/mode` 保持不变。不加新的权限模式，不做 Claude Code `/permissions` 那种允许／拒绝规则。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-1
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-25：「现在加塞一个任务，就是现在我想有类似claude code的bypass或者codex的yolo那种功能。这样权限默认全过，理解我意思吗？」
- 主控说明已有 `--yolo` 和 `/mode allow-all`，用户：「我要的是类似claude code的permission斜杠命令，这样我可以随时改。permission命令带出一个各种权限的选择。理解我意思吧？」
- 主控说明 `/mode` 就是这个选择框，并问是只换叫法／按法，还是要 Claude Code `/permissions` 那种规则；用户：「1，加 Shift+Tab 轮换，写任务文件给我看」。

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/任务/P2-1-斜杠命令.md`「要做的」和「主控追加」：`/mode` 怎么换模式、选择框和按键为什么要按「按键发生时屏幕上那一帧」判断。
- `docs/任务/P1-14-批准框换回输入框时的Enter.md`「要做的」（保护期）。
- `crates/canter-tui/src/commands.rs`（`MODES`、`mode`、`help`）、`app.rs`（按键分发的先后：批准框、选择框、命令列表、输入框；`info_line`）。
- `crates/canter-tools/README.md`「权限与执行器边界」那张表（三种模式各管什么）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-20-shift-tab`，分支 `p2-20-shift-tab`（已从 main 建好）。
- 只动 `crates/canter-tui/`、`crates/canter/tests/tui.rs`（如需真服务端测试）、本任务文件末尾的完成记录。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-20-shift-tab`。

## 要做的

- 输入框里按 Shift+Tab（crossterm 的 `BackTab`），按这个顺序换到下一个模式：Always ask → Ask for execution → Allow all → Always ask。换模式和 `/mode <名字>` 走同一条路（同样的改设置请求），回答中途也能换，下一个动作按新模式走。
- 往下轮的起点是**按键时屏幕上显示的模式**：连按两下、而服务端还没回话、屏幕还没变时，两下都换到同一个模式，不会一下跳两格。
- 只在输入框能用时生效：批准框、提问框、选择框（包括 `/mode` 的三选一）、命令列表开着时，Shift+Tab 不换模式；批准框、提问框换回输入框的 300 ms 保护期里也不换（这时就是 P1-14 防误按的窗口）。
- 换完以后信息行显示新模式（现在就有）；不另弹提示。
- `/help` 里加一行说明这个快捷键（比如 `Shift+Tab   Cycle permission mode`），样式和命令行一致。

## 怎么算做完

- 输入框里按 Shift+Tab，权限模式按 Always ask → Ask for execution → Allow all → Always ask 轮换，信息行显示新模式；`/mode` 照旧能用。（主控按用户 10-25「1，加 Shift+Tab 轮换」拟，用户看过任务文件后：「验收照你拟的，派吧」）
- 验证只做这些：
  - 针对验收写测试，先写、确认因为还没有快捷键而失败：按三下 Shift+Tab，每一下发出的改设置请求和信息行依次是三种模式，第四下回到 Always ask。
  - 边角用例：屏幕没变时连按两下只换一格；批准框、提问框、选择框、命令列表开着时不换；换回输入框的 300 ms 内不换；回答中途能换。
  - 做完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的 `CARGO_TARGET_DIR`）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `/mode` 的行为、三种模式的含义和权限规则；不加新模式、不做允许／拒绝规则；不改协议、核心、服务端。
- 不改默认权限模式，不加配置项。
- 不调真实模型；测试用隔离的 HOME、`CANTER_HOME` 和临时 socket，不碰用户正在跑的服务端（别读、别停它的 pid）。
- 不升级依赖；要加新依赖的，停下来问。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事（包括有的终端收不到 Shift+Tab、要动上面范围以外的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-20-shift-tab` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
