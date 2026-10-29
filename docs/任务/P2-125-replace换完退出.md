# 任务：`canter serve --replace` 换完退出

2026-10-29，canter/main 交给 canter/dev-replacedetach（<派的时候按读表补：哪家、模型 / 强度>）。
路由：<派的时候补>
类型：功能变更
依据：`canter serve --replace` 现在换掉旧服务端后自己留在前台当新服务端，窗口一关、Ctrl+C 服务端就停了，升级要专门占一个窗口。本件让它在后台起新服务端，新服务端就绪后命令退出，把终端还给用户。不带 `--replace` 的 `canter serve` 照旧在前台跑。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：P2-124（已就绪那一行，已合并）
执行：派出

## 用户原话

- 10-29 装 P2-124 后，用户：「有一个问题啊，--replace跑完不退出吗？」主控说明它换完就成了前台的新服务端，建议让 `--replace` 在后台起新服务端（和终端自动起的一样，日志写进 `server.log`），打出已就绪那一行后退出；只想前台跑、看输出时用不带 `--replace` 的 `canter serve`。用户：「开这件，换完退出」

## 先读

- `crates/canter/src/lib.rs`、`main.rs`（`serve` 命令入口，P2-124 刚加了版本参数）。
- `crates/canter-server/src/lib.rs`（`serve`：拿锁、监听、注册、已就绪那一行、关机）、`lifecycle.rs`（`Lease::acquire` 里 `--replace` 怎么让旧服务端退出、怎么等）、`discovery.rs`（`ensure_server`：终端自动起服务端的办法，`setsid` 脱离终端、清环境、输出写进 `~/.canter/server.log`、等就绪）。
- `docs/任务/P2-124-服务端启动好了打一行已就绪.md`。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-125-replace-detach`，分支 `p2-125-replace-detach`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-125-replace-detach`。

## 要做的

1. `canter serve --replace` 照现在的规矩让旧服务端退出（等正在跑的一轮、等连接断开的时长都不变），新服务端在后台起，和终端自动起的一样（脱离终端、输出写进 `~/.canter/server.log`），尽量复用自动起服务端那套办法，不另写一套。
2. 新服务端就绪后，命令在终端里打出 P2-124 那一行（`canter server ready · <版本> · pid <新服务端的 pid>`），退出码 0；之后关掉这个终端、按 Ctrl+C 都不影响新服务端。
3. 换不成（旧服务端没在时限内退出、配置不对、新服务端起不来）时，在终端里打出原因、退出码不是 0，不留下半起的服务端；原因要能看懂，起不来时提示看 `~/.canter/server.log`。
4. 原来没有服务端在跑时，`--replace` 也照这样在后台起一个、就绪后退出。
5. 不带 `--replace` 的 `canter serve` 不变：照旧在前台跑、打已就绪那一行。
6. `canter --help` 里 `serve` 那两行说明跟着改；DESIGN 里写服务端怎么起、怎么升级的地方补一句（找不到合适的地方就在完成记录里说）；HANDOFF 不用改，主控改。

## 怎么算做完

- （用户 10-29）`canter serve --replace` 换完退出。
- 验证只做这些：一条针对这件的测试（有旧服务端时 `--replace` 换掉、命令退出、新服务端还在且能连、终端输出里有已就绪那一行；没有服务端时也一样），先确认它在 main 上失败；再跑全套 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改旧服务端退出的规矩和时限，不改终端自动起服务端的行为，不改 `canter serve`（不带 `--replace`）。
- 不加等待进度之类别的输出。
- 不碰用户正在跑的服务端和 `~/.canter`；测试用隔离的 HOME 和 `CANTER_HOME`、临时 socket，不调真实模型接口。测试里起的服务端用完按记下的 PID 停掉，不留在后台。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-125-replace-detach` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑没有就写「没有」。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
