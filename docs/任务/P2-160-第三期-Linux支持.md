# 任务：Linux 支持（一）：能编译、能跑，沙箱下一件做

2026-11-10，canter/main 交给 canter/dev-linux（Codex，`gpt-6-astra` / xhigh）。交叉审查交 Claude Code（`opus[1m]` / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 1.0）、要、碰要害）
执行：派出
类型：功能变更
依据：11-10 定接 Terminal-Bench（P2-260）前先让 canter 能在 Linux 上跑。原排在第三期，用户提前。Linux 支持拆两件：本件做编译和运行，Linux 沙箱是 P2-272。两件都做完再做 P2-260。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
优先：高
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-29：「暂时在这台mac上。linux的你开一个任务排到第三期」
- 11-10（看完 benchmark 评估，主控说接 Terminal-Bench 要先让 canter 能在 Linux 容器里跑）：「我觉得可以先做160。我的那台5090安装的是ubuntu16.04. 你应该可以用上」
- 11-10（主控问做多大，推荐拆两件、沙箱另做）：「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.你觉得如何？」
- 11-10：用 `my-5090d-host` 开发和测试。
- 11-10（主控说 Terminal-Bench 要把 canter 放进每道题自己的 Linux 容器里跑，24.04 上正常编出来的程序进不了 glibc 更旧的容器，建议静态版写进本件）：「这个你考虑了吗？」；问过能不能在 macOS 上跑、能不能接进现在的 eval 架构后：「还是走正常的bench吧。不要投机取巧了」
- 11-10（三处待定）：沙箱开着时的提示，用户问「按照你的改的话，会破坏什么吗？」，主控答只改 Linux 上那句报错、Mac 上走不到后：「可以，提交然后派吧」；`service install`：「不做」；验收：「可以，接受你的建议」。

## 背景（主控 11-10 摸的底）

- 5090 两台都是 Ubuntu 24.04（不是 16.04）。用 `my-5090d-host`（ssh 别名，免密登录；x86_64、32 核、60G 内存，内核 7.0，glibc 2.39，已装 Rust 1.97、Docker、bubblewrap，用户在 docker 组里；没装 cargo-nextest）。这台是用户的深度学习工作站，跑着 GitLab Runner、Ollama。
- 主控把 main `ca163f2` 原样拷过去试过（`git archive`，不是 git 仓库）：`cargo build --workspace --all-targets` 2 分 18 秒编过（只有一条 `ScratchConfiguration` 没用到的警告）。`cargo test --workspace` 1987 过、66 挂、49 跳过。挂的大致几类：
  - 沙箱默认开着，`crates/canter-tools/src/sandbox.rs` 约 38 行在非 macOS 上直接报 `Seatbelt requires macOS`，读文件、记忆、shell 都走它，所以连读文件都失败；
  - 后台命令、进程组、SIGKILL、Ctrl-C 一类（`canter-server` 的 `user_shell`、`wake`、`lsp_tests`，`canter` 的 `exec`、`tui`）；
  - 浏览器登录要打开网址（`auth` 测试）；
  - 一个测试挂了把锁弄坏，后面跟着挂的（`PoisonError`、`index out of bounds`），修好源头可能就好了；
  - eval 几条可能只是拷过去的目录不是 git 仓库。
  
  试编的目录已删。开工先在 5090 上自己跑一次，拿到准确名单。
- 只在 macOS 能用的地方：沙箱（Seatbelt）、`canter service install`（写 `~/Library/LaunchAgents`，`crates/canter/src/service.rs`）、打开链接和浏览器登录（`open`）。DESIGN 第 19 条已定 Linux 上点链接里的路径改成复制。

## 先读

- `docs/DESIGN.md`：第 19 条里「可点的链接」那段（Linux 上的处理）；沙箱一节（搜 Seatbelt、P2-123）；P2-14 `service install` 那段（搜 `LaunchAgents`）。
- `crates/canter-tools/src/sandbox.rs`、`crates/canter/src/service.rs`、`crates/canter/src/auth.rs`、`crates/canter-tui/src/links.rs`。
- 后台命令和进程组：`crates/canter-tools/src/shell.rs`、`crates/canter-server` 里 `user_shell`、`wake`、`lsp` 相关文件。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-160-linux`，分支 `p2-160-linux`（已从 main 建好）。代码在这里改、在这里提交。
- Mac 上的编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-160-linux`。
- 5090（`ssh my-5090d-host`）：只用 `~/canter-linux/` 下面，自己建。代码放 `~/canter-linux/p2-160-linux`，编译目录 `~/canter-linux/.target/p2-160-linux`。同步办法你定（比如在 `~/canter-linux/` 建一个裸仓库，从 worktree `git push` 过去再 checkout；或 rsync 工作区），要保证 5090 上跑的是你当前的代码、而且是个 git 仓库。

## 要做的

- canter 在 Linux（x86_64，Ubuntu 24.04）上能编译，服务端、终端、`canter exec` 都能用。
- Linux 上原来的功能照 macOS 一样能用，沙箱除外。
- 沙箱：本件不做 Linux 沙箱（P2-272 做）。Linux 上沙箱开着（默认就开着）时，命令和改文件照旧拒绝，行为不变，只把报错 `Seatbelt requires macOS` 换成看得懂的英文：Linux 上还没有沙箱，要先在配置里关掉沙箱（`"sandbox": false`）或用 `--yolo`。不悄悄地不隔离就跑。macOS 上不变。
- `canter service install`：Linux 上不做（用户：「不做」）。只在 Linux 上提示一句英文说不支持（终端打开时会自己起服务端），不往 Linux 上写 macOS 的 launchd 配置。macOS 上不变。
- 打开网址：Linux 上有图形界面时用 `xdg-open`，打不开就把网址打出来让用户自己开（登录照样能走完）。DESIGN 第 19 条已定的链接规则照旧。
- 静态版：编出 x86_64 的 musl 静态版 canter，放进系统比 24.04 旧的容器、没有 glibc 的容器（Alpine）也能跑。为 P2-260 准备：Terminal-Bench 把 canter 放进每道题自己的容器里跑。在 5090 上用 Docker 里的编译环境编，不用 sudo、不装系统包；怎么编（编译用的镜像、`cross`、`cargo-zigbuild` 等）你定，编的命令写进 `docs/DESIGN.md` 或 README（你选一处，写明），完成记录里也写。带 C 代码的依赖（比如 tree-sitter）编不过时停下来问。
- 测试：Linux 上该跑的测试都过。只有测的东西本身只在 macOS 有（Seatbelt、launchd、`open`）的测试才只在 macOS 跑；Linux 上挂了的其他测试要修好代码或测试里真正的平台差异，不能为了过而加 `#[cfg(target_os = "macos")]` 跳过、删掉、放宽断言或缩短超时。拿不准某条算不算只在 macOS 有，停下来问。
- 改设计先改 `docs/DESIGN.md`（Linux 支持的范围、沙箱开着时的行为、`service install`、静态版）。

## 怎么算做完

- 验收（主控建议，用户 11-10：「可以，接受你的建议」）：
  1. 5090 上 Linux 测试全过（只在 macOS 跑的除外），Mac 上照旧全过。
  2. 主控在 5090 上用 `canter` 打开终端、用 `canter exec`，各真跑一轮（DeepSeek flash，`--yolo`）。要把 DeepSeek key 放到 5090 的 `~/.canter/config.json`，用户同意才放，跑完问用户删不删。（合并后主控做，不归你。）
  3. 在 5090 上开着沙箱时，看到上面那句英文提示。
  4. 静态版放进 Debian 11、Ubuntu 20.04、Alpine 三个容器，`canter --version` 能跑、服务端能起来。在容器里真调模型留给 P2-260。
- 验证只做这些：
  - 修进程组、信号、沙箱提示这类行为的地方，先有一条在 Linux 上因这个原因失败的测试（多数是现成挂着的那些），改完变过；
  - 5090 上：`cargo test --workspace`（没装 nextest，不要装）全过；`cargo clippy --all-targets -- -D warnings`；
  - Mac 上：你改过的每个 crate 跑全 `cargo nextest run -p <crate>`，另外 `canter`、`canter-server` 两个一定跑（重放和进程组测试在里面）；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - 验收第 3、4 条在 5090 上各做一次，命令和结果写进完成记录。
  
  觉得不够，在回复里说，不要自己加。

## 不要做

- 不做 Linux 沙箱（P2-272），不做 systemd 服务。静态版只编出来、验过能跑，不做发布流程（上传、版本号、`canter upgrade` 装静态版）。不接 Harbor、不跑 Terminal-Bench（P2-260）。
- 不按发行版、按模型、按服务商写分支。
- macOS 上的行为不变（沙箱、`service install`、打开链接）。
- 5090 上只用 `~/canter-linux/`，不碰机器上已有的东西（Conda、GitLab Runner、Ollama、已有的 Docker 容器和镜像、`ml-mode`），不用 sudo，不装系统包，不往 `~/.cargo/bin` 装工具。自己起的容器用完删掉；自己拉的镜像留着，在完成记录里列出来。不读、不改 5090 和 Mac 上的 `~/.canter`。不把 DeepSeek key 带过去，测试不调真实模型接口。
- 不用 Python。不要按名字批量杀进程（`pkill -f canter` 这类）：主控和用户的 canter 进程命令行里都带着 canter。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（包括上面说的「拿不准算不算只在 macOS 有」「C 依赖编不过」），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送到 `origin`（推到 5090 上自己的裸仓库可以）。只在 `p2-160-linux` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
