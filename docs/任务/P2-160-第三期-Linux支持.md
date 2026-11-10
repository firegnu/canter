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

## 阶段记录（2026-11-10，未完成，等待主控决定）

以下保留当时中断的记录；CA 决定和后续结果见文末完成记录。

### 做了什么

- Linux 默认沙箱继续拒绝执行，错误改为 `Sandbox is not available on Linux yet. Set "sandbox": false in config.json or use --yolo.`；新增公开工具接口回归，检查 shell、read、write、edit 都拒绝且不产生写入。Linux 的 service install/uninstall 明确提示不支持、终端自动启动服务端，测试确认不会写 launchd 文件。
- 保留已有 xdg-open 实现，修正 Linux 登录文案中的 Mac，并验证浏览器启动器失败后手动登录仍完成。已有终端链接规则未改。
- 修复 `/bin/sh` 看护进程杀进程组的兼容写法；会话写锁、服务端锁、发现探测锁显式解锁，避免 Linux 并发 fork 继承文件描述符导致短暂残留锁。
- 普通功能测试在 Linux 显式关闭未实现的沙箱，保留 macOS 沙箱覆盖；只有确实验证 Seatbelt/launchd 的部分限于 macOS。fork scratch 隔离测试改从持久化上下文读取实际 scratch 路径；仍检查隔离。修正 dash 作业控制、终端自动折行、Node 测试报告格式、复制参数和部署夹具复制可执行文件的真实平台差异，保留原功能断言。
- DESIGN 记录 Linux 范围、静态构建命令和待定 CA 问题。C 依赖全部编过，产物 `~/canter-linux/.target/p2-160-linux/musl/x86_64-unknown-linux-musl/release/canter` 经 `file` 确认是 x86-64 static-pie，无 glibc 动态依赖。

### 验证了什么

- 开工在独立 Linux git 仓库跑 `cargo test --workspace --no-fail-fast`：1984 通过、69 失败、49 忽略。新增沙箱、service 测试和调整后的 auth 测试均先观察到目标原因失败，再修到通过；进程组、锁、信号等使用现有失败测试定位并回归。
- Linux 最终 `cargo test --workspace` 退出 0：合计 2054 通过、0 失败、49 忽略（含文档测试）。命令在 `~/canter-linux/p2-160-linux` 内执行：

  ```sh
  PATH="$HOME/canter-linux/node-v22.23.4-linux-x64/bin:$PATH" \
  npm_config_cache="$HOME/canter-linux/npm-cache" \
  CARGO_TARGET_DIR="$HOME/canter-linux/.target/p2-160-linux" \
  cargo test --workspace
  ```

  日志为 `~/canter-linux/p2-160-linux-final.log`。Node 只解包到任务目录，官方 v22.23.4 的 linux-x64 包经官方 SHA256 核对，未装宿主机包。
- Mac 在指定 target 目录运行 `cargo nextest run -p canter -p canter-server -p canter-core -p canter-tools -p canter-store -p canter-tui -p canter-eval --no-fail-fast`：1990 通过、50 跳过。最后修改部署测试夹具后重新跑完整 `cargo nextest run -p canter --no-fail-fast`：229 全过。日志分别为 `/tmp/p2-160-mac-nextest-final.log`、`/tmp/p2-160-mac-canter-final.log`。
- `cargo fmt --check`、`git diff --check` 通过。
- 静态构建按 DESIGN 的 Docker 命令完成，包含 tree-sitter 等 C 依赖。构建日志为 5090 上 `~/canter-linux/p2-160-musl-final.log`。
- 验收 4 的 Debian 11 检查：`--version` 成功，显示 `canter 0.1.0 · 3690057-dirty`（这是独立 Linux 验证仓库的基线提交，不是本分支提交号）；随后服务端在加载 CA 时 panic。使用任务目录内的 Rust 检查程序，隔离 HOME/CANTER_HOME，假模型配置、禁网容器；计划检查 `/v1/info` 后发送 SIGTERM 并等待退出，实际未到 HTTP 检查。命令为：

  ```sh
  docker run --rm --network none \
    -v "$HOME/canter-linux/.target/p2-160-linux/musl/x86_64-unknown-linux-musl/release/canter:/canter:ro" \
    -v "$HOME/canter-linux/musl/smoke:/smoke:ro" debian:11 /smoke
  ```

  错误是 `crates/canter-server/src/model_catalog.rs:190` 的 `catalog HTTP client`：`No CA certificates were loaded from the system`。遇到该决策点后停止后续容器验收，等待已启动的前台测试结束；没有遗留本任务容器。

### 要主控决定

Debian 11 基础镜像没有系统 CA；当前 reqwest/rustls 客户端在服务端启动时即加载 CA，尚未请求模型也会失败。静态链接消除了 glibc 依赖，但没有消除证书依赖。

- 方案 A（建议）：明确运行环境需要系统 CA，只在自己的验收镜像中加入 `ca-certificates`，继续沿用现有系统信任源。后续 P2-260 在部署 canter 时保证证书可用。
- 方案 B：随静态版携带 CA 根证书，支持无系统 CA 的基础镜像。这需要决定信任源、更新方式及依赖，属于新的 TLS 设计。

尚未采用任一方案，也未改 TLS 行为。按本任务「碰到要拿主意的事……结束这一轮，等主控回复」停止；主控的决定后续补在这里。

### 没做的事

- 本任务尚未完成；两端 clippy、验收 3 单独展示提示尚未执行（Linux 全套已包含提示断言）。Debian 服务端验收未通过，Ubuntu 20.04 和 Alpine 容器运行验收尚未执行。
- 未实现 Linux 沙箱、systemd、发布/升级流程；未接 Harbor 或 Terminal-Bench；未读写两端真实 `~/.canter`，未带凭证，真实模型调用 0 次。合并后主控的 DeepSeek 终端/exec 验收未做。未合并 main、未推 origin。
- 自己拉取/构建的镜像保留：`rust:1.97-alpine`（固定 digest 见 DESIGN）、`canter-p2-160-musl:latest`、`debian:11`、`ubuntu:20.04`、`alpine:3.23`。自己的容器均以前台 `--rm` 运行并已退出；未改现有容器和镜像。代码、构建缓存、Node 和检查程序保留在 `~/canter-linux/`，供继续验收。

### 踩过的坑

- Linux 单测单跑会隐藏 fork 继承锁的问题；完整并发测试才复现关闭 writer 后仍报 Locked、服务端锁探测短暂误报。显式 unlock 后全套通过。
- 看护脚本原来的 `kill -KILL -- -<pgid>` 不被 dash 接受；中途尝试 `kill -s KILL -<pgid>`，Mac SIGKILL 回归仍失败。最终使用两端都接受的 `kill -KILL -<pgid>`，两端进程组回归通过。
- Linux 并发部署测试偶发 ETXTBSY：其他测试的 fork 可继承 `fs::copy` 期间打开的可写可执行文件。夹具改用前台 `/bin/cp`，写描述符只在复制子进程中存在；Linux 部署 9 项和全套、Mac canter 229 项通过。
- 远端缺 Node，eval 需要它；解包独立官方发行包后仍遇 Node 默认报告格式差异，fixture 显式用 spec 报告器，eval 42 项通过。macOS 专有 `cp -cR` 改为 `cp -R`。
- 静态构建成功不代表基础容器能启动服务端：缺少 CA 的失败发生在 HTTP 客户端初始化，而不是首次联网。

## 完成记录

### 做了什么、主控决定

沿用前述 Linux 兼容修复。主控 11-10 明确选方案 1：沿用系统信任源，不随静态版携带 CA；运行镜像用 apt-get/apk 安装 `ca-certificates`。缺 CA 时只调整初始化失败的处理，不改信任源或其他 TLS 行为。该决定已写进 DESIGN 的静态版说明。

模型目录 HTTP 客户端构建失败现在通过 ModelAuth 返回 ServerError，再走已有命令行错误输出。缺 CA 时输出 `canter: No system CA certificates were found. Install ca-certificates and try again.`，退出码 1；替换服务端的预检查也会返回错误，不先停止旧服务端。新增 Linux CLI 回归测试，隔离 HOME 和证书目录，断言完整错误、退出码以及没有服务端注册文件；先观察到原 panic/退出码 101，再验证修复通过。

错误来源和测试隔离方式参考本地已锁定依赖源码：rustls-platform-verifier 0.7.1 的 `src/verification/others.rs`、rustls 0.23.46 的 `src/error.rs`、rustls-native-certs 0.8.5 的 `src/lib.rs`。只参考，没有复制第三方实现或新增依赖。

### 验证了什么

- Linux 最终 `cargo test --workspace` 退出 0，合计 2055 通过、0 失败、49 忽略，包含文档测试；环境变量与前述阶段记录相同，日志为 `~/canter-linux/p2-160-linux-ca-final.log`。
- Mac 上一轮涉及的 7 个 crate 共 1990 项通过；本轮修改只涉及 canter/server，重新完整运行 `cargo nextest run -p canter -p canter-server --no-fail-fast`，655 通过、5 跳过。日志为 `/tmp/p2-160-mac-ca-final.log`，编译目录仍为指定的 p2-160-linux。
- 两端 `cargo clippy --all-targets -- -D warnings` 均退出 0，日志分别为 `/tmp/p2-160-mac-clippy.log`、5090 的 `~/canter-linux/p2-160-linux-clippy.log`。`cargo fmt --check`、`git diff --check` 通过。
- 验收 3：在 5090 的代码目录、指定 target 目录执行 `cargo test -p canter-tools --test sandbox_unavailable -- --nocapture`，1 项通过。实际打印 `Sandbox is not available on Linux yet. Set "sandbox": false in config.json or use --yolo.`；shell、read、write、edit 均拒绝，副作用断言通过。
- 验收 4：下述三个有 CA 容器、一个无 CA 容器全部通过。所有命令均在前台结束，未遗留本任务容器。

### 静态版和容器验收

按 DESIGN 的 Docker 命令重编译成功。产物仍在 5090 的 `~/canter-linux/.target/p2-160-linux/musl/x86_64-unknown-linux-musl/release/canter`，`file` 为 x86-64 static-pie，SHA256 为 `57e9f3dce743c8c5722fc7d2f53b3644e1c410e9b7cf4463ebcd8b06fb42f617`。版本输出中的 `3690057-dirty` 是独立验证仓库的基线标识，源码已从本 worktree 同步。

验收镜像的构建命令（在 5090 上前台运行，仅向自己的新镜像装包）：

```sh
docker build -t canter-p2-160-debian11-ca - <<'DOCKERFILE'
FROM debian:11
RUN sed -i 's@http://deb.debian.org/debian-security@http://archive.debian.org/debian-security@g' /etc/apt/sources.list \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates
DOCKERFILE
docker build -t canter-p2-160-ubuntu2004-ca - <<'DOCKERFILE'
FROM ubuntu:20.04
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates
DOCKERFILE
docker build -t canter-p2-160-alpine323-ca - <<'DOCKERFILE'
FROM alpine:3.23
RUN apk add --no-cache ca-certificates
DOCKERFILE
```

Rust 验收辅助程序保留在 `~/canter-linux/musl/smoke.rs`，在同一个 musl 编译镜像中用 `rustc --edition=2024 --target x86_64-unknown-linux-musl smoke.rs -o smoke` 编译。它使用隔离 HOME/CANTER_HOME 和本地假模型配置；容器禁网，检查版本、启动服务端、通过 Unix socket 请求 `/v1/info`，最后按子进程 PID 发送 SIGTERM 并等待退出。以下三次都通过：

```sh
for image in canter-p2-160-debian11-ca canter-p2-160-ubuntu2004-ca canter-p2-160-alpine323-ca; do
  docker run --rm --network none \
    -v "$HOME/canter-linux/.target/p2-160-linux/musl/x86_64-unknown-linux-musl/release/canter:/canter:ro" \
    -v "$HOME/canter-linux/musl/smoke:/smoke:ro" "$image" /smoke
done
docker run --rm --network none \
  -v "$HOME/canter-linux/.target/p2-160-linux/musl/x86_64-unknown-linux-musl/release/canter:/canter:ro" \
  -v "$HOME/canter-linux/musl/smoke:/smoke:ro" debian:11 /smoke --no-ca
```

三个装有 CA 的容器均打印版本及 `canter server ready`，`/v1/info` 返回 200、protocolVersion 1，SIGTERM 后正常退出。原始 `debian:11` 未装 CA 的检查也通过：打印规定错误、退出码 1，没有 panic、socket 或注册文件。

新增保留镜像为 `canter-p2-160-debian11-ca:latest`、`canter-p2-160-ubuntu2004-ca:latest`、`canter-p2-160-alpine323-ca:latest`；前述编译镜像和基础镜像继续保留。容器均用前台 `--rm`；未改宿主机包或已有容器。

### 没做的事和踩过的坑

没有实现 Linux 沙箱、systemd、发布升级流程或 P2-260。真实模型调用仍为 0 次；未接触两端真实 `~/.canter`，未带凭证。合并后主控的 DeepSeek 终端/exec 验收仍按原任务由主控执行。没有新增待主控决定的事项。

- Debian 11 的默认安全仓库能返回索引，但安装包 404；禁用 HTTP 缓存、切换 security.debian.org 均无效。对应 [Debian #1147093](https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=1147093)，改用官方 archive.debian.org 的相同发行版安全仓库后安装成功，未关闭包签名校验。
- Debian 基础镜像没有 `/bin/kill`。辅助程序最初在服务端已 ready、HTTP 已返回 200 后，因调用该外部命令失败；改为 Rust 直接按 PID 发送 SIGTERM，重新验收通过。
- 本轮第一次 Linux 全套中，现有 `mcp::http_mcp_ignores_server_proxy_environment` 偶发记录 2 个连接。单测重跑、完整 server 测试重跑、再加临时请求诊断的 10 轮完整 server 测试均通过，未能复现或确定原因。临时诊断已完全移除，未改 MCP 代码或该测试断言，保留这次观察供主控审查。

### 主控审查补充（2026-11-10）

1. 已核实并清理失败测试遗留的两个服务端。
   - PID 901321：远端启动时间 09:20:23，CANTER_HOME 为 `/tmp/canter-tui-ZmtBNR/state`；PID 981950：09:32:02，CANTER_HOME 为 `/tmp/canter-tui-IPntCI/state`。两者 PPID 均为 1，运行的是任务 target 目录里的旧 canter，临时目录已删除。
   - 从两个进程仍持有的会话文件描述符读到的 cwd 都是各自临时目录的 `somewhere/else/deep`，对应 `canter_opens_in_any_directory_and_starts_the_server`。`p2-160-baseline.log` 中该测试在读取未生成的 `env.txt` 时失败；`p2-160-linux-check.log` 中该测试因状态栏已显示 `Sandbox off`、旧断言仍等待原状态栏而超时，日志明确含 `IPntCI`。
   - 该测试由客户端自动拉起后台服务端，`World.server` 保持 None；末尾才按注册 PID 发 SIGTERM 并等待 ESRCH，因此中途断言失败会跳过清理。后续 `p2-160-remaining.log`、`p2-160-linux-final.log`、`p2-160-linux-ca-final.log` 均记载该测试通过，现存进程的启动时间也都早于这些成功运行。没有发现正常跑完仍遗留的情况；按主控要求不改 TUI 代码、不重跑 TUI 测试，仅记录失败路径。
   - 核对 `/proc/<pid>/exe` 后只对 901321、981950 发送 SIGTERM，并等待两者退出。随后以及本轮 eval 测试结束后，检查远端进程列表均确认没有 `canter serve`，未按名字批量杀进程。
2. `eval/tasks/long.toml` 和 `memory-global.toml` 的 node_modules 准备命令恢复优先 `cp -cR`：Mac 继续使用 clonefile；第一次复制失败才删除本次半截 node_modules，再执行 `cp -R`。已有 node_modules 仍直接保留，Linux 测试走兼容回退，不再令 Mac 每次整份复制。
3. 本轮只运行 canter-eval 测试：Mac `cargo nextest run -p canter-eval --no-fail-fast`，61 项全过；Linux `cargo test -p canter-eval`，19 项单测和 42 项集成测试全过，文档测试 0 项。继续使用前述独立 target、Node 和 npm cache，日志为 `/tmp/p2-160-mac-eval-review.log` 和远端 `~/canter-linux/p2-160-linux-eval-review.log`。这是任务配置修正，使用现有 prepare/grading 测试验证；未改其他 crate 或增加测试范围。`git diff --check` 通过，所有命令均在前台结束，无新增待定事项。
