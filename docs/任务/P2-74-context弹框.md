# 任务：/context 的结果改成弹框

2026-10-27，canter/main 交给 canter/dev-contextpopup（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 1.0）、不要、改行为）
类型：功能变更
依据：`/context` 的结果现在画进对话区，和模型的回答混在一起；改成在输入框上方弹框，看完关掉，对话区不留。只改终端界面里的 `/context`。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27 用户：「context命令现在的输出是直接到agent的输出中了？」主控答：只画在终端对话区，不发给模型；可以改成弹框或换样式。用户：「弹框」。
- 10-27 主控给了下面的样子，问哪些命令改成弹框，用户选「只改 /context」。

## 样子（用户看过的）

```
 Context  ctx 12% 120k/1M (last request)

   System prompt              2.3k
   Global AGENTS.md            1.1k  ~/.agents/AGENTS.md
   Project AGENTS.md           3.4k  ~/Developer/.../canter/AGENTS.md
   Environment                   80
   Tools (6)                   2.9k
   Messages (42)              110k  user 8k · assistant 31k · …
   Free                       880k

 Estimated by size; the first line is what the model reported.
 Currently loaded context; queued input and future tool results are not included.
 Esc close
╭──────────────────────────────────╮
│ >                                │
╰──────────────────────────────────╯
 status line 1
 status line 2
```

## 先读

- `docs/DESIGN.md` 第 8 条「上下文查询（P2-8）」那段末尾的 P2-74 一句。
- `crates/canter-tui/src/context.rs`（每行写什么）；`crates/canter-tui/src/app.rs` 的 `show_context`、`show_report`。
- 现有的输入框上方弹框：`/statusline`（`crates/canter-tui/src/statusline/setup.rs` 和 app.rs 里怎么画、怎么收按键、窗口矮时怎么滚），照它的样子和按键习惯做。
- `crates/canter-tui/README.md` 里讲 `/context` 的地方。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-74-contextpopup`，分支 `p2-74-contextpopup`（派的时候从 main 建好）。
- 只动 `crates/canter-tui/`（代码、测试、`tests/expected/` 下的期望画面、README）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-74-contextpopup`。

## 要做的

- 输入 `/context` 后，结果不再写进对话区，改成在输入框上方弹框，照上面的样子：每行的写法和现在 `context.rs` 出的一样，最后加一行 `Esc close`。
- 数字是打开那一刻查到的，弹框开着时不跟着变。
- Esc 或 Enter 关掉；关掉后对话区和打开前一样，不留任何一行。
- 弹框开着时按键归弹框（和 `/statusline` 一样），输入框里的草稿留着，关掉后接着打。
- 窗口放不下时照 `/statusline` 的办法：先让对话区让出地方，还放不下就让中间那些段落行跟着 ↑/↓ 滚，上下写 `↑ N more` / `↓ N more`，第一行 `Context …` 和最后的说明行、`Esc close` 一直留着。
- 查询失败时（比如会话还没打开）照现在的提示方式，不弹空框。
- README 里 `/context` 的说明跟着改。

## 怎么算做完

- （用户 10-27）`/context` 的结果改成弹框，样子照上面；只改 `/context`。
- 验证只做这些：
  - 针对上面写测试，先写、确认在现在的代码上失败：打开 `/context` 弹框的画面；Esc 和 Enter 关掉后对话区里没有 `/context` 的行；输入框草稿在关掉后还在；矮窗口时中间几行跟着 ↑/↓ 滚、`↑/↓ N more` 对。
  - 现有画面测试和期望画面不应变；有变的（比如原来测 `/context` 画进对话区的）逐屏看过，在完成记录里说明为什么。
  - 做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的编译目录）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `/files`、`/mcp` 和别的命令的显示；不改服务端、协议、核心；不改 `/context` 每行的内容和算法。
- 不加依赖，不升级已有依赖。
- 不读用户真实的 `~/.canter` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，不碰用户正在跑的服务端（别读、别停它的 pid）。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括按键和现有弹框、输入框的用法打架，样子里没写到的显示细节），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-74-contextpopup` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
