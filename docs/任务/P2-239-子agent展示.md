# 任务：第二期 - 子 agent 展示改成树形

2026-11-06，canter/main 交给 canter/dev-agents-view（Claude Code，`opus[1m]` / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.97）、不要、拿不准；主控定改行为：只改终端绘制和 `/agents` 列表的行，不碰服务端和协议）。界面活按「界面和设计」读表，`middle-design`。
类型：样式／文案调整
依据：`/agents` 面板标题数字重复、主会话一行又长又粗、主会话和子 agent 同一层看不出从属；状态行下面「▸ N sub-agents running」看不出能点、颜色分不清状态。照用户选的 A 改成树形，状态行那一行一起改。只改 `crates/canter-tui`。
提示：沿用现有视觉和用语约定，聚焦指定的呈现结果。
依赖：P2-238
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-06 用户（附两张截图：子 agent 屏幕上状态行下的「▸ 1 sub-agent running」，和打开的 `/agents` 列表）：「还有agents的展示。我也觉得不好看。」
- 主控在 Chrome 里给了示意：现在的样子；A 树形（主会话在上暗色截短，子 agent 挂在下面四列对齐，状态上色，当前所在标 `(here)`，标题「Sub-agents · 1 running · 2 done」）；B 两段（「← Back to main session」单独一段，子 agent 列表格）；状态行那一行改成「● 1 sub-agent running · 2 done · click or /agents」。
- 用户：「选A，状态行那一行也一起改」

## 现状（主控查过的）

- `crates/canter-tui/src/agents.rs`：`Agents::row`（状态行下面那一行，`▸ 2 sub-agents running · 1 done` / `▸ 3 sub-agents`）、`Agents::title`（`Sub-agents · 3 · 2 running`）、`Agents::targets`（列表每行打开什么：子 agent 屏幕上先是主会话 `None`，再是各子 agent；主会话屏幕上只有子 agent）、`Agents::rows`（标题、每个目标一行、按键行；主会话一行是 `← Main session · <主会话名字>`，名字常常就是第一句提问）、`mark`、`status_label`、`type_label`、`header`（子 agent 屏幕顶上两行，本件不改）。
- `crates/canter-tui/src/app.rs`：`agent_choices` 和 `DrawnAgents` 记每一行对应哪个目标，点击状态行那一行打开列表、点击列表某行打开那个会话；P2-238 之后列表画在顶替输入框的青色圆角框里，标题在上边框，按键在下边框。
- `/agents` 在没有子 agent 时显示 `No sub-agents in this session.`（`agents::empty`），本件不改。

## 先读

- `crates/canter-tui/src/agents.rs` 全文，`crates/canter-tui/src/app.rs` 里 `agents_key`、`agent_choices`、`DrawnAgents` 和画状态行下面几行（`plan`、`agent_row`）的地方。
- 相关测试：`crates/canter-tui/src/run_agents_tests.rs`，以及 `crates/canter/tests/` 里碰到子 agent 列表和那一行的测试（用 `grep -rn "sub-agent" crates/canter/tests crates/canter-tui` 找）。
- `docs/DESIGN.md` 第 19 条展示层、P2-179 子 agent 界面那段。
- 项目规矩 `CLAUDE.md`。

## 在哪里干活

- worktree：`../canter-worktrees/agents-view`，分支 `agents-view`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/agents-view`。
- 只动 `crates/canter-tui/`、`crates/canter/tests/` 里断言这几处样子的测试，和 `docs/DESIGN.md` 里讲子 agent 列表和那一行的地方。

## 要做的

用户选的样子（A），在子 agent 屏幕上大致这样（框是 P2-238 的）：

```
╭─ Sub-agents · 1 running · 2 done ─────────────────────────────────────────╮
│ › Main session · 用 reviewer 子 agent 审查 src/ 里解析日期和时长的代码…      │
│   ├ ● Review date and duration parsing (here)  reviewer  running  2m 05s  │
│   ├ ✓ Find date helpers                        explorer  done     41s     │
│   └ ✗ Check duration overflow                  general   failed   12s     │
╰──────────────────────────────────────── ↑↓ select · Enter open · s stop · Esc close ─╯
```

状态行下面那一行：

```
⠋ Ambling… · 11s · ↑ 6.8k · ↓ 254 tokens · Esc to interrupt
  ● 1 sub-agent running · 2 done · click or /agents
```

1. **列表标题**：`Sub-agents` 后面只写状态数：有跑着的写 `· N running`，有结束的写 `· M done`（两样都有就都写，`Sub-agents · 1 running · 2 done`）；没有总数那个重复的数。
2. **主会话一行**：
   - 放在最上面，主会话屏幕上和子 agent 屏幕上都有。写 `Main session · <名字>`，暗色、不加粗（选中时照现在的选中样式：`›` 加粗）；名字放不下就截短加 `…`。
   - 子 agent 屏幕上：Enter 或点击回到主会话，照现在。
   - 主会话屏幕上：这一行后面标 `(here)`，Enter 或点击就是关掉列表（已经在主会话里）；打开列表时默认选中第一个子 agent，不选中这一行。
3. **子 agent 行**：
   - 挂在主会话下面，前面用树形线：不是最后一个用 `├`，最后一个用 `└`，线是暗色。
   - 四列对齐：状态符号（照现在的 `mark`：`●` `✓` `✗` `■` `○`）加名字、类型、状态词、用时。类型暗色；状态词和用时按状态上色：跑着用强调色，完成用成功色，失败用失败色，其它（打断、关掉、未知）暗色。
   - 状态词：`running`、`done`（原来的 `completed`）、`failed`，其它照原来的词（`stopped by you`、`interrupted`、`closed`、`unknown`）。只改列表里的这个词，聊天里子 agent 卡片用的词不动。
   - 当前所在的那个子 agent（在它的屏幕上打开列表时）名字后面标暗色 `(here)`。
   - 窗口窄时：先截短名字，再截别的；照现在不换行。
4. **状态行下面那一行**：
   - 有跑着的：`● N sub-agent(s) running`，`●` 用强调色，后面有结束的加 ` · M done`，最后暗色 ` · click or /agents`。
   - 都没在跑：`○ N sub-agent(s)`，`○` 暗色，后面同样加 ` · click or /agents`。
   - 不再用 `▸`；点击打开列表照现在能用；列表打开时这一行照现在不显示。
5. 其它不变：按键（↑↓、Enter、`s` 停、Esc）、点击、子 agent 屏幕顶上两行、没有子 agent 时的提示、聊天里的子 agent 卡片、`/bg` 后台命令那一行（`▸ N background commands`）都不动。
6. **DESIGN** 里讲 `/agents` 列表和那一行的地方，改成新样子。

## 怎么算做完

- 用户要的：`/agents` 列表改成树形（主会话在上暗色截短、子 agent 挂在下面四列对齐、状态上色、当前所在标 `(here)`、标题只写 running 和 done 的数），状态行下面那一行改成带颜色的符号加 `click or /agents`。
- 验证只做这些：
  - 针对这件的测试，先写、确认现在失败：子 agent 屏幕和主会话屏幕各打开一次列表，查标题、主会话一行、`├`/`└`、四列对齐、`(here)`、状态词；主会话屏幕上默认选中第一个子 agent、Enter 选主会话一行时关掉列表；状态行下面那一行跑着和都没跑两种写法。
  - 现有断言旧样子的测试改成新样子，只改断言的样子，不删测试、不放宽要查的内容（包括 `crates/canter/tests/` 里的）。
  - `cargo nextest run -p canter-tui` 和 `cargo nextest run -p canter`（两个包都有界面测试），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改服务端、协议和别的 crate 的代码；不改子 agent 的行为、按键和点击。
- 不改上面第 5 条列的那些。
- 界面文字一律英文；新加的字只有 `Main session`（原来就有）、`(here)`、`done`、`click or /agents`。
- 第二期核心不绑 DeepSeek；DeepSeek 这些别家的测试一条都不许改，碰到了停下来问。
- 不碰用户真实的 `~/.canter`；要起服务端看效果用隔离的 HOME、`CANTER_HOME` 和临时端口，不调真实模型。不录屏。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（上面没写到的取舍，比如窄窗口时列怎么让、主会话屏幕上那一行和现有点击逻辑冲突），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `agents-view` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
