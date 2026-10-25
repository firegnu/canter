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
