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

## 完成记录

- 做了什么：新建、恢复和分叉请求增加缺省 false 的 `sandboxDisabled`，服务端用 `sessionSandbox` 宣告能力；带关闭请求的客户端先检查支持情况。交互终端和 exec 保存本次启动的 `--yolo` 意图，覆盖 `/new`、`/resume`、`-r`、`-c`、分叉、交接和服务端重连。运行时豁免与存盘权限档分离，最后一次打开决定当前执行器；`session.updated` 同步所有终端的实际沙箱状态。配置重读保留豁免，子 agent 继承创建时的实际沙箱；明确恢复子会话也采用这次终端的选择。未改 Shift+Tab 的权限逻辑或沙箱范围规则。
- 说明与协议：更新 DESIGN 第 8 条、非交互和沙箱章节、CLI 帮助、exec stderr、`/sandbox` 关闭提示及相关 README；重新生成协议 JSON Schema 和 TypeScript。参考 `/tmp/codex-ro` 的 `rust-v0.163.0`，`codex-rs/app-server-protocol/src/protocol/v2/thread.rs` 中 `ThreadStartParams` / `ThreadResumeParams.sandbox`；只参考打开边界传参的做法，没有搬代码。canter 使用布尔豁免、而非 Codex 多模式策略的理由已写进 DESIGN。
- 验证：先运行改成新预期的 `/new` 测试，确认失败于仍显示 `Sandbox on`；恢复同一会话和从磁盘恢复子会话也先取得真实行为失败，再实现到通过。新增普通终端 Shift+Tab 到 Allow all 后 `/new` 仍开沙箱、会话互不影响、最后打开者和双终端状态同步、磁盘恢复、`-c`、自动重连、分叉、配置重读、exec 新建/恢复/最近会话、子 agent 实际命令范围及协议兼容测试，并补交接状态断言。全部使用本机假模型、隔离 HOME / 状态目录和临时端口；验证了 yolo 命令确实能写项目外的测试临时文件，非 yolo 子 agent 仍受原边界限制。
- 验证命令：统一使用本任务独立 `CARGO_TARGET_DIR`。运行 `cargo nextest run -p canter -p canter-core -p canter-protocol -p canter-server -p canter-tui --no-fail-fast --status-level fail --final-status-level fail`：1,402 项运行，1,399 项首次通过，27 项默认跳过；另外三项见下文，处理后按原名定向复跑全部通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。没有跑 workspace 全套或文档示例测试。
- 主控决定：首次检查发现旧 `/new` 断言要求 yolo 仍显示 `Sandbox on`，依任务要求暂停。11-03 主控明确同意改为 `Sandbox off`，并要求普通启动 `/new` 仍显示 on 的回归测试；同时授权直接更新第 1～9 条明确推翻的旧行为断言，其余需要放宽或修改的仍先问。实际更新了 `slash_new_opens_a_new_session_in_this_terminal` 的状态文字；`new_session_retries_with_default_model_when_previous_model_was_removed` 中不带 yolo 恢复后，旧会话和新会话的预期 `sandbox_enabled` 改为 true，其余设置仍整份严格比较；`sandbox_lists_what_it_allows_or_how_to_turn_it_on` 的精确文本列表增加“不带 --yolo 启动”的提示。未删除测试或放宽断言。
- 踩过的坑：新写的 Shift+Tab 测试连续发送两次切换时，第二次可能仍基于旧档位；改为等第一次服务端状态到达再发第二次。新增协议字段后两个比较工具示例缺少结构体字段，补了 false；协议导出一致性测试提示遗漏生成文件，用仓库导出器重新生成后通过。改动 crate 测试并行运行时，未改动的 `no_example_for_non_git_empty_git_disabled_or_timed_out_git` 一次读到刚创建但尚未写入的空 PID，报 `ParseIntError`；原样单独复跑通过，没有改测试、断言或超时。
- 没做的事：没有真实模型调用（0 次），没有碰用户真实状态、安装程序或重启用户服务端，没有改其他仓库、发送消息、合并 main 或推送。实现和记录只提交在本任务分支；交叉审查、合并、全套和对外通知由主控处理。目前没有需要主控决定的事项。
