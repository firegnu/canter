# 任务：第三期 - 结构化代码搜索工具 ast_grep

2026-11-07，canter/main 交给 canter/dev-astgrep（canter，`canter --yolo`，模型用服务端配置的 `opencode-go/deepseek-v4.1-flash`，用户点名）。不算 Codex / Kimi 对比样本。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.43）、要（安全和隐私 0.96）、碰要害）。交叉审查读 `review-codex`。
类型：功能变更
依据：新加一个只读工具 `ast_grep`，按语法结构搜代码（照 omp 的 `ast_grep`）；本轮只做搜索，不做按结构改代码（`ast_edit`）。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
待用户：确认「要做的」里标了（待用户定）的几处
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-07 用户：「再看一下canter比较pi，omp等还差什么功能？挑一个中等难度的。我要使用canter搭配deepseek flash来做一下」
- 主控对比后挑了 omp 的结构化代码搜索 `ast_grep`（按语法结构搜，不按文字搜；比如找所有 `.unwrap()` 调用、找带某个参数的函数，不被注释、字符串、换行干扰）。用户：「好，写ast_grep任务文件给我看」

## 现状（主控查过的）

- canter 没有专门的搜索工具，模型搜代码靠 `shell` 里的 `rg`（系统提示词 `crates/canter-core/src/prompt.rs` 的 `SEARCH_BULLET`）。
- 工具都在 `crates/canter-tools/src/lib.rs`：`Tool` 枚举（`ALL`、`name`、`risk`、`definition`）、`Tools::find`（哪些工具这次可用）、执行分发（`Tool::Read =>` 那几处）、工具结果转协议 `Item`（`Tool::Read => Item::FileRead`）。
- 权限：`crates/canter-tools/src/permission.rs`。`read` 走 `file_rules`（`rules.json` 的文件规则、canter 私有文件拒绝），计划模式 `evaluate_plan`、只读审查轮 `evaluate_review` 都放行 `read`。
- 子 agent 的只读角色用 `crates/canter-core/src/roles.rs` 的 `explorer_tool` 列只读工具。
- 协议 `Item` 在 `crates/canter-protocol/src/types.rs`，每种工具一个变体；改了要重新生成 `generated/`（命令见 `crates/canter-protocol/README.md`「在 workspace 根目录运行」那段）。
- 终端卡片在 `crates/canter-tui/src/cells.rs`（照 `web_search` 那张：标题 `Search <query>`，右边 `N results`），导出在 `export.rs`，统计在 `crates/canter-core/src/stats/tools.rs`。
- 新加一个工具的样子可以看 P2-106 `web_search` 的合并提交 `7cb5b9a`（动了哪些文件）。
- canter 已经依赖 `tree-sitter = "=0.25.10"` 和 `tree-sitter-bash`（`crates/canter-tools/Cargo.toml`，危险命令判断用）。
- 参考：omp 的 Rust 实现在 `/tmp/oh-my-pi/crates/pi-ast/`（用 `ast-grep-core`，MIT），工具说明在 `/tmp/oh-my-pi/packages/coding-agent/src/prompts/tools/ast-grep.md`，参数在 `packages/coding-agent/src/tools/ast-grep.ts`。只读参考；真搬代码照 `CLAUDE.md`「从别的项目搬来的代码」那条办。

## 先读

- 项目规矩 `CLAUDE.md`。
- `docs/DESIGN.md` 里工具、权限、计划模式、缓存（第 17 条「照顾缓存命中」）的条目。
- 上面「现状」列的文件和 `7cb5b9a` 的改动列表。

## 在哪里干活

- worktree：`../canter-worktrees/astgrep`，分支 `astgrep`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/astgrep`。

## 要做的

新工具 `ast_grep`，只读，所有模型、所有会话默认都有：

1. **参数**：`pattern`（必填，ast-grep 的模式，比如 `$X.unwrap()`）；`path`（可选，文件或目录，默认会话目录）；`lang`（可选，不给就按扩展名认）；`offset`（可选，翻页跳过前面多少条）。
2. **语言**：Rust、Python、TypeScript（含 `.tsx`）、JavaScript（含 `.jsx`、`.mjs`、`.cjs`）。（待用户定）
3. **搜哪些文件**：目录照 `.gitignore` 跳过（和 `rg` 一样），跳过二进制文件和认不出语言的文件；给了 `lang` 只搜这种语言。
4. **结果**：按路径、行号排序；每条给 `路径:行:列` 和匹配到的代码（多行的只给前几行）。一次最多 50 条，超了说一共多少条、下一页用 `offset` 多少。结果另有总长度上限，照现有工具输出的限制办法。
5. **出错**：模式写错（解析不了）直接报错，说清哪里不对，让模型改模式；有文件解析出错的，在结果末尾说几个文件没搜成，不当成「没找到」。按 Esc 能停下。
6. **权限**：算只读（`Risk::Read`），路径规则和 `read` 完全一样（`rules.json` 文件规则、canter 私有文件拒绝）。给的是目录时，目录下每个文件都要过同样的规则，规则拒绝或要问的文件不读、不出现在结果里，只在末尾说跳过了几个；符号链接指到外面的照 `read` 的规则办。计划模式、只读审查轮、子 agent 只读角色（`explorer_tool`）都能用。
7. **界面**：终端卡片照 `web_search` 那张的样子，标题 `AST search <pattern>`，右边 `N matches`（1 条写 `1 match`），展开列出匹配；出错照现有卡片显示错误。界面文字都是英文。导出、统计里有这个工具。
8. **系统提示词**：`SEARCH_BULLET` 那里加一句英文，说按语法结构找代码时用 `ast_grep`，按文字找照旧用 `rg`。工具说明写短，它每次请求都会发。
9. **协议**：`Item` 加一个新变体，重新生成 `generated/`；按协议 README 的规矩写兼容性测试和说明。
10. **文档**：`docs/DESIGN.md` 记这个工具和取舍，`docs/usage.md` 工具一节加说明。
11. **依赖**：用成熟、活跃维护的 `ast-grep-core` 和各语言的 tree-sitter 语法包；只能有一份 `tree-sitter`，和现在的 `=0.25.10` 配得上。配不上的，停下来问主控，不要升级或降级已有的 `tree-sitter`。

## 怎么算做完

- 用户原话没有给验收，主控列的待用户确认：给一个模式，能在 Rust、Python、TS/JS 的样例代码里按结构找到该找的、不误报注释和字符串里的同样文字；翻页、`.gitignore`、模式写错、私有文件拒绝、计划模式能用，这几样行为照「要做的」。
- 验证只做这些：
  - 给这个工具写针对性的测试（用测试里临时建的小样例，不读用户真实目录）。
  - 碰要害这档另加边角用例：目录里混着 canter 私有文件和 `rules.json` 拒绝的文件、符号链接指到私有文件，结果里都不出现、内容不被读。
  - 改过的每个 crate 跑它的全部测试：`cargo nextest run -p <crate>`。工具定义会改变发给模型的请求内容，所以 `canter-server` 和 `canter` 两个 crate 一定要跑（逐字节比对的请求测试在这两个里）；逐字节比对的期望值因为新工具变了的，照实更新，在完成记录里说更新了哪些、为什么。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不做按结构改代码（`ast_edit`），不加列表外的语言。
- 不改 `rg` 的用法和别的工具的行为，不改 `read` 的路径规则。
- 功能核心不绑定某家模型：不按 DeepSeek 或别家的名字、`base_url`、请求字段、缓存行为分支；所有模型一样有这个工具。
- 不用 Python 写任何代码、测试或辅助脚本。
- 不调真实的模型接口；不碰用户的 `~/.canter`、真实会话和配置，测试用隔离的 HOME 和临时目录。
- 不改别的仓库；`/tmp/oh-my-pi` 只读。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括依赖版本配不上、某个行为本文件没写清），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `astgrep` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。另写参考了 omp 哪些地方、有没有搬代码。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
