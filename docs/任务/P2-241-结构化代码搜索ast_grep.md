# 任务：第三期 - 结构化代码搜索工具 ast_grep

2026-11-07，canter/main 交给 canter/dev-astgrep（canter，`canter --yolo`，模型用服务端配置的 `opencode-go/deepseek-v4.1-flash`，用户点名）。不算 Codex / Kimi 对比样本。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.43）、要（安全和隐私 0.96）、碰要害）。交叉审查读 `review-codex`。
类型：功能变更
依据：新加一个只读工具 `ast_grep`，按语法结构搜代码（照 omp 的 `ast_grep`）；本轮只做搜索，不做按结构改代码（`ast_edit`）。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-07 用户：「再看一下canter比较pi，omp等还差什么功能？挑一个中等难度的。我要使用canter搭配deepseek flash来做一下」
- 主控对比后挑了 omp 的结构化代码搜索 `ast_grep`（按语法结构搜，不按文字搜；比如找所有 `.unwrap()` 调用、找带某个参数的函数，不被注释、字符串、换行干扰）。用户：「好，写ast_grep任务文件给我看」
- 主控列了三条请用户确认：语言先做 Rust、Python、TS/JS；验收照主控列的那条；第一次在 canter 仓库开 canter 可能要用户点信任框。用户：「都照你的建议，派吧」

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
2. **语言**：Rust、Python、TypeScript（含 `.tsx`）、JavaScript（含 `.jsx`、`.mjs`、`.cjs`）。
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

- 用户确认的验收（「都照你的建议」）：给一个模式，能在 Rust、Python、TS/JS 的样例代码里按结构找到该找的、不误报注释和字符串里的同样文字；翻页、`.gitignore`、模式写错、私有文件拒绝、计划模式能用，这几样行为照「要做的」。
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

## 完成记录

### 做了什么

- 新只读工具 `ast_grep`：`crates/canter-tools/src/ast_grep.rs` 实现参数解析和模式编译、目录遍历（复用已有的 `ignore`，照 `.gitignore`，不跟随符号链接）、逐文件过执行器和私有文件检查及 `rules.json`、解析匹配、分页、跳过与语法错误说明；`lib.rs` 接入 `Tool::AstGrep`（插在 `read` 后）、`Action::AstGrep`、工具定义、`prepare`/`execute`、`Tools.file_rules`；`permission.rs` 走 `read` 同一套 `evaluate` 和文件规则，计划模式 `evaluate_plan`、审查轮 `evaluate_review` 放行。
- 依赖：`ast-grep-core = 0.40.0`、`tree-sitter-rust =0.24.2`、`tree-sitter-python =0.25.0`、`tree-sitter-javascript =0.25.0`、`tree-sitter-typescript =0.23.2`；`cargo tree -i tree-sitter` 确认仍只有 `tree-sitter v0.25.10` 一份。
- 协议：新增 `AstGrepMatch` 和 `Item::AstGrep`（序列化为 `astGrep`），重新生成 `generated/`，加兼容性测试（旧客户端按 Unknown 跳过）；终端卡片、导出、`canter exec`、统计、系统提示词 `SEARCH_BULLET`、`roles.rs` 的 `explorer_tool`，以及 `session.rs` / `configuration.rs` 传 `command_rules_path` 都接上。
- 文档：`docs/usage.md` 工具表加一行并补只读和文件规则说明；`docs/DESIGN.md` 加「结构化搜索（P2-241）」条目。

### 验证了什么

- 针对性测试 `crates/canter-tools/tests/ast_grep.rs` 11 个全过：五种语言按结构找 `.unwrap()`、注释和字符串里的同样文字不误报、`.gitignore`、二进制和未知扩展名、50 条分页与 `offset` 越界、模式错误、某语言模式解析不了、语法错误文件计数、`rules.json` 拒绝和要问的文件加私有文件加指向私有文件的符号链接均不出现也不读、`rules.json` 不可读时直接失败、计划模式与审查轮、取消。
- `cargo nextest run -p canter-tools`：218 tests，217 passed，1 failed；失败的是 `guardian concurrent_commands_do_not_inherit_each_others_pipes`（`Os { code: 2, NotFound }`），在 main 上同样失败，属本机环境问题。
- `canter-protocol` 55 passed；`canter-core` 256 passed；`canter-server` 417 passed；`canter` 228 passed；`canter-tui` 704 tests，703 passed，1 failed：`statusline::tests::config_command_gets_only_public_json_and_terminal_environment`（本机 `TMPDIR=/private/tmp/...` 里带 `private`），main 同样失败。新增的 `tool_cards` 20 个（含 ast_grep 卡片）单独跑过。
- `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过；`cargo fmt` 重排过新文件，之后又重跑了上面两个针对性测试。
- 因工具列表变化更新的逐字节期望：`crates/canter-core/tests/data/core_deepseek.txt`（提示词快照）、`crates/canter-core/tests/fixtures/p2-223-main-requests.jsonl`、`crates/canter-core/tests/sandbox-disabled-request.json`、`crates/canter/tests/fixtures/no_mcp_request.json`（请求体里多一条 `ast_grep` 工具定义）；工具名列表加 `ast_grep` 的有 `tests/web_fetch.rs`、`tests/web_search.rs`、`crates/canter/tests/support/mcp.rs`、`crates/canter/tests/tui.rs`。

### 拿主意的地方

- 依赖版本：`ast-grep-core` 0.41 及以后要 tree-sitter 0.26/0.27，0.51 还要 rust 1.90（workspace 是 1.89），所以停在 0.40.0（依赖 `tree-sitter ^0.25.4`，配得上 `=0.25.10`）。
- 语言映射：`.ts`/`.mts`/`.cts` 按 TypeScript、`.tsx` 按 TSX 语法、`.js`/`.jsx`/`.mjs`/`.cjs` 按 JavaScript；`lang=typescript` 同时选 `.ts` 和 `.tsx`，`lang=tsx` 只选 `.tsx`。
- 没给 `lang` 时把模式对五种语法各编译一次，有一种能编译就不算整体错误；编译不了的语言的文件只计数并在末尾说明，不把整次搜索判错。
- 没给 `path` 时在 `prepare` 里把会话目录写进 `arguments["path"]`，让路径规则和批准参数与显式给 `path` 时一致（`write`/`edit` 也是这样做的）。
- 目录里被规则挡住或读不了的文件只报个数（`Skipped N files that file rules block, require approval for, or that could not be read.`），不列路径；语法树有错的文件也只报个数。二进制、非 UTF-8、未知扩展名静默跳过，不计入跳过数（和 `rg` 一样）。
- `offset` 按条跨文件累计；结果按路径、行、列排序，每条只留前三行（多行再写 `… (N more lines)`）。

### 没做的事

- 不做 `ast_edit`，不加列表外的语言，不改 `rg`、`read` 的路径规则和别的工具的行为。
- 没加 `ast_grep` 专用的配置项、批准或沙箱行为，权限完全复用 `read`。

### 踩过的坑

- `tree-sitter` 配版比预计窄：`ast-grep-core` 0.41+ 要 0.26、0.50 要 0.27、0.51 要 rust 1.90，只有 0.40.0 配得上现有的 `=0.25.10`；语法包也得挑依赖 0.25 的那几个版本。
- 同一个模式不是每种语言都能编译（例如 `$X.unwrap(` 在有的语法里仍是合法节点），所以模式错误必须按语言分别判断和说明，不能一个结论了事。
- `ast_grep` 插在 `read` 后面，`crates/canter/tests/support/mcp.rs` 里按位置取工具的地方从 `tools[18]` 变 `tools[19]`，20 个工具的注释也要同步改。
- 逐字节 fixture 不能靠手改：按测试提示临时改测试写盘、再恢复测试代码才得到正确内容。
- 中途误跑过一次 `python3` 的临时命令（只读、无副作用），发现后一律改用 Rust 测试和 shell 完成。
- 两个预存在失败（`canter-tools` 的 guardian、`canter-tui` 的 statusline）与本件无关，不要当成本次回归。

### 参考了 omp 哪里，有没有搬代码

看 `/tmp/oh-my-pi/crates/pi-ast/src/language/mod.rs`（语言适配和 expando 字符）、`search/mod.rs`（遍历、分页、错误报告）、`ops.rs`（模式编译与匹配），以及 `packages/coding-agent/src/prompts/tools/ast-grep.md` 的说明写法。没有搬代码：语言枚举、遍历、渲染、权限和协议都照 canter 现有结构自己写。
