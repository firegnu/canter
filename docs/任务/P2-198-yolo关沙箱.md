# 任务：第二期 - 带 --yolo 开的会话不套沙箱

2026-11-03，主控建。11-03 用户：「193补收尾，然后派198」。canter/main 交给 canter/dev-yolo-sandbox（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：高 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.94）、要、碰要害）
类型：功能变更
依据：本轮让 `--yolo` 开的会话不套沙箱，并改说明；不改 Shift+Tab、不加别的沙箱开关。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

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

## 主控补的（11-03，用户没说到的边角，照「跟着打开它的终端走」定）

7. 一个会话同时被带 `--yolo` 和不带的两个终端打开时，以最后一次打开（新建、恢复、重连）它的那个终端为准；两个终端的状态栏都照这个会话当时的实际情况显示。
8. `config.json` 的 `sandbox` 改了、服务端自动重读（P2-154）时，`--yolo` 开的会话不被改回去；不带 `--yolo` 的照旧跟配置走。
9. 不套沙箱的会话里开出的子 agent 也不套沙箱；套沙箱的会话开出的照旧。

## 先读

- `docs/DESIGN.md`「沙箱（P2-123）」一节（第 418 行起，尤其第 426 行 `--yolo` 那条）、第 296～298 行 `canter exec`、第 8 条协议下 P2-177～P2-180 那几条（看协议改动怎么记）。
- 现在的接线：`crates/canter/src/lib.rs`（`--yolo`、`--help` 文字、`Open::New/Resume/Continue`）、`crates/canter/src/exec.rs`、`crates/canter-protocol/src/commands.rs`（新建、恢复会话的参数）、`crates/canter-core/src/session.rs` 和 `configuration.rs` 里 `sandbox` / `sandbox_enabled`、`crates/canter-tui/src/statusline/bar.rs`。
- Codex 只读克隆在 `/tmp/codex-ro`：参考它 app-server v2 新建、恢复线程时带 `sandbox` 的做法；协议里和 Codex 不一样的地方写进 DESIGN 第 8 条，说明为什么。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-198-yolo-no-sandbox`，分支 `p2-198-yolo-no-sandbox`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-198-yolo-no-sandbox`。

## 怎么算做完

- 上面 1～9 条做到。用户原话：「--yolo就是不管配置文件怎么写，都是sandbox off」。
- DESIGN 第 426 行和第 296 行 `exec` 那句改成新规矩；协议改动记进 DESIGN 第 8 条；`canter --help` 里 `--yolo` 的说明（现在写着 `commands still run in the sandbox`）跟着改；`canter exec` 关于沙箱的 stderr 提示和新规矩一致。屏幕上的字一律英文。
- 验证只做这些：为本件加针对性测试（新建、恢复、重连、配置重读、子 agent 各至少一条，测试用假模型、隔离的 HOME 和 canter 状态目录、临时端口）；跑改到的 crate 的测试（`cargo nextest run -p <crate>`，改了哪几个跑哪几个）、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。全套由主控合并后跑，你不跑。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 Shift+Tab 的行为，不加界面里临时切沙箱的办法，不加新的配置项。
- 不改沙箱本身的规则（能读写哪些路径、联网），不改权限三档的含义。
- 不调真实模型接口；不碰用户真实的 `~/.canter`、会话和配置。
- 不改 ranch、paddock、cairn 等别的仓库，也不送话给它们（主控合并后去说）。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录。停自己起的服务用起的时候记下的 PID。
- 不用 Python。
- 碰到要拿主意的事（包括协议字段怎么起名拿不准、上面 1～9 条之间有冲突、现有测试要改断言才能过），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-198-yolo-no-sandbox` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 合并以后（主控做）

- 跑一次全套（用户：「做完在跑全套」，P2-197、P2-199 没跑的一起补）。
- 告诉 ranch/main、paddock/main：`--yolo` 起的 canter 不再套沙箱，请它们改各自的说明。
