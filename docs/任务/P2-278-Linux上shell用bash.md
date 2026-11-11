# 任务：Linux 上 shell 工具用 bash 跑命令

2026-11-11，canter/main 交给 canter/dev-linux-bash（canter，`canter --yolo`，用 canter 服务端配置的模型，不分强度；用户点名 canter）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.53）、交叉审查拿不准（安全隐私 0.76）、影响面拿不准；主控判断：不改沙箱的挂载、权限和放行规则，只换沙箱里外启动的解释器，涉及安全的两点——不按 PATH 找、不带 `BASH_ENV`——写进要做的，审查时逐条核对，所以不算碰要害）
类型：功能变更
依据：P2-274 Terminal-Bench 遥测：shell 工具用 `/bin/sh -c`，Linux 上它通常是 dash，模型写的 `time cmd`、bash 专有写法报错（2848 条命令里约 22 次：`time: not found` 17、`Syntax error` 4、`Bad substitution` 1），模型下一步都自己改过来，只多花 token。macOS 的 `/bin/sh` 本来就是 bash，所以只有 Linux 受影响。本轮只换 Linux 上 shell 工具的解释器，不改护栏和只读检查规则。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-11（主控列出「shell 在 Linux 用 bash、`rg -r` 那句说明、护栏拦错的两例」后）：「我觉得可以立刻做，你觉得如何？」
- 主控答：`rg -r` 已由 P2-270 做掉；护栏建议不动；bash 这件建议当小任务派出、带测试和审查。用户看过本任务单（含下面四条验收）后：「派吧。如果不是什么碰要害的活你觉得canter可以吧？」

## 先读

- `AGENTS.md` 或 `CLAUDE.md`（仓库根）的「规矩」一节。
- `docs/DESIGN.md`「Linux 支持（P2-160）」一节（5090 上怎么编、怎么跑）。
- `crates/canter-tools/src/executor.rs` 启动 shell 的那段（约第 230～255 行），`crates/canter-tools/src/sandbox_linux.rs` 结尾 `"--", "/bin/sh"` 那段，`crates/canter-tools/src/lib.rs` shell 工具的说明文字（约第 265 行）。
- `docs/任务/P2-272-Linux沙箱.md` 的「在哪里干活」和完成记录：5090 上同步代码、独立编译目录、只用 `~/canter-linux/` 的做法照它。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-278-linux-bash`，分支 `p2-278-linux-bash`（已从 main 建好）。只动 `crates/canter-tools/`，以及确实因为工具说明变化要跟着改的测试快照。
- Mac 上编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-278-linux-bash`。
- 5090（`ssh my-5090d-host`，Ubuntu 24.04，有 `/bin/bash`、`/usr/bin/bwrap`）：只用 `~/canter-linux/` 下面。代码放 `~/canter-linux/p2-278-linux-bash`，编译目录 `~/canter-linux/.target/p2-278-linux-bash`，同步办法同 P2-272。

## 要做的

1. **Linux**：shell 工具（前台、后台、Linux 沙箱 bubblewrap 里）有 bash 时用 bash 跑命令，没有时照旧用 `/bin/sh`（Alpine 这类没装 bash 的环境）。bash 只从固定位置找（`/bin/bash`、`/usr/bin/bash`），不按 PATH 找，免得项目里的同名程序冒充。
2. **行为和现在一样，只换解释器**：不读用户的启动文件；`BASH_ENV` 不带进去（dash 本来不认它，带进去会在每条命令前执行别的脚本）。其它环境变量、工作目录、进程组、超时、输出截断、后台命令、Ctrl+B 都不变。
3. **工具说明写对**：shell 工具说明里「Run a command with /bin/sh -c」按实际用的写（Linux 用 bash 时写 bash）。一台机器上一次服务端启动内固定不变，不在会话中途变，免得缓存变冷。
4. **macOS 不变**：仍用 `/bin/sh`，说明文字逐字不变（Mac 上的缓存不受影响）。
5. **不动的**：看护进程（guardian）自己的固定脚本、hooks 的 `/bin/sh`、终端状态栏脚本，都照旧用 `/bin/sh`；只读检查和危险命令检查已经按 bash 语法解析（tree-sitter-bash，认得进程替换），不改规则。

## 怎么算做完

- 用户确认的验收：
  1. Linux 上 shell 工具跑 `time true`、`[[ 1 -eq 1 ]] && echo ok`、`echo $'a\tb'` 都成功；沙箱开着和 `--yolo` 都一样。
  2. 没有 bash 的 Linux 照旧能跑命令，说明写 `/bin/sh`。
  3. macOS 上工具说明和行为不变（对比改前改后的说明文字）。
  4. `BASH_ENV` 指向的脚本不会被执行。
- 验证只做这些：
  - 先在 5090 上写一条因为现在用 dash 而失败的测试（比如 shell 工具跑 `time true` 或 `[[ … ]]` 失败），确认是因为这个原因失败，改完变过。
  - 验收第 2 条照 P2-272 的做法，用仅 `cfg(test)` 可用的注入点模拟「找不到 bash」，不动机器上的 bash；第 4 条写一条测试。
  - 5090 上：`cargo test -p canter-tools`（没装 nextest，不要装），沙箱开着和关着的相关测试都在里面；`cargo clippy --all-targets -- -D warnings`。
  - Mac 上：`cargo nextest run -p canter-tools`；工具说明若有别的 crate 的测试在比对，那个 crate 也跑全；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 跑完 5090 上不留 `canter serve` 和测试进程。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不按 PATH 找 bash，不用用户的 `$SHELL`（zsh、fish 等语法不同，检查器不认）。
- 不改护栏和只读检查的规则；不放宽 `rm` 关键路径检查。
- 不改 hooks、guardian、状态栏用的 shell。
- 不在核心逻辑里按模型或服务商分支（DESIGN 第 23 条）。
- 不调真实模型接口；不读、不改两台机器上的 `~/.canter`。5090 上只用 `~/canter-linux/`，不用 sudo、不装系统包。不用 Python。
- 不要按名字批量杀进程（`pkill -f canter` 这类）：主控和用户的 canter 进程命令行里都带着 canter，一条命令能把它们全杀掉。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如某处必须改沙箱参数才能用 bash、说明文字在测试里有别处依赖拿不准），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送到 `origin`（推到 5090 上自己的目录可以）。只在 `p2-278-linux-bash` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
