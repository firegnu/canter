# 任务：第二期 - 带 --yolo 开的会话不套沙箱

2026-11-03，主控建（草稿，还没派）。
待用户：用户说派再派（11-03：「先别派」）。

## 用户原话

- 11-03：「另外觉得--yolo不应该打开sandbox。其他默认可以打开（或者根据配置文件），但是--yolo就是不管配置文件怎么写，都是sandbox off。」
- 同日 paddock/main 告知（用户在场）：用 canter 派真实小活时，沙箱挡住了两回：写不了 worktree 外的编译目录（`paddock-worktrees/.target/<分支>`）；开不了 PTY（`failed to openpty: Operation not permitted`），21 个终端测试在墙内全挂。派活方看不出 canter 开着沙箱；关沙箱只能改全局 `config.json` 的 `sandbox:false`，会连用户自己平时用的 canter 一起关。
- 主控说明这推翻 DESIGN 第 424 行（「`--yolo` 仍只等于 `allowAll`，沙箱照开」），建议照用户说的做；用户：「加一个任务吧，稍后我来做。做完在跑全套。」

## 要做成什么

1. 用 `--yolo` 开的会话，不管 `config.json` 的 `sandbox` 怎么写，命令都不套沙箱。
2. 不带 `--yolo` 开的会话照旧按配置（默认开）；同一个服务端上两种会话可以同时在跑，互不影响。
3. 会话里用 Shift+Tab 切到 Allow all 不关沙箱，只有启动时的 `--yolo` 关。
4. 状态栏的 `Sandbox on/off` 照这个会话实际的情况显示。

## 用户定的（11-03）

主控推荐、用户：「接受你的推荐。更新任务文件。先别派」。

5. `canter exec --yolo` 同样不套沙箱，和交互式的 `--yolo` 一个意思。
6. 沙箱跟着这次打开会话的终端（或 exec）走：
   - 用 `--yolo` 开的终端，它打开的会话都不套沙箱：包括这个终端里 `/new`、`/resume`、分叉、交接打开的会话，以及服务端升级后终端自动重连恢复的会话。
   - 不带 `--yolo` 打开（包括 `canter -r`、`canter -c`），照配置（默认开）。
   - 权限档照旧跟着会话存：用 `--yolo` 开过的会话不带 `--yolo` 恢复时，权限仍是 Allow all，沙箱照配置开回来（就是现在 `--yolo` 的样子）。

## 做完以后

- 改 DESIGN 第 424 行和相关说明；`canter --help` 里 `--yolo` 的说明（现在写着 `commands still run in the sandbox`）跟着改。
- 合并后由主控跑一次全套（用户：「做完在跑全套」，P2-197、P2-199 没跑的一起补）。
- 告诉 ranch/main、paddock/main：`--yolo` 起的 canter 不再套沙箱，请它们改各自的说明。
