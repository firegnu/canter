# 任务：Terminal-Bench 2.1 接入 canter（第一步：适配器和试跑）

2026-11-10，canter/main 交给 canter/dev-tbench（Codex，`gpt-6-astra` / xhigh）。交叉审查交 Claude Code（`opus[1m]` / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：常规（置信 0.25）、要、碰要害；推翻：常规和重各 0.5，要在用户的工作站上装框架、碰 key 和 Docker，按重派）
类型：功能变更
依据：P2-151「11-09 Terminal-Bench 排在第一版之后」；11-10 用户定用官方 Harbor 跑 Terminal-Bench 2.1，适配器和结果放进仓库的 `bench/terminal-bench/`。本件只做第一步：装好、写好适配器、用 oracle 查环境、canter 真跑 3 道题、估出全套花费和时间。全套 89 题和 Terminus-2 那一轮是第二步，主控在用户在场时做，不归本件。
依赖：P2-257、P2-258、P2-259、P2-160、P2-272
执行：派出
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09：「记进去，第一版跑通后接着做」
- 11-10（看完 benchmark 评估）：「我觉得可以先做160。」「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.」
- 11-10（问过能不能在 macOS 上跑、能不能接进现在的 eval 架构）：「还是走正常的bench吧。不要投机取巧了」
- 11-10（主控提：适配器和结果放进仓库单独的 `bench/terminal-bench/`，给「不用 Python」开这一个目录的例外）：「这两条都同意」
- 11-10（主控推荐：开发真跑 canter 3 道题；第一轮比 canter 和 Terminus-2；全套跑之前先报估算、用户同意再跑；题目镜像跑完前留着）：「四个问题按你的推荐来」
- 11-10（派出后，用户问会不会有泄题；主控建议每轮跑完扫会话记录，查有没有访问 Terminal-Bench 题库地址，查到的逐题看、抄答案的单列并按没做出来算，canter 和 Terminus-2 同一套规则；先在第一步做出来）：「加吧，按你的建议来」

## 背景（主控 11-10 摸的底）

- Harbor 是 Terminal-Bench 官方的运行框架：每道题起一个 Docker 容器，把外壳装进去，把英文题目交给它，外壳自己干完（或超时）后 Harbor 跑隐藏的测试，全过得 1 分；总分是通过题数 ÷ 题数。2.1 是 2.0 的修订版（26 道题修过），数据集名 `terminal-bench/terminal-bench-2-1`。
- 自定义外壳用 `harbor run ... --agent-import-path <模块>:<类>` 接入，类继承 `BaseInstalledAgent`：`install()` 装外壳（能做进镜像的步骤），`configure()` 每次运行前的准备（写凭证、上传文件），再实现运行和事后取用量。环境接口有 `upload_file`、`exec`、`download_dir` 等。环境变量用 `--ae KEY=VALUE` 传给外壳。Harbor 自带的 `pi`、`opencode`、`claude_code` 外壳可以照着看。主控只读克隆在 Mac 的 `/tmp/r49-src/harbor`（`ec66182`），你在 5090 上需要读源码就自己克隆到 `/tmp` 下，只读。
- 5090（`my-5090d-host`，Ubuntu 24.04，x86_64，32 核，60G 内存，磁盘剩 1.3T）：Docker 29.8，当前用户能直接用；有 Python 3.12，没有 `uv` 和 Harbor。机器上有用户自己的 Docker 容器和镜像。
- canter 的 musl 静态版编法见 `docs/DESIGN.md`「Linux 支持（P2-160）」。题目容器要先装 `ca-certificates`，缺了服务端报错退出。
- DeepSeek key：用户同意本件真调模型（canter 3 道题）。key 在 5090 的 `~/canter-linux/accept-home/.canter/config.json`（验收用的测试 HOME，`providers.deepseek.api_key`），用 `jq` 读进环境变量传给 Harbor，不打印、不写进任何文件。

## 先读

- `docs/DESIGN.md`「Terminal-Bench（P2-260）」「Linux 支持（P2-160）」两节
- `CLAUDE.md`「语言与依赖」那条 Python 例外
- `docs/任务/P2-160-第三期-Linux支持.md` 完成记录里 5090 的同步办法和静态版编译
- Harbor 源码：`src/harbor/agents/installed/base.py`，和一个自带外壳（比如 `pi.py`）

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-260-terminal-bench`，分支 `p2-260-terminal-bench`（已从 main 建好）。只动 `bench/terminal-bench/` 和本任务文件。
- 5090：只用 `~/canter-linux/` 下面。代码放 `~/canter-linux/p2-260-terminal-bench`，同步办法同 P2-160。`uv`、Harbor、Python、uv 缓存都装在 `~/canter-linux/` 下（用 `UV_INSTALL_DIR`、`UV_TOOL_DIR`、`UV_TOOL_BIN_DIR`、`UV_CACHE_DIR`、`UV_PYTHON_INSTALL_DIR` 这类变量指过去，不改 shell 配置文件）。Harbor 自己的缓存和结果目录也尽量指到 `~/canter-linux/` 下；指不过去的写进完成记录。Docker 镜像落在系统的 Docker 目录，这个没办法，照常。

## 要做的

- 在 5090 上装 `uv` 和 Harbor，Harbor 钉一个具体版本（用当时最新的正式版），写进 README。
- 用分支起点那个干净提交编 canter 的 musl 静态版，`canter --version` 写进结果数据。
- 写 `bench/terminal-bench/` 下的适配器（Python，只做衔接）：每道题把静态版拷进容器、按容器系统装 `ca-certificates`、把配置写进容器里的 `~/.canter/config.json`（服务商、模型、思考档从 Harbor 的参数和环境变量来，不写死）、运行 `canter exec --yolo "<题目>"`、结束后用 `canter stats --last --json` 取 token 和花费交给 Harbor、把 canter 的会话记录存进 Harbor 这道题的日志目录。
- 写 `bench/terminal-bench/README.md`：在任意一台 x86_64 Linux（有 Docker）上从零跑起来的步骤：装 uv 和钉住版本的 Harbor、编静态版、key 用哪个环境变量传、先 oracle 查环境、再试跑几题、再跑全套的命令；Terminus-2 用同一个模型跑的命令也写上（本件不跑）；用 `jq` 比较两轮或两个外壳的几条命令。
- `bench/terminal-bench/results/`：每一轮一个纯数据文件（JSON 或 CSV），记 canter 版本、外壳、模型、思考档、Harbor 版本、日期、总分、每题过没过、用时、token、花费。本件只有试跑这一轮。
- 用 Harbor 自带的 oracle 外壳（直接用标准答案，不调模型）跑 2～3 道题，确认环境正常；再用 canter 真跑 3 道题，挑耗时短的。真跑的题里最好有一道是 Alpine 镜像，顺带看 canter 的看护进程在 busybox ash 上杀进程组正不正常（P2-160 留下的）；没有 Alpine 题就说明。
- 按试跑数字估出全套 89 题跑一轮的花费和时间（写清按什么算的），写进完成记录。
- 偷看答案检查（派出后用户加的）：写一个检查办法（优先用 README 里的 `jq`、`grep` 命令加纯数据的规则文件；确实要写程序就放在 `bench/terminal-bench/` 的 Python 里，不引入别的语言），每轮跑完扫每道题外壳的会话记录（网页抓取、搜索、shell 命令和输出里的网址），找访问 Terminal-Bench 题库或答案来源的痕迹（比如 `github.com/harbor-framework`、`terminal-bench` 相关仓库、`tbench.ai`、Hugging Face 上的 `harborframework` 数据集）。输出每题有没有命中、命中了什么，写进这一轮的结果数据。同一套规则要能用在 Terminus-2 的日志上（日志格式不同就写清两种各怎么查；本件只在 canter 试跑上实际查）。命中的题怎么判由主控逐题看，适配器和检查不自动改分。

## 怎么算做完

- 验收（主控建议，用户 11-10：「四个问题按你的推荐来」）：
  1. 5090 上 oracle 跑的 2～3 道题都通过。
  2. canter 真跑 3 道题，每道都走完整个流程：装进容器、跑完、Harbor 判分、token 和花费记进 Harbor、会话记录存下来。题做没做对不要求。
  3. `bench/terminal-bench/` 有适配器、README、`results/` 里这次试跑的数据文件；照 README 能在另一台机器上跑起来（不用真去另一台机器，步骤写全即可）。
  4. 完成记录里有全套一轮的花费和时间估算。
  5. 偷看答案检查在 3 道试跑上实际跑过，结果写进这一轮的结果数据（用户 11-10：「加吧，按你的建议来」）。
- 验证只做这些：上面的 oracle 和 canter 试跑；`git diff --check`。不跑 canter 的 Rust 测试（本件不改 Rust 代码）。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 canter 的代码（`crates/`、`eval/`、`Cargo.*`）。跑出 canter 自己的问题，记进完成记录、在回复里报给主控，不在本件里修。
- 不投机取巧（用户原话）：不改题目、不放宽超时和资源、不让 canter 看到测试或标准答案、不按某道题调 canter 的提示或配置。canter 用默认配置跑，只配服务商和模型。
- 不跑全套 89 题，不跑 Terminus-2 或别家外壳（第二步主控做）。真调模型的题目运行一共不超过 6 次（3 道题，加修适配器后重跑最多 3 次），次数写进完成记录。
- key 不打印，不写进宿主机文件、日志、结果数据、提交或镜像构建层。11-10 主控纠正原来「不写进任何文件」与容器配置要求的矛盾：允许仅临时写入单题容器的 `~/.canter/config.json`（0600），在容器内生成，不采集，取完 stats 和会话后清掉，最后由 Harbor 删除容器。查一下 Harbor 自己的结果目录（包括异常日志）是否会存下凭证，选择不让 key 进入宿主机文件的传法；每次真跑后，在 Harbor 结果目录和 `~/canter-linux` 下用 `grep -rlF` 查 key，只打印文件名，查到就停下报主控。决定及检查结果写进完成记录。
- 不上传任何东西到 Harbor Hub 或别的服务，不登录 Harbor 的账号。
- 适配器不写死 DeepSeek：服务商、模型、地址、key 的环境变量名都从参数来；DeepSeek 只是这次跑的配置。
- Python 只放 `bench/terminal-bench/`，不进 cargo 工作区。
- 5090 上只用 `~/canter-linux/`，不用 sudo、不装系统包、不改 shell 配置文件；不碰机器上已有的容器和镜像，不 `docker system prune` 这类会删别人东西的命令；Harbor 自己起的容器跑完由 Harbor 清。不读、不改两台机器上真实的 `~/.canter`。
- 不按名字批量杀进程（`pkill -f canter`、`pkill -f docker` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如 Harbor 的接口和这里写的不一样、某种容器里 canter 起不来、key 的传法），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送到 `origin`（推到 5090 上自己的裸仓库可以）。只在 `p2-260-terminal-bench` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 阶段记录（2026-11-10，未完成，等待主控决定）

### 做了什么、验证了什么

- 根目录没有 AGENTS.md，已先读 CLAUDE.md，再读本任务及指定的 DESIGN、P2-160 完成记录；当前分支起点是干净提交 `64b80e3f963f0005f69f078e68a4af71a611ca28`。
- 只读检查 5090：SSH 可连接，架构 x86_64，Docker 29.8.0，已有 jq、curl、git；未安装工具、同步代码或启动容器。
- [PyPI](https://pypi.org/project/harbor/) 的 JSON 元数据确认当前正式版为 `0.25.0`，要求 Python >=3.12。阅读主控已有的只读源码 `/tmp/r49-src/harbor`（`ec6618290a90aa4ba2cfd08bd91b2f8a1afa760a`）中的 `BaseInstalledAgent`、`pi`、环境变量序列化和 Docker 执行接口；只参考，没有复制代码。尚未安装并核验正式版包的实现。
- 凭证路径静态核对发现下述冲突，因此按本任务停工规则结束本轮。没有读取测试 HOME 里的 key，更没有读取两台机器的真实 `~/.canter`。真实模型题目运行 **0 次**，oracle **0 次**。本轮文档变更用 `git diff --check` 检查，不跑 Rust 测试。

### 要主控决定：容器内配置是否允许含 key

任务一方面要求把配置写进容器的 `~/.canter/config.json`，另一方面规定「key 不打印、不写进任何文件、日志、结果数据和提交」。当前 canter 无法同时满足这两条：

- `crates/canter-server/src/config.rs` 的 `Config::load_impl` 直接从 `config.json` 读取并反序列化；`Provider.api_key` 是字符串，没有环境变量引用或替换。
- 同文件 `Provider::validate` 要求非空 key；`Provider::client` 直接把该字符串传给 `ApiKey::new`。把 `${KEY_ENV}` 写进配置会成为字面量凭证，不会读取环境变量。
- 本件明确禁止修改 canter 代码，所以不能在此添加从环境变量读取 key 的功能。

可选办法：

1. **建议**：主控明确允许 key 仅临时存在于单题容器的 `~/.canter/config.json`（0600），通过运行时环境传入、在容器内生成，不在宿主机准备含 key 的文件；不采集配置，取完 stats 和会话后清除凭证，最后由 Harbor 删除容器。仓库、宿主机文件、日志、结果和镜像构建层仍不含 key。这符合任务原先的配置写入流程，只需明确容器临时文件的例外。
2. 保留「任何文件都不能含 key」的字面要求：先另开 canter 环境变量凭证支持任务，完成后再继续本件；本分支不修改 Rust。

还需在主控决定后完成 Harbor 正式版的凭证落盘核验：已读的 `ec661829` 源码会把敏感 `AgentConfig.env` 值序列化为环境变量引用或掩码，但 Docker `_run_docker_compose_command` 的非零退出异常包含完整命令，`_compose_exec` 又把环境变量展开为 `-e KEY=value`。因此不能仅凭正常结果 JSON 的脱敏就断言所有错误日志安全；后续适配器必须处理这条异常路径，并确认日志中没有 key，才可真跑。

### 没做的事、踩过的坑

- 未安装 uv/Harbor、未编静态版、未写适配器或 README、未产生结果数据与全套估算；未跑 89 题或 Terminus-2。没有合并 main、没有推送 origin。所有已执行的命令均在前台结束，无本任务遗留进程或容器。
- 实现和试跑尚未开始，没有尝试后换方案的坑；本轮发现的是任务约束与现有凭证接口的冲突，未自行采用替代办法。

## 完成记录

**当前状态：续跑尚未完成，等待主控处理下述 Harbor 正式版接口差异。** 前面的阶段记录保留，凭证矛盾已由主控解决。

### 主控决定（2026-11-10）

主控确认原任务两条凭证要求自相矛盾，采用方案 1：允许 key 临时存在于单题容器的 `~/.canter/config.json`（0600），在容器里生成，不采集这个文件，取完 stats 和会话后清掉，最后由 Harbor 删除容器。宿主机的仓库、日志、Harbor 结果目录（包括异常日志）、结果数据和镜像层均不能新增含 key 的文件。主控授权适配器自行选择避开 Docker `-e KEY=value` 异常泄露的传法；每次真跑之后在 Harbor 结果目录和 `~/canter-linux` 下用 `grep -rlF` 检查，只输出文件名，查到就停下报告。

选定的传法是标准输入管道：适配器从指定的宿主机环境变量读取 key，构造配置的内存字节，通过 Docker compose 的 `exec -T` stdin 交给容器内的 `cat > "$HOME/.canter/config.json"`。不使用 `--ae KEY=value`，不把 key 放进命令行或本地临时文件；Docker 异常中的命令因此没有 key。Harbor 0.25.0 的 Docker 后端 `_run_docker_compose_command` 支持 `stdin_data`，自带 tar 上传也使用这个通道。此传法尚未实现或真跑验证。

### 续跑做了什么

- 在 5090 的 `~/canter-linux/uv-bin` 安装 uv `0.13.7`，以 `UV_NO_MODIFY_PATH=1` 禁止修改 shell 配置；Harbor `0.25.0` 安装在 `~/canter-linux/uv-tools/harbor`，`harbor --version` 确认版本。工具、uv 缓存和临时目录均指向 `~/canter-linux`，Harbor 命令设置 `HARBOR_TELEMETRY=0`。当前工具虚拟环境使用已有的 `/usr/bin/python3.12`；后续仍需按任务要求改为 `~/canter-linux/python` 下的 uv managed Python。
- 创建专用裸仓库 `~/canter-linux/p2-260-source.git`，只推送分支起点 `64b80e3f963f0005f69f078e68a4af71a611ca28`；克隆至指定的 `~/canter-linux/p2-260-terminal-bench`，核对 HEAD 和工作区干净后启动 musl 编译。使用既有编译镜像 `canter-p2-160-musl`，独立 target 为 `~/canter-linux/.target/p2-260-terminal-bench/musl`，构建日志为 `~/canter-linux/p2-260-musl-build.log`。
- 静态编译已在前台正常结束（退出 0），产物为 `~/canter-linux/.target/p2-260-terminal-bench/musl/x86_64-unknown-linux-musl/release/canter`；版本 `canter 0.1.0 · 64b80e3f`，没有 dirty 后缀；`file` 确认 x86-64 static-pie，SHA256 为 `4f813abe5e46235b8b93856e901ccbdd3a9c8ff17e4f9aeb78dabb10e6240a4a`。编译容器以 `--rm` 前台运行并已退出，未新增或改动镜像。
- 核对测试 HOME 配置时只输出 model、base_url 和模型能力/价格字段，不输出或传递 key。未读取两台机器的真实 `~/.canter`。

### 要主控决定：正式版没有 configure 钩子

安装后核对的真实代码位于 `~/canter-linux/uv-tools/harbor/lib/python3.12/site-packages/harbor/agents/installed/base.py`：0.25.0 的 `setup()` 仅调用 `install()` 和检测版本，没有 `configure()`。主控预读的 `ec661829` 则已有 `configure()`，且支持跳过 install 后仍 configure。两者元数据都显示 0.25.0，不能据此把主分支源码当作正式包接口。

1. **建议**：仍钉最新正式版 0.25.0。在适配器覆盖 `setup()`，先 `await super().setup(environment)` 安装和检测版本，再调用自己的 `configure()` 写入临时配置；不改 Harbor、不改 canter。这样仍明确区分安装和每题凭证准备，凭证不会进入镜像构建层。
2. 改钉主控预读的未发布提交，直接使用其 configure 钩子；这会偏离「最新正式版」要求，不建议。

按本任务「Harbor 的接口和这里写的不一样」的停工规定，尚未自行选择兼容实现。真实模型题目运行仍为 **0 次**，oracle **0 次**，没有模型花费或试跑结果可供估算；每次真跑后的 grep 检查尚未触发。未跑 Rust 测试、未合并 main、未推送 origin。

本轮 `git diff --check` 通过，所有命令均已在前台结束。踩过的坑：正式包的版本号与主控预读源码相同，但安装生命周期接口不同；以后应以钉住的正式包源码为准。
