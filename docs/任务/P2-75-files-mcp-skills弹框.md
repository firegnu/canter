# 任务：/files、/mcp、/skills 改成弹框并重新排版

2026-10-27，canter/main 交给 canter/dev-reportpopup（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.98）、不要、看得见；推翻：改成弹框会改按键处理，不只是样子，按改行为验）
类型：样式／文案调整
依据：三个命令的结果现在是同一颜色的纯文字，画进对话区，挤成一坨；照用户选定的样子分节上色，并改成和 `/context`（P2-74）一样的弹框。只改终端界面。
提示：沿用现有视觉和用语约定，聚焦指定的呈现结果。
执行：派出
依赖：P2-74
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27 用户（附了一张截图：`/files` 和 `/mcp` 连着用后，对话区里一大段同色文字，MCP 每个工具写成 `短名 (mcp__服务__短名)`）：「files mcp的命令输出也太难看了。包括skills，好歹设计一下。都挤成一坨了。」
- 主控给了下面三个样子，问了两处，用户都选推荐：三个都和 `/context` 一样弹框；`/mcp` 的工具平铺成几行。

## 样子（用户看过的）

规矩：第一行是标题（加粗）加数量摘要；节之间空一行；节标题加粗；次要信息灰色；最后一行 `Esc close`（和 P2-74 一样）。

`/files`：

```
 Files  read 2 · changed 0 · git 11

 Read
   docs/architecture.md
   src/app.js

 Changed by canter
   (none)

 Git status · main
   modified    README.md
   modified    cli.js
   added       docs/architecture.md        staged
   added       examples/simple-cli.js      staged
   deleted     simple-cli.js               staged
   untracked   .claude/settings.local.json
   untracked   hello.txt

 Edits made through shell commands only show up under Git status.
```

`/mcp`：

```
 MCP  3 services · 31 tools

 ● context7     connected · 2 tools
   query-docs  resolve-library-id

 ● everything   connected · 15 tools
   echo  get-annotated-message  get-env  get-header-params
   get-resource-links  get-resource-reference  get-structured-content
   get-sum  get-tiny-image  gzip-file-as-resource  …

 ○ github       disconnected
   spawn npx ENOENT

 Tools are called as mcp__<service>__<tool>.
```

`/skills`：

```
 Skills  3 · global 2 · project 1

 Global  ~/.agents/skills
   corral
     Start a separate agent session with corral, hand it the question…
   handoff
     记录当前会话的关键信息，为新会话做准备

 Project  .agents/skills
   release                         not for model
     Build and install a release, then restart the server…
```

## 先读

- `docs/DESIGN.md` 第 8 条「上下文查询（P2-8）」那段末尾 P2-74、P2-75 两句。
- P2-74 做好的 `/context` 弹框（合并后在 main 上；看它的任务文件 `docs/任务/P2-74-context弹框.md` 和代码）：三个命令照它的弹框、按键、滚动来做，能共用的就共用。
- 现在的输出：`crates/canter-tui/src/files.rs`、`crates/canter-tui/src/commands.rs` 的 `mcp`、`skills`；`theme.rs` 里现有的颜色。
- `crates/canter-tui/README.md` 讲这三个命令的地方。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-75-reportpopup`，分支 `p2-75-reportpopup`（P2-74 合并后从 main 建好）。
- 只动 `crates/canter-tui/`（代码、测试、`tests/expected/` 下的期望画面、README）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-75-reportpopup`。

## 要做的

- **弹框**：三个命令的结果都不再写进对话区，改成和 P2-74 的 `/context` 一样的弹框，打开、关闭、按键、草稿保留、矮窗口滚动（第一行标题和最后几行说明、`Esc close` 固定，中间滚）都一样。
- **颜色**：只用 `theme.rs` 里已有的颜色（16 色终端也要分得清）。标题和节标题加粗；次要信息（数量后缀、路径前缀、staged、描述、说明行）用灰色。
- **`/files`**：
  - 三节 Read、Changed by canter、Git status · 分支名；节标题后面不写数量，数量在第一行。空的节写 `(none)`。不在 git 仓库里时第三节写 `Git status` 加 `(not a git repository)`。
  - Changed by canter 每行前面写 added / modified / deleted（现在写的是中文「新增 / 修改 / 删除」），一个文件改过几种时用 ` / ` 连起来，和现在一样。
  - Git status 把 git 的两位代码换成词：modified、added、deleted、renamed、untracked、conflict（`??` 是 untracked，`U` 或 `AA`、`DD` 是 conflict），词那一栏对齐。只暂存了的标灰色 `staged`，暂存后又改过的标 `partly staged`。词的颜色：modified 黄，added 绿，deleted 红，renamed 和 untracked 灰，conflict 红加粗。
  - 最后一行说明换成 `Edits made through shell commands only show up under Git status.`
- **`/mcp`**：
  - 第一行 `MCP  N services · M tools`。
  - 每个服务一行：连上了是绿色 `●`，后面是服务名（名字一栏对齐）和灰色的 `connected · N tools`。没连上是红色 `○` 和 `disconnected`，下一行缩进写出错原因。warnings 照现在的内容，缩进写在服务下面，用灰色。
  - 工具只写短名，两个空格隔开，铺满弹框宽度再换行；一个名字比整行还长时单独占一行，照现有规则截断。
  - 最后一行 `Tools are called as mcp__<service>__<tool>.`
  - 没配服务时照现在的 `No MCP services configured.`
- **`/skills`**：
  - 第一行 `Skills  N · global X · project Y`。
  - 按 Global、Project 分两节，节标题后面用灰色写目录（`~/.agents/skills`、`.agents/skills`），没有的那节不写。
  - 每个技能名字一行，用强调色；不给模型用的在同一行右侧写灰色 `not for model`。描述另起一行，多缩进两格，灰色，最多两行，超出用 `…` 截。
  - 没技能时照现在那四行说明。
- README 里这三个命令的说明跟着改。

## 怎么算做完

- （用户 10-27）`/files`、`/mcp`、`/skills` 照上面的样子分节上色，不再挤成一坨；和 `/context` 一样弹框；`/mcp` 的工具平铺成几行。
- 验证只做这些：
  - 三个命令各一张弹框画面的测试（用上面样子里的数据或等价的假数据），先写、确认在现在的代码上失败。另加：`/mcp` 有没连上的服务；`/files` 的 git 状态几种词和 staged / partly staged；`/skills` 描述超过两行时截断；关掉后对话区里没有这几个命令的行。
  - 现有画面测试和期望画面里，因为这三个命令改了样子而变的，逐屏看过，在完成记录里说明为什么；别的不应变。
  - 做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的编译目录）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `/context`（P2-74 已定），不改别的命令；不改服务端、协议、核心；三个命令拿到的数据不变，只改怎么画。
- 不加新颜色、不加依赖、不升级已有依赖。
- 不读用户真实的 `~/.canter`、`~/.agents` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，不碰用户正在跑的服务端（别读、别停它的 pid）。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括样子里没写到的显示细节、窄窗口下对齐放不下），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-75-reportpopup` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- **做了什么**：把 P2-74 的 `/context` 弹框抽成共用的 `report.rs`（标题、中间各行、说明行，按键、滚动、`↑/↓ N more` 规则原样搬过来），中间各行按窗口宽度现排（单行截断、工具名铺行、描述折行最多两行）。`/context` 改用它，画出来和原来一样；`/files`、`/mcp`、`/skills` 也改成打开这个弹框，不再写对话区，按键、草稿、互相顶掉都和 `/context` 同一套。`git.rs` 的 status 改成返回分支和各条（代码、路径），跑的 git 命令不变；`files.rs`、`commands.rs` 的 `mcp`、`skills` 照样子分节上色，颜色只用 `theme.rs` 已有的（modified 用 `warning`，added `success`，deleted `failure`，conflict `failure` 加粗，灰色用 `secondary`，技能名 `accent`）。原来只有这三个命令用的「对话区单行截断」开关没人用了，一并删掉。README 加了三个命令的说明。
- **验证了什么**：新增 `tests/report_popup.rs` 五个测试：`/files` 整框画面（合并冲突中的仓库：modified、partly staged、conflict、staged 的 added/deleted、未暂存 deleted、untracked，加颜色和粗细）、不在仓库里的 `(none)` 和 `(not a git repository)`、`/mcp` 整框（有没连上的服务、warning、工具铺行、超长名字单独一行截断，加颜色）、`/skills` 整框（两节、`not for model`、描述超过两行截断、中文描述，加颜色）、三个命令 Esc 关掉后整屏和打开前一样。先在原代码上跑，五个都因为没有弹框失败，实现后通过。`context_popup.rs` 四个没改照样过。全套：`cargo nextest run --all-targets` 626 个全过，`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 都过。
- **现有测试的变化**（`slash.rs` 三个，`tests/expected/` 没变，都逐屏打出来看过）：
  - `files_lists_deduplicated_history_and_isolated_git_status`：只换了要找的字（`Read files`→标题行和 `Read`，`新增 / 修改`→`added / modified`，`Git: main`→`Git status · main`，` M`/`??`→`modified`/`untracked`，说明行换新句，`none`→`(none)`），原来查的去重、`~` 缩写、不执行 fsmonitor 和 filter、窄窗口截断都还在。
  - `skills_show_names_sources_and_manual_marker_in_single_truncated_rows` 改名 `skills_show_names_by_source_with_manual_marker_and_descriptions_below`：原来查「一行写完、窄了截断」，现在样子改成分节、描述另起一行，改查 90 宽一行写完、40 宽折两行加 `…`。
  - `mcp_status_and_approval_show_server_tool_arguments_and_escape_interrupts`：要找的字换成新写法；后半段要按 Esc 打断回合，弹框开着时 Esc 归弹框，所以先按一次 Esc 关掉弹框再往下走。
- **拿主意的地方**（样子没写到的，都照已有写法定了，主控可改）：
  - Git status 节标题只写分支名：git 给的 `main...origin/main [ahead 1]` 里的上游和 ahead/behind 不再显示（原来 `Git: …` 那行有）。
  - 对齐的栏（git 的词、Changed by canter 的词、MCP 服务名、技能名后的 `not for model`）都是最宽的加 3 格；`staged` 那栏只按带标记的行对齐，免得一条很长的 untracked 路径把它推出屏。样子里的空格数是手画的，没照抄。窄窗口不另排，每行照现有规则截断加 `…`。
  - 两位代码：暂存区有改动时词取暂存区那边（`AM` 是 added partly staged，`MD` 是 modified partly staged）；`T` 算 modified；表外的字母原样显示。`--no-renames` 下 renamed 实际出不来，映射留着。
  - git 出错（读不了、跑不了、超时）和不在仓库一样写在括号里；干净的仓库写 `(none)`；这几种第一行写 `git 0`。
  - 第一行的数量摘要、节标题后的分支和目录、`(none)` 用灰色；Changed by canter 的词和 git 的词同样上色。
  - `/mcp`：`disconnected` 和 `○` 一样红；出错原因用正常字色；单数写 `1 service`、`1 tool`；连上了但有 error 的也写原因；没配服务时第一行 `MCP  0 services · 0 tools`，下面 `No MCP services configured.`，不写最后那句说明。
  - `/skills`：没有说明行，所以最后是空行加 `Esc close`；`not for model` 放在名字栏后面，不贴右边；服务端以后给的未知来源放第三节 `Other`（不写目录），只算进总数；没技能时第一行 `Skills  0 · global 0 · project 0`，下面原来那四行。
- **没做的事**：三个命令矮窗口滚动、Enter 和 Ctrl+C 关、草稿保留没有单独写测试（和 `/context` 走同一段代码，`context_popup.rs` 测着；任务清单里也没要）。服务端、协议、`/context` 内容和别的命令没动。
- **踩过的坑**：第一次跑全套时 `canter::tui statusline::startup_picker_exits_on_input_end_without_sighup` 失败一次（起真终端进程、带超时的测试），单独跑和再跑全套都过，和这次改动无关。`slash.rs` 的 `Screen::key` 按完键不重画，查关没关要先 `draw()`。
