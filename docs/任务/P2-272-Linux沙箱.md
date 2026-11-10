# 任务：Linux 支持（二）：Linux 沙箱

2026-11-10，canter/main 交给 canter/dev-linux-sandbox（Codex，`gpt-6-astra` / xhigh）。交叉审查交 Claude Code（`opus[1m]` / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.88）、要、碰要害）
类型：功能变更
依据：11-10 Linux 支持拆两件，P2-160（已合并）做编译和运行，本件做 Linux 上的沙箱，让 Linux 上沙箱开着时和 macOS 一样能用、管同样的范围。两件都做完再做 P2-260。
依赖：P2-160
执行：派出
优先：高
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-10：「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.你觉得如何？」
- 11-10（P2-160 合并后）：「推送吧，然后接着做272」
- 11-10（主控列了三处待定：用系统 bubblewrap、没有 bwrap 时失败并提示、三条验收）：「都按推荐来，提交然后派吧」

## 背景（主控 11-10 摸的底）

- macOS 沙箱的范围见 `docs/DESIGN.md`「沙箱（P2-123）」一节：模型 shell（前台、后台、转后台、临时脚本）在墙里；可写 cwd、P2-118 新增目录、会话临时目录、可丢弃缓存，`.git` 的 config、hooks 锁住；不能读写 canter 状态目录（含控制 socket）和 `~/.ssh`、`~/.aws`、`~/.gnupg`；默认不联网，`sandboxRequest` 可申请联网和写路径；read / write / edit 在服务端守同一套边界；被墙挡住时识别输出提示申请；`/sandbox`、状态栏、doctor 显示和自检。用户 `!`、MCP、`web_fetch` 在墙外。
- 现在 Linux 上沙箱开着时一律拒绝（P2-160），提示关沙箱或用 `--yolo`。
- 5090（`my-5090d-host`，Ubuntu 24.04，内核 7.0）：bubblewrap 0.9.0 已装。Ubuntu 默认限制普通用户建命名空间（`kernel.apparmor_restrict_unprivileged_userns = 1`），但系统带了 `bwrap-userns-restrict` 配置给 bwrap 放行。主控实测普通用户 `bwrap --ro-bind / / --dev /dev --proc /proc --unshare-net --tmpfs ~/.ssh` 能跑：根目录只读、`~/.ssh` 被盖住、没有网络。内核也开了 Landlock。
- Codex（`rust-v0.163.0`，只读克隆在 `/tmp/r49-src/codex`）在 Linux 上：`codex-rs/linux-sandbox` 用 bubblewrap 管文件和命名空间（`--ro-bind / /`，可写的再 `--bind`，受保护的子路径再盖成只读），再用 seccomp 挡联网（并挡 io_uring 防绕过）；优先用 PATH 上的系统 bwrap，没有就用随包带的一份；建不了命名空间时启动就警告。Landlock 已弃用：它只能列允许的路径，做不到「全都能读、唯独某些目录不能读」，也挡不住 Unix socket。

## 先读

- `docs/DESIGN.md`：「沙箱（P2-123）」一节全文，「Linux 支持（P2-160）」一节。
- `docs/任务/P2-123-沙箱.md`、`P2-163`、`P2-164`、`P2-165`、`P2-166` 的任务文件里定规则的部分（被挡识别、`sandboxRequest`、文件工具边界、显示）。
- `crates/canter-tools/src/sandbox.rs`、`shell.rs`、`guardian.rs`；`crates/canter-server/src/doctor.rs`。
- Codex：`/tmp/r49-src/codex/codex-rs/linux-sandbox/README.md`、`src/bwrap.rs`、`src/seccomp.rs`（只读，`/tmp` 重启会清空，丢了照 HANDOFF「机器和环境」重新克隆）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-272-linux-sandbox`，分支 `p2-272-linux-sandbox`（已从 main 建好）。代码在这里改、在这里提交。
- Mac 上的编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-272-linux-sandbox`。
- 5090（`ssh my-5090d-host`）：只用 `~/canter-linux/` 下面。代码放 `~/canter-linux/p2-272-linux-sandbox`，编译目录 `~/canter-linux/.target/p2-272-linux-sandbox`，同步办法同 P2-160（见 `docs/任务/P2-160-第三期-Linux支持.md` 完成记录）；那里留着 P2-160 解包的 Node（`~/canter-linux/node-v22.23.4-linux-x64`）和 npm 缓存，eval 测试要用。

## 要做的

- Linux（x86_64）上沙箱开着时，模型 shell 进墙跑，范围和 macOS 一样（上面背景第一条和 DESIGN 沙箱一节的全部规则，包括 `sandboxRequest` 申请联网和写路径、被挡识别、`/sandbox`、状态栏、doctor 自检）。read / write / edit 的服务端边界在 Linux 上照 macOS 生效。macOS 上的行为不变。
- 做法（用户定：按推荐）：照 Codex，用系统的 bubblewrap 管文件和命名空间；联网照 macOS 的规则挡（用什么挡、Unix socket 怎么管，你参考 Codex 定，写进完成记录）。不用 Landlock。
- 机器上没有 bwrap、或者 bwrap 建不了命名空间（比如在不给权限的容器里）时（用户定：按推荐）：照现在的规矩「起不来时命令失败并说明原因」，给一句英文：要装 bubblewrap，或关沙箱、用 `--yolo`；doctor 也报出来。不随 canter 带一份 bwrap，不悄悄在墙外跑。
- P2-160 交叉审查留下的（见下一节）一起做。
- 改设计先改 `docs/DESIGN.md`（沙箱一节加 Linux 的做法，P2-160 那节里「Linux 上没有沙箱」的说法改掉）。
- 从 Codex 搬代码（比如 seccomp 规则）时，文件开头注明来源（项目、`rust-v0.163.0`、原文件），保留许可证声明，按 Apache-2.0 维护 `NOTICE`；只参考不搬的，在完成记录里写参考了哪里。

## P2-160 交叉审查留给本件的（11-10）

- 测试夹具里按 `cfg!(target_os = "macos")` 写的沙箱默认值，有了 Linux 沙箱后改回两边都开：`crates/canter/tests/tui.rs` 的 World、`tests/server.rs` 的 Fixture、`canter-server` 的 `goal_tests`、`user_shell_tests`、`fork_tests`，`tests/exec.rs` 几处和 `--yolo`，两个记忆测试的过滤，doctor 测试的分支。
- `crates/canter-server/src/fork_tests.rs` 两个分叉测试（约 455、465、680、699 行）：P2-160 改成从模型历史读临时目录，macOS 上丢了「会话沙箱可写范围里有自己的临时目录」这条正向断言，补回（两边都有沙箱后两边都断言）。
- `crates/canter/tests/support/yolo.rs` 的 `yolo_reconnect_restores_the_terminals_sandbox_choice`：开头补 `world.set_sandbox(true)`，否则 Linux 上测不到 `--yolo` 重连后丢没丢。
- `crates/canter-server/src/doctor.rs` 约 470 行：Linux 上 doctor 的沙箱提示（`Seatbelt sandboxing is unavailable…` / `Run on macOS…`）和工具报错那句统一。
- 看护进程 `kill -KILL -<pgid>` 在 Alpine 的 busybox ash 上没验过（P2-260 在 Alpine 题目容器里跑时顺手看，不归本件）。

## 怎么算做完

- 验收（主控建议，用户 11-10：「都按推荐来」）：
  1. 5090 上沙箱开着跑 Linux 全套测试全过（Seatbelt 专属的除外），macOS 上沙箱的测试在 Linux 上有对应的都跑；Mac 上照旧全过。
  2. 5090 上开着沙箱，主控用 DeepSeek flash 真跑一轮（和 P2-160 验收第 2 条一起做，要先把 key 放到 5090，用户同意才放）：让模型读 `~/.ssh` 被挡，写 cwd 外被挡或要批准，联网要批准；cwd 里正常改文件、跑测试。跑完问用户 key 删不删。
  3. 没有 bwrap 时看到上面那句英文提示。
- 验证只做这些：
  - 先有一条在 Linux 上因为没有沙箱而失败的测试（比如沙箱开着时读 `~/.ssh` 被挡、写 cwd 外被挡、默认不能联网），改完变过；
  - 5090 上：沙箱开着跑 `cargo test --workspace`（没装 nextest，不要装）全过；`cargo clippy --all-targets -- -D warnings`；
  - Mac 上：改过的每个 crate 跑全 `cargo nextest run -p <crate>`，另外 `canter`、`canter-server` 一定跑；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - 验收第 3 条在 5090 上做一次（比如让 PATH 里找不到 bwrap），命令和结果写进完成记录；第 2 条合并后主控做，不归你。
  - 跑完 Linux 全套后 5090 上不留 `canter serve`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不用 Landlock，不随 canter 带 bwrap。不做 Windows。不改 macOS 沙箱的规则和行为。
- 不扩大沙箱范围（按域名代理、精简环境变量、MCP 隔离、文件工具移进受限进程，DESIGN 写着「以后再做」的照旧不做）。
- 不按发行版、按模型、按服务商写分支。
- 5090 上只用 `~/canter-linux/`，不碰机器上已有的东西，不用 sudo、不装系统包、不改 sysctl 和 AppArmor。不读、不改两台机器上的 `~/.canter`，不调真实模型接口。
- 不用 Python。不要按名字批量杀进程（`pkill -f canter` 这类）：主控和用户的 canter 进程命令行里都带着 canter。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如 macOS 某条规则在 bubblewrap 里做不到一样、Unix socket 怎么管拿不准），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送到 `origin`（推到 5090 上自己的裸仓库可以）。只在 `p2-272-linux-sandbox` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
