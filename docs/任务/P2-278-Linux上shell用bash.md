# 任务：Linux 上 shell 工具用 bash 跑命令

依据：P2-274 Terminal-Bench 遥测：shell 工具用 `/bin/sh -c`，Linux 上它通常是 dash，模型写的 `time cmd`、bash 专有写法报错（2848 条命令里约 22 次：`time: not found` 17、`Syntax error` 4、`Bad substitution` 1），模型下一步都自己改过来，只多花 token。macOS 的 `/bin/sh` 本来就是 bash，所以只有 Linux 受影响。用户 11-11 同意立刻做（P2-248 记了来源）。
执行：派出
待用户：看任务单；确认下面「主控建议的验收」

## 用户原话

- 11-11（主控列出「shell 在 Linux 用 bash、`rg -r` 那句说明、护栏拦错的两例」后）：「我觉得可以立刻做，你觉得如何？」
- 主控答：`rg -r` 已由 P2-270 做掉；护栏建议不动；bash 这件建议当小任务派出、带测试和审查。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-278-linux-bash`，分支 `p2-278-linux-bash`（派出时从 main 建）。
- Linux 上的测试在 5090 跑，只用 `~/canter-linux/`（环境照 `bench/terminal-bench/README.md` 和 DESIGN「Linux 支持（P2-160）」）。

## 要做的

1. **Linux**：shell 工具（前台、后台、Linux 沙箱 bwrap 里）有 bash 时用 bash 跑命令，没有时照旧用 `/bin/sh`（Alpine 这类没装 bash 的环境）。bash 只从固定位置找（`/bin/bash`、`/usr/bin/bash`），不按 PATH 找，免得项目里的同名程序冒充。
2. **行为和现在一样，只换解释器**：不读用户的启动文件；`BASH_ENV` 不带进去（dash 本来不认它，带进去会在每条命令前执行别的脚本）。其它环境变量、工作目录、进程组、超时、输出截断、后台命令、Ctrl+B 都不变。
3. **工具说明写对**：shell 工具说明里「Run a command with /bin/sh -c」按实际用的写（Linux 用 bash 时写 bash）。一台机器上一次服务端启动内固定不变，不在会话中途变，免得缓存变冷。
4. **macOS 不变**：仍用 `/bin/sh`，说明文字不变（Mac 上的缓存不受影响）。
5. **不动的**：看护进程（guardian）自己的固定脚本、hooks 的 `/bin/sh`、终端状态栏脚本，都照旧用 `/bin/sh`；只读检查和危险命令检查已经按 bash 语法解析（tree-sitter-bash，认得进程替换），不改规则。审查时核对：Linux 改用 bash 后，plan 模式和只读子 agent 对进程替换、`$'…'` 这类 bash 写法的判断和 Mac 上一致。

## 主控建议的验收（待用户确认）

- Linux 上 shell 工具跑 `time true`、`[[ 1 -eq 1 ]] && echo ok`、`echo $'a\tb'` 都成功；沙箱开着和 `--yolo` 都一样。
- 没有 bash 的 Linux（Alpine 容器）照旧能跑命令，说明写 `/bin/sh`。
- macOS 上工具说明和行为不变（对比改前改后的说明文字）。
- `BASH_ENV` 指向的脚本不会被执行。

## 不要做

- 不按 PATH 找 bash，不用用户的 `$SHELL`（zsh、fish 等语法不同，检查器不认）。
- 不改护栏和只读检查的规则；不放宽 `rm` 关键路径检查（11-11 主控建议保持现状）。
- 不改 hooks、guardian、状态栏用的 shell。
- 不在核心逻辑里按模型或服务商分支（DESIGN 第 23 条）。
- 这是确定性的改动，照「先红后绿」写普通测试；不需要跑 eval（DESIGN「用 eval 来改」第 1 条）。
