# 任务：第二期 - 子 agent 和后台命令挪到底部

2026-11-06，canter/main 交给 canter/dev-footer（Claude Code，`opus[1m]` / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.97）、不要、改行为；样子用户已在示意里选定，不算出方案，按中档）。界面活按「界面和设计」读表，`middle-design`。
类型：功能变更
依据：子 agent 那一行（P2-239）和后台命令那一行现在夹在「模型在干活」那一行和输入框之间，打开的列表也在输入框上面，用户觉得输入框上面不优雅。照用户选的 A：两行都挪到输入框下面、两行状态行的下面，按 ↓ 选中、Enter 展开，展开的列表在底部往下长，输入框留在原处变灰。只改 `crates/canter-tui`。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-238、P2-239
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-06 用户（看了 P2-239 的新样子）：「这个ui我还想调整一下，我觉得整个subagents的ui以及交互，是否可以搬到statusline的下面。理解我意思吗？现在的prompt上面还是感觉不优雅」
- 主控在 Chrome 里给了示意：A 提示和列表都挪到底部（平时一行在状态行下面；输入框空着时 ↓ 选中、反色，Enter 或 ↓ 展开；展开的列表在底部往下长，输入框留着变灰，Esc 收起）；B 只挪提示那一行，列表照 P2-238 顶替输入框。问后台命令那一行要不要一起挪。
- 用户：「选A，后台命令那一行也一起挪」

## 现状（主控查过的）

- 绘制在 `crates/canter-tui/src/app.rs`：`View::render` 从上到下是对话、提示行、`command`、状态行（「Waltzing…」那行）、`hung`（计划步骤和 Tip）、`plan`（后台命令那一行 `▸ N background commands`、子 agent 那一行，子 agent 在最后）、`list`、`Bottom`（输入框、批准框或 P2-238 的面板框）、两行信息行（`INFO_ROWS`，模型、档位、权限、沙箱、花费；目录、分支、上下文等，可由 `statusLine` 脚本换掉）。
- 子 agent：`agents.rs` 的 `Agents::row`、`Agents::rows`（P2-239 树形）、`agents_key`、`agent_choices`、`DrawnAgents`；点那一行打开 `/agents` 列表，点列表某行打开那个会话。
- 后台命令：那一行在 `app.rs` 里拼（`▸`/`▾ N background commands`）；点它打开 `/ps` 列表（一个 `chooser`，`background_open`、`background_choices`、`DrawnBackground`），选一条打开它的输出（`background.rs` 的报告，`s stop · r refresh · Backspace list · Esc close`）。
- P2-238 之后，`/agents`、`/ps` 和后台命令输出都和别的面板一样，用青色框顶替输入框。
- 输入框里 ↑/↓ 现在用于多行光标移动和输入历史（`input_history`），补全列表开着时用于选择。

## 先读

- `crates/canter-tui/src/app.rs` 的 `View::render`、`Bottom`、画 `plan` 行的那段、`agents_key`、`background_click`、`agent_click`（或同名的点击处理）、输入框的按键处理（找 `KeyCode::Down` 和 `input_history`）。
- `crates/canter-tui/src/agents.rs`、`crates/canter-tui/src/background.rs`。
- 相关测试：`run_agents_tests.rs`、`popup_tests.rs`、`crates/canter-tui/tests/` 和 `crates/canter/tests/` 里碰到这两行、`/agents`、`/ps` 的（`grep -rn "sub-agent\|background commands\|/ps" crates/canter-tui crates/canter/tests`）。
- `docs/DESIGN.md` 第 19 条展示层、P2-179 子 agent 界面、P2-238 面板加框、P2-239。
- 项目规矩 `CLAUDE.md`。

## 在哪里干活

- worktree：`../canter-worktrees/footer`，分支 `footer`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/footer`。
- 只动 `crates/canter-tui/`、两个包里断言这些样子和按键的测试，和 `docs/DESIGN.md` 里讲这几处的地方。

## 要做的

用户选的样子（A），平时：

```
⠋ Waltzing… · 16s · ↑ 27.1k · ↓ 388 tokens · Esc to interrupt
  ⎿ Tip: Use /theme to choose the colours of code.
╭──────────────────────────────────────────────────────────────╮
│ › Type a message (Enter to send, Ctrl+J for a new line)      │
╰──────────────────────────────────────────────────────────────╯
 opencode-go/deepseek-v4.1-flash · think high · Allow all · Sandbox off · <$0.01
 ~/Developer/personal_projs/canter-test-rust (main) · (+0,-0) · 7k/400k …
 ● 1 sub-agent running · 2 done · ↓ to view
```

按 ↓ 选中（这一行反色），展开后：

```
╭──────────────────────────────────────────────────────────────╮
│ › Type a message (Enter to send, Ctrl+J for a new line)      │   ← 灰色，不收按键
╰──────────────────────────────────────────────────────────────╯
 opencode-go/deepseek-v4.1-flash · think high · …
 ~/Developer/personal_projs/canter-test-rust (main) · …
╭─ Sub-agents · 1 running · 2 done ────────────────────────────╮
│ › Main session · 用 reviewer 子 agent 审查 src/ (here)          │
│   └ ● Review date duration parsing  reviewer  running  1m 00s │
╰──────────────────────── ↑↓ select · Enter open · s stop · Esc close ─╯
```

1. **两行挪到底部**：子 agent 那一行和后台命令那一行，从输入框上面挪到两行信息行下面（后台命令在前、子 agent 在后，照现在的先后）。内容照现在（P2-239 的子 agent 那一行；后台命令那一行照现在的字和符号），只把结尾的打开提示换成 ` · ↓ to view`（子 agent 原来的 ` · click or /agents` 也换成这个），提示暗色。输入框上面不再有这两行。
2. **按 ↓ 选中**：
   - 只在 ↓ 原来什么都不做的时候生效：输入框里光标在最后一行、没有在翻输入历史（或已经翻回到最新）、没有补全列表、没有别的面板或批准框开着。原来 ↓ 有用的情况一律照旧。
   - ↓ 选中底部第一行（反色显示，后面暗色 `Enter to open · ↑ back`）；有两行时再按 ↓ 到下一行，在第一行按 ↑ 回到输入框。选中时打字照常进输入框并取消选中；Esc 取消选中。
   - 选中时按 Enter 或 ↓（在最后一行时）展开那一行的列表。
3. **展开的列表在底部**：
   - `/agents` 列表（P2-239 树形）、`/ps` 列表和从它打开的后台命令输出，都画在两行信息行下面，用 P2-238 一样的青色圆角框（标题在上边框、按键在下边框）；放不下时照现在滚动、截断。
   - 展开时输入框留在原处，边框和文字变灰，不收按键（按键交给列表，照现在各列表的按键）；Esc 收起，回到输入框，草稿不丢。
   - 用 `/agents`、`/ps` 打开、点击那一行打开，都走同样的底部列表。
   - 别的面板（`/mcp`、`/model`、`/context` 等）照 P2-238 顶替输入框，不动。
4. **点击**：点底部那一行打开列表、点列表里某行打开它，照现在能用（行号随新位置算对）。
5. **窗口矮时**：先让对话少显示几行；还放不下时底部这两行和列表照现在的规则让位（放不下就不显示），不让输入框和两行信息行被挤掉。拿不准怎么让，停下来问。
6. **DESIGN** 第 19 条和 P2-179、P2-238、P2-239 讲这几处位置的地方，改成新样子，写上 ↓ 选中的规则。

## 怎么算做完

- 用户要的：子 agent 和后台命令那两行挪到两行状态行下面，输入框上面干净；↓ 选中、Enter 展开，展开的列表在底部往下长，输入框留在原处变灰，Esc 收起。
- 验证只做这些：
  - 针对这件的测试，先写、确认现在失败：两行在信息行下面、输入框上面没有它们；空输入框 ↓ 选中、↑ 回去、两行间移动、Enter 展开子 agent 列表和 `/ps` 列表、列表在信息行下面、输入框变灰、Esc 收起草稿还在；↓ 原来有用的情况（多行光标、翻历史、补全列表）照旧；点击那一行和列表行照旧打开。
  - 现有断言旧样子和旧位置的测试改成新样子，只改样子和位置，不删测试、不放宽要查的内容（两个包都查）。
  - `cargo nextest run -p canter-tui` 和 `cargo nextest run -p canter`，加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改服务端、协议和别的 crate；不改子 agent、后台命令本身的行为和列表里的按键。
- 不改别的面板（照 P2-238），不改补全列表、批准框、信息行的内容和 `statusLine` 脚本的行为。
- 界面文字一律英文；新加的字只有 `↓ to view`、`Enter to open · ↑ back`。
- 第二期核心不绑 DeepSeek；DeepSeek 这些别家的测试一条都不许改，碰到了停下来问。
- 不碰用户真实的 `~/.canter`；要起服务端看效果用隔离的 HOME、`CANTER_HOME` 和临时端口，不调真实模型。不录屏。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（上面没写到的取舍，比如 ↓ 和现有按键冲突、窗口太矮怎么让、`statusLine` 脚本换掉信息行时底部行放哪），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `footer` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- **做了什么**：`crates/canter-tui/src/app.rs` 里后台命令那一行和子 agent 那一行从输入框上面（`plan`）挪到两行信息行下面（`View.footer`），先后照旧；两行结尾都是暗色 ` · ↓ to view`（`agents.rs` 原来的 ` · click or /agents` 换掉）。输入框按 ↓：先照旧翻历史，翻不动时光标不在最后一行就照旧往下移，在最后一行且底部有行就选中第一行（`footer_selected`，反色，结尾换成暗色 ` · Enter to open · ↑ back`）。选中时 ↑/↓ 在两行间移，第一行按 ↑ 回输入框，最后一行按 ↓ 或任一行按 Enter 出 `BackgroundList`/`AgentList`（和 `/ps`、`/agents`、点击走同一条路）；Esc 只取消选中；别的键（打字、带修饰键的）取消选中后照输入框处理。`/agents` 列表（含没有子 agent 时的说明，新记了 `agents_empty`）、`/ps` 列表和命令日志开着时，`Bottom::Input` 变灰（每行改成暗色、不画光标、`!` 模式也不上色）留在原处，列表用 P2-238 的青色框画在两行下面（`footer_box`），子 agent 列表的框占子 agent 那一行；点击行号（`DrawnBackground`、`DrawnAgents`）按新位置算。别的面板照旧顶替输入框。`Bottom` 的绘制挪进 `Bottom::draw`，给输入框和底部的框共用。DESIGN 第 19 条加 P2-240 一条（含 ↓ 选中规则），P2-238、第 22 条后台命令、P2-179 列表那段改成新位置。
- **验证了什么**：先写 `src/footer_tests.rs` 7 条，改代码前全部失败（底部没有这两行）；改后通过：两行在信息行下、输入框上面没有；空输入框 ↓ 选中、两行间移动、↑ 回去、Enter/↓ 展开两种列表；打字和 Esc 取消选中；多行光标、翻历史、`/` 补全列表时 ↓ 照旧；子 agent 列表和 `/ps`（含命令日志）在信息行下面打开、输入框变灰不收按键、Esc 收起草稿还在；别的面板照旧顶替输入框。旧测试只改了位置和提示文字：`run_agents_tests.rs`（那一行改在框下第 3 行找、点击行号随之改，`click or /agents` → `↓ to view`）、`agents.rs` 单元测试的文字、`tests/user_shell.rs` 计数行在信息行下面、`popup_tests.rs` 给 `/ps`、`/agents` 加 `check_under_info`（标题、按键、暗色等检查照旧，只把「在状态行下面」换成「输入框在状态行下面、框在信息行下面」）、`crates/canter/tests/tui.rs` 的 `status()` 不再跳过子 agent 那一行，底部开着列表时取上面那个框。点击那一行和列表行的旧测试（`user_shell` 点后台行/命令/收起，`run_agents_tests` 点子 agent 行、点主会话行）没改断言、照旧通过。`cargo nextest run -p canter-tui` 704 条、`cargo nextest run -p canter` 228 条全过；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 干净。
- **拿主意的地方**：（1）两行前面的缩进照现在两格，没照示意图里对齐信息行的一格（任务说内容照现在）。（2）列表开着时后台命令那一行照旧显示 `▾`，`/ps` 的框画在两行下面（子 agent 那一行还在时在它下面）；子 agent 列表开着时它那一行由框的标题顶替，照原来。（3）窗口矮时照原来的先后：对话先让，列表（框）优先于这两行，这两行放不下就不画，输入框和信息行不让。（4）选中只在「光标/历史/补全列表都用不上 ↓」时生效，判断在原来 ↓ 的分支里做，所以批准框、面板、搜索、Ctrl+R 开着时根本走不到；选中后弹出这些框或选中的行没了，就取消选中，免得框关了以后又冒出来。（5）没有子 agent 时 `/agents` 的说明也算底部列表（任务说 `/agents` 打开都走底部）。
- **没做的事**：帮助里 `Click ▸ N background commands` 那条没改、没加 ↓ 的说明（新字只许加两处）；没起真服务端看效果。
- **踩过的坑**：用 perl 的 `s|...|...|` 改测试文件时，正则里的 `\|`（闭包参数）和分隔符冲突，把文件开头弄乱了；`git checkout` 这一个文件后改用逐处编辑。
