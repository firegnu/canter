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
- 做法（用户定：按推荐）：照 Codex，用系统的 bubblewrap 管文件和命名空间；联网照 macOS 的规则挡（用什么挡、Unix socket 怎么管，你参考 Codex 定，写进完成记录）。不用 Landlock 文件系统规则；11-10 后续决定仅抽象 Unix socket scope 例外，见完成记录。
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

- 除用户 11-10 确认的抽象 Unix socket scope 外不用 Landlock，不随 canter 带 bwrap。不做 Windows。不改 macOS 沙箱的规则和行为。
- 不扩大沙箱范围（按域名代理、精简环境变量、MCP 隔离、文件工具移进受限进程，DESIGN 写着「以后再做」的照旧不做）。
- 不按发行版、按模型、按服务商写分支。
- 5090 上只用 `~/canter-linux/`，不碰机器上已有的东西，不用 sudo、不装系统包、不改 sysctl 和 AppArmor。不读、不改两台机器上的 `~/.canter`，不调真实模型接口。
- 不用 Python。不要按名字批量杀进程（`pkill -f canter` 这类）：主控和用户的 canter 进程命令行里都带着 canter。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如 macOS 某条规则在 bubblewrap 里做不到一样、Unix socket 怎么管拿不准），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送到 `origin`（推到 5090 上自己的裸仓库可以）。只在 `p2-272-linux-sandbox` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

实施和验证记录见后文；此前的中断原因保留如下，主控已经决定继续。

### 用户决定（11-10，实施前确认）

- 接受两处 Linux 差异，不做监管进程。Unix socket 用空目录遮蔽 `/run`（含 `/var/run`、docker.sock、XDG_RUNTIME_DIR 下总线等）和 `/tmp`，挂回命令可写范围，联网时按需只读恢复 DNS 解析文件。项目、新增目录、会话临时目录和缓存里的 socket 可建可连；不常见且仍可读位置的 socket 仍能连。
- 默认新网络命名空间隔离抽象 socket；联网时只使用 `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`，内核不支持则继续运行并在 doctor 说明。此项是原「不用 Landlock」的唯一例外，不增加文件系统或 TCP Landlock 规则。
- Git 仅锁命令启动时已有仓库的 config、hooks 等原保护路径；本条命令中新建的仓库下一条再锁。macOS 行为不变。

### 尚不存在的 writePaths（11-10 主控已决定）

- 继续核对现有验收时发现另一处机制差异：`sandbox_write_request_opens_only_that_call_and_allow_all_refuses_it` 批准尚不存在的 `../outside/new`，命令自行创建目录；`sandbox_approved_background_and_promoted_children_keep_the_request_until_exit` 批准尚不存在的 `../outside/result`，且断言后台命令实际写入前该文件仍不存在。不能统一提前创建为文件或目录，也不能默默把父目录全部放开。
- 在 5090 的 `~/canter-linux/` 下以独占临时目录前台运行 `bwrap --ro-bind / / --bind "$sandbox_probe_dir/new" "$sandbox_probe_dir/new" -- /bin/true`，实测 `bwrap: Can't find source path …/new: No such file or directory`。确认没有创建目标后清理本次临时目录；未读真实凭证、未调用模型、未启动 canter serve。
- 建议明确增加 Linux 差异：范围外 `writePaths` 必须已存在；新文件或新目录由模型重新申请其已存在的父目录，批准框明确展示父目录，仍只给这条命令。拒绝原精确申请并说明如何申请，不能自动扩大。已有可写目录内部的新建不受影响，macOS 保持原样。若必须保留精确批准不存在路径，需要另定机制，普通绑定挂载做不到，且本次决定已排除监管进程。
- 按任务「碰到要拿主意的事……停下来」停止；未改功能代码或测试，未跑 Cargo 检查，不把上述机制探测算作任务要求的 RED。设计文档已记录用户确认的 socket/Git 差异及 Landlock 唯一例外。
- 主控后续确认采用建议：Linux 拒绝不存在的申请路径，英文提示仅能申请已存在的路径、请改为申请已存在的上级目录；不自动扩大、不代为创建，原批准流程不变，macOS 不变。后续遇到 bubblewrap 无法等价实现时，不扩大权限的更严处理或明确拒绝由开发直接实施并记录；只有放宽权限或改变可见批准流程才再次停止。

### 做了什么

- 增加 Linux x86_64 后端：查找 PATH 中的系统 bwrap，根只读、当前命令范围可写、敏感目录遮蔽、原缓存和 Git 元数据只读；保留前台、后台、转后台、取消和原看护进程。使用 user／PID／IPC 命名空间，未联网时另建 network namespace。seccomp 只允许 Unix socket（批准联网或 Allow all 时加 IPv4／IPv6），禁用 io_uring、ptrace、跨进程内存访问和硬链接创建；没有新增依赖。
- 实施前述三项用户决定。`/run`、`/tmp`、XDG_RUNTIME_DIR 为空的只读挂载，可写子目录及联网所需解析文件精确放回；Landlock 先查 ABI，只有 ABI ≥ 6 才建立仅含抽象 socket scope 的 ruleset，旧内核不因此阻止执行。Git 按每次启动的已有仓库快照锁住，支持嵌套仓库、worktree、子模块，已有可写目录中的普通新建仍可用。Linux 的不存在 writePaths 在批准前明确拒绝，重新申请已存在的父目录后批准框显示该父目录。
- Linux 可丢弃缓存使用 `.cache/canter/{cargo,npm}`；`/sandbox` 列出 Linux 范围。doctor 实跑不写文件的 namespace／seccomp／scope 探测，失败提示和 shell 一致；缺少可选 scope 时说明差异。bwrap 缺失或启动失败绝不在墙外重试。保持 macOS Seatbelt 规则及批准流程。
- 恢复 P2-160 暂停的 Linux 沙箱测试、各夹具默认开关、记忆测试、两处分叉临时目录的正向断言和 yolo 重连断言；让取消／后台测试把私有 PID 映射为本次夹具的宿主 PID，再验证进程已退出。增加普通回归检查：缺失 writePaths、下一条命令锁新仓库、运行时目录及 DNS、抽象 socket、bwrap 缺失与 namespace 启动失败。

### 验证

- RED：5090 修改前运行 `cargo test -p canter-tools --test sandbox sandbox_allows_project_writes_but_rejects_outside_writes -- --exact --nocapture`，因为原 `Sandbox is not available on Linux yet` 失败；实现后同一项通过。原 RED 输出留在 `~/canter-linux/p2-272-red.log`，不是语法错误或夹具错误。
- 5090：最终 `cargo test --workspace` 成功，154 组结果共 2103 passed、0 failed、50 ignored（含文档测试；忽略项含原有测试和由父测试显式调用的 socket 辅助入口）。所有可移植沙箱测试均在 Linux 开启，只有 macOS mDNS 专属项保持平台限制。全量输出在 `~/canter-linux/p2-272-linux-final.log`；最终 `cargo clippy --all-targets -- -D warnings` 通过，输出在 `~/canter-linux/p2-272-linux-clippy-final.log`。环境使用任务独立 CARGO_TARGET_DIR、P2-160 已有 Node `v22.23.4` 和 `~/canter-linux/npm-cache`，未安装 nextest 或系统包。
- Mac：`cargo nextest run -p canter-tools -p canter-core -p canter-server -p canter --no-fail-fast` 全部通过（1189 passed、50 skipped，`/tmp/p2-272-mac-nextest-final.log`）；后续 PID 测试适配后单独完整复跑 `canter-server` 也通过（426 passed、5 skipped，`/tmp/p2-272-mac-server-final.log`）。`cargo clippy --all-targets -- -D warnings` 通过，最后输出在 `/tmp/p2-272-mac-clippy-final.log`。使用本任务独立 CARGO_TARGET_DIR；`cargo fmt --check`、`git diff --check` 通过。
- 缺少 bwrap 的单独验收：5090 前台运行 `cargo test -p canter-tools --test sandbox_unavailable unavailable_bubblewrap_explains_opt_out_and_never_runs_the_command -- --exact --nocapture`（测试只将命令会话的 PATH 设为空），1 passed，确认命令正文未执行。输出为 `Could not start sandbox: Linux sandbox could not start. Install bubblewrap and enable unprivileged user namespaces, or set "sandbox": false in config.json or use --yolo.`，留在 `~/canter-linux/p2-272-no-bwrap.log`。namespace 失败用隔离夹具的启动器故障注入检查，不改机器策略。
- 所有命令均前台执行并等到退出。Linux 全量结束后按本任务编译产物的精确路径检查 `/proc`，本任务的 `canter serve` 残留为 0；未按名字杀进程。

### 拿主意的地方

- 仅参考 Codex `rust-v0.163.0` 的 `codex-rs/linux-sandbox/README.md`、`src/bwrap.rs`、`src/seccomp.rs`，自己生成挂载参数和经典 BPF，没有搬代码，因此不新增 NOTICE 条目。Landlock ABI／scope 按[内核文档](https://docs.kernel.org/userspace-api/landlock.html)和 UAPI 核对；只使用用户允许的 scope，不加 Landlock 文件、TCP 或 signal 规则。
- 按主控后续授权选择更严的差异：绑定单个文件时可原地写，但删除／原子替换可能报 EBUSY，需要模型重新申请已存在的父目录；不自动替它改申请。禁止硬链接创建，编译工具需要时可用自身的复制退路。路径与敏感挂载冲突、挂载源会重开被隐藏的运行时目录、可写根覆盖 `/` 或 `/proc`、`/sys`、`/dev`，以及无法读取 Git 快照或准备挂载目标时明确失败；符号链接形式的 Git 目录可能整目录只读。隐藏目录下面未重新开放的原缓存继续隐藏，不为只读展示而重开。上述情形均不放宽权限、不改变批准框。
- 绑定挂载不能直接保护尚不存在的 Git 子项，所以命令期间给缺失保护目标建最小占位，并在原进程资源生命周期里持有；并发命令引用计数，最后退出仅清理 inode／内容未变的项。`commondir` 写 `.` 保持原 Git 布局，其他文件为空、目录为空。正常结束、失败和取消会清理；服务端遭 SIGKILL 可能留占位，DESIGN 已明写，不为清理另建监管进程。

### 没做的事

- 未合并、未推送、未安装或替换用户程序；没有 sudo、系统包、sysctl 或 AppArmor 改动，也没有读改真实 `~/.canter` 或复制 key。真实模型调用 0 次，DeepSeek flash 一轮验收按原任务交主控合并后做。Alpine busybox 看护进程验证仍交 P2-260。不另加绕过测试，不实现 Windows、域名代理或其他延后范围。
- 5090 内核支持抽象 socket scope；旧内核的可选降级按 ABI 分支实现，没有另装内核做实机验证。

### 踩过的坑

- bind 的源必须存在，不能精确挂载不存在的 writePaths；经主控决定改为明确拒绝。Git 缺失的 `commondir` 若占位为空会让 Git 不能发现仓库，改为 `.` 后本地依赖构建、普通仓库、worktree、子模块回归通过。
- `/tmp` 遮蔽后，原先放在 `/tmp` 的“墙外但应可读”测试源也被遮住；需要测试只读而非隐藏语义的夹具移到 Cargo 的任务独立测试目录。实际会话临时目录仍使用原 `/tmp/canter-shell-…`；Linux `/tmp` 不是 macOS 的符号链接，相关断言按实际系统语义验证。
- PID namespace 内的 `$$` 不能用于宿主 `kill(pid, 0)` 断言，原测试会拿小 PID 查到别的进程而报 EPERM；改为通过本次夹具 cwd 和 `/proc` 的 NSpid 找到自己的宿主 PID，仍断言 ESRCH。未放宽退出断言。
- 收紧挂载冲突校验时一度把已被 `/tmp` 隐藏、无需挂回的原缓存也当成启动冲突；改为继续隐藏它们，Linux 前后台及唤醒相关 38 项再次全部通过。未通过更改批准流程或放开目录解决。

没有待主控决定的设计问题；主控仍需审查分支并执行原定的合并后真实模型验收。
