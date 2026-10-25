# 任务：/context 和 /files

2026-10-25，canter/main 交给 canter/dev-context（Codex，gpt-6-astra / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.66）、拿不准、拿不准；后两项主控定：只读命令加一个只读接口，不改存盘、权限和发给模型的内容）
类型：功能变更
依据：第二期第 8 件（`docs/任务/P2-0-第二期需求单.md`）。本轮加两个只看不改的命令：`/context` 按类列出上下文里装了什么、各占多少；`/files` 列本会话读过、改过的文件和当前目录的 git 状态。不做文件的打开、编辑、diff 等操作。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-1
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-24 勾选第二期范围时：「pi 扩展里的 btw 顺口问、handoff 交接、context / files 查看、review 审查」。
- P2-0 需求单：「context / files：`/context` 看上下文里装了什么、占多少；`/files` 列本会话读过改过的文件和 git 状态」。
- 10-25：主控建议下一件做 P2-8，用户：「都照你的建议，写 P2-8 任务文件给我看」。

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/DESIGN.md` 第 8 条（协议，和 Codex 不同的地方要写进去）、第 17 条（发给模型的内容只往后加：本件只读，不能改请求内容）。
- `crates/canter-core/src/context.rs`：`prefix()`（系统提示词怎么拼：基础提示、全局和项目 `AGENTS.md`、技能列表）、`estimate` / `message_tokens` / `request_tokens`（现有的 token 估算）、`restore`（压缩后历史怎么恢复）。
- `crates/canter-core/src/runner.rs`：一次请求由哪些部分组成（前缀、压缩摘要、历史消息、工具定义）。
- `crates/canter-protocol/src/types.rs` 的 `Item::FileRead`、`Item::FileChange`（`FileUpdateChange` 的 `path`、`kind`、`diff`）、`TokenUsage.contextTokens`。
- `crates/canter-tui/src/app.rs` 里 `/skills`、`/usage` 怎么取数据、怎么显示在对话里；`crates/canter-tui/src/git.rs`（P2-6 读分支）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-8-context-files`，分支 `p2-8-context-files`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-8-context-files`。
- 可以动协议、核心、服务端、终端和相关测试、README、`docs/DESIGN.md` 第 8 条、本任务文件末尾的完成记录。

## 要做的

- **`/context`**：在对话里显示下一次请求的上下文按类各占多少，样子大致是：

  ```
  Context  ctx 42% 420k/1M (last request)
    System prompt                3k
    Global AGENTS.md             1k  ~/.agents/AGENTS.md
    Project AGENTS.md            2k  ~/canter/AGENTS.md
    Skills (5)                   1k
    Tools (9)                    4k
    Compaction summary          12k
    Messages (34)              395k  user 20k · assistant 80k · tool results 290k · images 5k
    Free                       580k
  Estimated by size; the first line is what the model reported.
  ```

  - 分类：基础系统提示词、全局 `AGENTS.md`、每个项目 `AGENTS.md`（各一行，带路径）、技能列表、工具定义、压缩摘要（没压缩过不写这行）、历史消息（再分用户、助手、工具结果、图片）、剩余。
  - 各类的数用现有的估算办法算（和压缩判断用的同一套），不另调模型接口。第一行照状态栏的写法用模型报回来的实际占用；还没请求过就写 `no request yet`，各类照样列。
  - 数据由服务端算（只有服务端知道这次会发什么），加一个只读接口给终端取；接口只读，不改会话状态，也不改下一次发给模型的内容。
- **`/files`**：在对话里列出：
  - 本会话读过的文件（来自 `FileRead`），去重，按首次出现的顺序。
  - 本会话改过的文件（来自 `FileChange`），去重，带改动类型（新增 / 修改 / 删除）。
  - 当前会话目录的 git 状态：分支名，加 `git status` 列出的有改动和未跟踪的文件；不在 git 仓库里就写一句说明。
  - 路径在会话目录下的写相对路径，其余写 `~` 缩写的绝对路径。没有读过或改过的，那一节写 `none`。
  - 模型用 shell 命令改的文件不在「改过的」里（canter 认不出来），git 状态那一节能看到；这点写进 `/help` 或输出末尾的一句说明。
  - 跑 `git status` 时不拿锁（`GIT_OPTIONAL_LOCKS=0`），关掉会起别的程序的设置（比如 `core.fsmonitor`），设超时；失败就写一句说明，不报错退出。
- 两个命令都加进 `/` 命令列表和 `/help`，窗口窄时照现在 `/skills` 的办法截断。

## 怎么算做完

- `/context` 按类列出上下文里各部分占多少 token 和窗口还剩多少；`/files` 列出本会话读过、改过的文件和当前目录的 git 状态。（主控拟，用户看过任务文件后：「验收照你拟的，派吧」）
- 验证只做这些：
  - 针对验收写测试，先写、确认在现在的代码上失败：用假模型跑一个有 `AGENTS.md`、技能、读文件、改文件、压缩过的会话，`/context` 的各类都在、数和估算函数算出来的一致、调用接口前后下一次请求的内容不变；`/files` 读过、改过的列表去重正确，git 状态用临时目录建的仓库测（有改动、有未跟踪、不在仓库里三种）。
  - 做完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的 `CARGO_TARGET_DIR`）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不做 pi 那种文件浏览器的操作（打开、编辑、Quick Look、diff、加进输入框），只列出来。
- 不调真实模型接口，不读用户真实的 `~/.canter`、`~/.agents`、git 仓库和配置；测试用隔离的 HOME、`CANTER_HOME`、临时 socket，git 用临时目录建的仓库，跑 git 时 `GIT_CONFIG_GLOBAL=/dev/null`、`GIT_CONFIG_NOSYSTEM=1`；不碰用户正在跑的服务端（别读、别停它的 pid）。
- 不改发给模型的内容和顺序，不改 token 估算办法本身。
- 不升级依赖，要加新依赖的，停下来问。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事（包括要动上面范围以外的文件、协议要改已有字段的意思），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-8-context-files` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

### 做了什么

- 新增只读 `GET /v1/sessions/{id}/context`，接入协议类型、Schema、TypeScript、服务端和终端 `/context`。按基础系统提示、全局指令、每层项目指令、模型可调用技能、工具定义、压缩摘要、用户/助手/工具结果/图片消息分类，显示估算与剩余窗口；第一行沿用状态栏的实测占用，无用量时显示 `no request yet`。
- 打开会话时只在内存记录前缀各段估算；压缩及恢复时记录摘要在历史中的位置。普通请求构造提取为查询和运行循环共用的函数，消息、工具、选项及顺序不变；原 token 估算函数和存盘格式未改。
- 新增 `/files`：从完成的 FileRead/FileChange 项目汇总，按首次出现顺序去重，同一路径保留曾发生的各类改动。会话目录内显示相对路径，其余缩写 HOME；空列表显示 `none`。git 输出分支及已改动/未跟踪文件，失败或非仓库时显示说明，并提示 shell 改动只在 git 状态中可见。
- git 查询设统一两秒超时、`GIT_OPTIONAL_LOCKS=0`，隔离全局/系统配置，关闭 fsmonitor、hooks、pager、untrackedCache 及主仓库/子模块的外部内容过滤器。两个命令加入 `/` 和 `/help`，显示沿用 `/skills` 的单行截断。补充 README、协议 README 和 DESIGN 第 8 条。

### 验证了什么

- 先写并运行验收测试，确认原有代码因缺少上下文路由、命令列表项和文件汇总输出而失败，再实现。自查时还在同一个临时仓库用例中复现了 clean filter 会执行外部命令的问题，修复后确认不会生成标记文件。
- 假模型会话覆盖全局及两层项目 AGENTS.md、技能、真实工具读写和压缩；逐项估算与模型请求一致，接口前后快照相同，下一次请求的消息、工具及选项完整符合预期。核心测试直接用现有 `request_tokens`、`message_tokens`、`estimate` 校验，包括图片和压缩摘要，查询前后构造出的请求完全相同。
- 终端测试覆盖命令与帮助、首次无请求、分类与实测占用、窄屏截断；文件用例覆盖绝对/相对路径去重、首次顺序、多种改动类型、HOME 缩写，以及临时仓库中的修改、未跟踪文件和非仓库目录。
- 最终代码通过 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`；所有 Cargo 命令均使用任务指定的独立 `CARGO_TARGET_DIR`，命令均在前台运行并等待结束。另通过 `git diff --check`，协议导出产物与生成器一致。

### 拿主意的地方

- 前缀系统消息的角色/JSON 开销计入 System prompt，其余段取追加前后的估算差值，保证各项精确相加；技能只计实际列入模型前缀的条目。Free 是窗口减估算输入，不额外扣除输出预留。
- 查询显示当前已装入上下文，不猜测尚未消费的排队输入及未来工具结果，输出和协议文档均说明这一点。文件历史只计完成的操作，类型是会话中发生过的操作，不推断最终净改动。
- git 关闭重命名探测，重命名按删除/新增列出；关闭内容过滤器是保证只读查询不执行仓库配置程序所必需的，因此依赖过滤器归一化的文件可能保守地显示为改动。未增加依赖，未改已有协议字段含义，无需主控另作决定。

### 没做的事

- 没有文件打开、编辑、diff 或文件浏览器；没有新增持久化状态，没有改 token 估算规则或发送给模型的内容。
- 真实模型调用 0 次；测试均使用隔离目录、环境和 socket，未使用用户会话或常驻服务端。没有安装、部署、合并 main 或推送。
- 未复制外部项目代码；实现沿用本仓库的上下文、斜杠命令和显示机制。

### 踩过的坑

- macOS 的 git 将 `/var` 路径解析为 `/private/var`，直接按会话目录字符串去前缀会错误显示绝对路径；git 结果改用解析后的目录和 HOME 比较，临时仓库用例验证通过。
- 只关闭 fsmonitor 不够，`git status` 校验文件内容时仍会运行 clean/process filter；现先只读枚举过滤器配置，再通过本次 git 命令参数关闭，子模块使用同一组覆盖，所有步骤共用超时。
