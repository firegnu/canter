# 任务：工作区接口能拿到未跟踪新文件的内容和行数

2026-11-20 rein 主控转来需求，canter/main 出方案，用户定了行数和预算，rein/main 回复「满足」并要求在协议 README 写清超时和列表上限（主控已定，见要做的第 5 条）。
2026-11-20，canter/main 交给 canter/dev-p2-311（Codex，gpt-6.2-astra / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.44）、要、碰要害）。交叉审查交 Claude Code（opus[1m] / xhigh）。
类型：功能变更
依据：本轮给 workspace 的 diff、summary 加按需返回的新文件内容和行数，供 rein（iPad 客户端）显示；不改终端、不改 `/review`。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- rein 转来的需求：「GET /v1/sessions/{id}/workspace/diff 能拿到没加进 git 的新文件的内容」「按整份新增的 diff 给出（形状同 git diff --no-index /dev/null <文件>），有自己的截断预算，二进制文件只注明是二进制。最好 status 或 summary 也能拿到这类文件的行数，同样加新字段」「现有的 untracked 不要改」「作为新能力在 /v1/info 的 features 里声明，旧客户端照旧」
- 用户 11-20：「这个会破坏本地跑的canter tui吗？」（主控答：带参数才起作用，不带参数时走原来的代码、返回一模一样）「所以说这个是带参数才会起作用。对吧？」
- 用户 11-20：「行数两种都要，预算按你说的。」

## 先读

- `CLAUDE.md`（仓库根）的「规矩」。
- `crates/canter-server/src/workspace/git.rs`（`local`、`changed_lines`、`run`、`read_bounded`、`command`）和 `workspace/mod.rs`（`summary`、`diff` 两个处理函数）。
- `crates/canter-protocol/src/workspace.rs`。
- `crates/canter-protocol/README.md`「会话工作区只读查询（P2-299）」一节。

## 在哪里干活

- worktree：`../canter-worktrees/p2-311`，分支 `p2-311`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-311`。
- 测试里起真服务端的配置带 `"__test_session_environment": true`（照 `crates/canter/tests` 现有夹具），不读用户真实的 shell 配置。

## 要做的

1. **diff**：`GET /v1/sessions/{id}/workspace/diff?untrackedContent=true` 时，响应多两个字段：
   - `untrackedDiff: {text, truncated}`（沿用 `WorkspaceOutput`）；
   - `untrackedFiles: [{path, lines, binary}]`，每个新文件一项：`lines` 是整数或 null（超出计数预算、读不了时为 null），二进制文件 `lines` 为 0、`binary` 为 true。
   - 不带参数或 `untrackedContent=false` 时这两个字段不出现（不是 null，是没有这个键）。
2. **summary**：`GET /v1/sessions/{id}/workspace/summary?untrackedLines=true` 时多一个 `untrackedLines`：新文件总行数，整数或 null。超预算、文件列表被截断、git 失败或超时都给 null，不给只数了一部分的数。不带参数时不出现。超时和现有 `changedLines` 一样 2 秒。
3. **文本形状**照 `git diff --no-index /dev/null <文件>`：
   - `diff --git a/<p> b/<p>`、`new file mode 100644|100755|120000`、`--- /dev/null`、`+++ b/<p>`、`@@ -0,0 +1,N @@`，每行前加 `+`；末尾没换行加 `\ No newline at end of file`；空文件只有头；不写 `index` 行。
   - 二进制（前 8000 字节里有 NUL）只写头和 `Binary files /dev/null and b/<p> differ`。
   - 路径相对仓库根，和 staged/unstaged 的 `a/` `b/` 路径一致；文件集合就是同一次响应里 `untracked.text` 列出的那些完整行，在 git worktree 里同样成立。
   - 符号链接不跟进去，内容是链接指向的路径（和 git 一样），不读链接指向的文件。
   - 非 UTF-8 的文本按替换字符转成字符串。
   - 行数算法和 `git diff --numstat` 的新增行数一致。
4. **预算**：`untrackedDiff` 文本单独 256 KiB（和 staged/unstaged 的预算分开），超了截断、`truncated: true`；行数计数（`untrackedFiles.lines` 和 `untrackedLines`）读文件总量上限 8 MiB。读到预算就停，不把大文件整个读进内存。
5. **主控定的两点**（rein 要求写进 README）：
   - 超时：读新文件内容、数行数都算在 diff 现有的 15 秒里，不另加；最坏 15 秒返回，超时照现在返回 `collecting changes timed out`。
   - 列表上限：`untrackedFiles` 跟着 `untracked` 走，`untracked.truncated` 为 true 时 `untrackedFiles` 也不全，靠这个字段判断，不另加上限和标记。README 写明：服务端 `untracked` 的上限是 256 KiB（`LIMIT`），32 KiB（`LIST_LIMIT`）只是终端和 `/review` 自己再截的。
6. **兼容**：`/v1/info` 的 `features` 加 `untrackedDiff`，两个参数都靠它判断。不带新参数的请求，服务端走的代码和返回的 JSON 和改前一样；现有 `untracked`、终端未提交改动报告（`crates/canter-tui/src/uncommitted.rs`）、`/review` 的提示（`workspace/review.rs`）都不变。
7. **文档**：协议 README 那一节加上面的字段、参数、预算和第 5 条；DESIGN 第 8 条里 P2-299 那一段后面补一句这次的扩展和为什么（rein 要显示新文件内容）。协议生成文件（`crates/canter-protocol/generated/`）跟着更新。

## 怎么算做完

- 验收（用户原话）：「这个会破坏本地跑的canter tui吗？」——不带新参数时 diff、summary 返回的 JSON 和改前完全一样，现有终端 workspace 测试照过。
- rein 要的：带参数时能拿到新文件按整份新增的 diff、每个文件的行数和总行数；二进制只注明是二进制；有自己的截断预算；`features` 里有 `untrackedDiff`。
- 验证只做这些：
  - 针对这件活的测试：带参数的 diff/summary 形状（普通文件、末尾无换行、空文件、二进制、可执行文件、符号链接不跟进去）、截断、不带参数时没有新键；
  - `cargo nextest run -p canter-server`、`cargo nextest run -p canter-protocol`；TUI 只补编译需要的 `None`，跑 `cargo nextest run -p canter-tui`；
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - 不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改现有 `untracked`、`staged`、`unstaged` 的内容和预算，不改 `git::local`、`changed_lines` 现有的行为。
- 不改终端显示、不改 `/review` 发给模型的内容。
- 不放宽 git 的环境加固；新文件内容由服务端自己读文件拼，不对每个文件起一个 git 进程，也不跑仓库配置的 filter、textconv。
- 不跟随符号链接读仓库外的文件。
- 不针对某一家模型写特例。
- 不调真实的模型接口；测试不碰用户的 `~/.canter`。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务用起的时候记下的 PID。
- 碰到要拿主意的事（包括：git 的输出和上面写的形状对不上；某种文件类型不知道怎么处理），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-311` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 实现与兼容。服务端通过 `/v1/info.features` 声明 `untrackedDiff`。diff 可按需读取同一次 Git 列表的完整条目，返回整份新增的差异和逐文件行数。summary 可单独请求总行数。省略参数或传 false 时沿用旧响应。协议 README、DESIGN 第 8 条、JSON Schema 和 TypeScript 声明均已更新。
- 读取与预算。复用锁文件中的 rustix 1.1.5，从仓库根的目录描述符逐级打开路径，符号链接只取链接值。新文件流式计数，共享 8 MiB 读取预算。只为 diff 缓存独立的 256 KiB 预算所需的前缀。计数失败时 summary 返回 null，diff 保留逐文件记录。
- 主控决定。单行块头照 Git 写 `@@ -0,0 +1 @@`，两行及以上写 `@@ -0,0 +1,N @@`。当前文件超过剩余计数预算、无法确定 N 时，保留先前 diff 和当前文件的 `diff --git`、`new file mode` 两行，到此截断并置 `truncated: true`，逐文件记录保留、`lines` 为 null。协议 README 已写明这两项。
- 验证。先确认旧实现的两项 HTTP 验收测试因缺少新字段而失败。回归覆盖旧 JSON、worktree 子目录、普通与可执行文件、空文件、二进制、符号链接、转义路径、非 UTF-8 内容、末行缺换行、精确预算边界和截断的路径列表。三个指定包的 nextest 分别通过 canter-server 468 项（跳过 5 项）、canter-protocol 62 项、canter-tui 755 项。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。使用临时仓库、私有 socket 和隔离的会话环境，真实模型调用 0 次。
- 踩过的坑。实测 Git 对单行省略行数，已按主控决定固定在测试中。核对生成物时发现默认 Schema 把逐文件的 `lines` 也当成可省略，已明确生成必填且允许 null 的字段。
- 范围。TUI 只在测试夹具补了两个 `None`，终端展示和 `/review` 输入沿用原实现。按任务范围只在本分支提交，全仓测试、交叉审查、合并和推送留给主控。没有待主控决定的问题。
