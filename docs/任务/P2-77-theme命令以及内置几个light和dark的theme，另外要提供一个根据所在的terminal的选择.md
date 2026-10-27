# 任务：theme命令以及内置几个light和dark的theme，另外要提供一个根据所在的terminal的选择

2026-10-27，canter/main 交给 canter/dev-theme（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.59）、拿不准、拿不准；只改终端、读写 P2-63 已有的 `tui.json`，主控定不要交叉审查、按改行为）
类型：功能变更
依据：DESIGN 第 19 条末尾「代码配色（P2-77…）」一句。照 Codex 加 `/theme` 选择框，换代码块和 diff 的语法配色。只改终端界面。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27 用户在看板上建了这一件，标题就是本文件标题。
- 10-27：「接下来我想做77.但是你要设计一下，这个我想模拟codex的做法。内置多少theme，你需要考虑一下」。
- 10-27 主控问了四处，用户都选推荐：只换代码和 diff 的配色（照 Codex，界面颜色照旧跟终端）；内置全部 32 个，和 Codex 一样；`terminal` 和 `auto` 两个都给，默认 `terminal`（就是现在的 `ansi`）；支持 `~/.canter/themes/` 里放 `.tmTheme` 文件。

## 先读

- `docs/DESIGN.md` 第 19 条末尾「代码配色（P2-77…）」一句；同一条开头讲颜色的几句（界面跟终端 16 色、diff 底色、`CANTER_THEME`）。
- Codex（只读克隆在 `/tmp/r49-src/codex`，`rust-v0.163.0`；不在了就照 `HANDOFF.md`「机器和环境」重新克隆）：`codex-rs/tui/src/theme_picker.rs`（选择框、预览、宽窄两种排法、Esc 还原）、`codex-rs/tui/src/render/highlight.rs`（内置配色的名字表、`adaptive_default_theme_selection`、自定义 `.tmTheme`、和内置重名去重、读不了的提示、换算 256 色）。只参考，不搬代码；完成记录里写参考了哪里。
- `crates/canter-tui/src/highlight.rs`（现在固定用 two-face 的 `ansi`，有缓存）、`theme.rs`（`Depth`、`Background`、`CANTER_THEME`）。
- `crates/canter-tui/src/statusline/setup.rs` 和 `app.rs` 里读写 `tui.json` 的地方（P2-63）；`commands.rs` 的 `COMMANDS`、`Chooser`。
- `crates/canter-tui/README.md` 讲颜色、代码块的地方。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-77-theme`，分支 `p2-77-theme`（派的时候从 main 建好）。
- 只动 `crates/canter-tui/`（代码、测试、README）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-77-theme`。

## 要做的

`/theme`（命令列表说明 `Choose the code colour theme`）打开选择框，样子大致这样（宽窗口左边列表、右边预览；窄窗口预览放在列表下面，照 Codex 的宽窄规矩）：

```
 Theme · code and diff colours
 › terminal          (current)   │   12   fn greet(name: &str) -> String {
   auto · catppuccin-mocha       │   13 -     format!("Hello, {}!", name)
   1337                          │   13 +     format!("Hello, {name}!")
   ansi                          │   14   }
   base16                        │
   …                             │   ```rust
   my-theme          (custom)    │   let x = 1;
   ↓ 21 more                     │   ```
 ↑/↓ preview · Enter save · Esc cancel
```

- 列表顺序：`terminal`、`auto`（后面写它现在选中的是 mocha 还是 latte），然后 two-face 自带的 32 个（名字和 Codex 那张表一样），然后 `~/.canter/themes/*.tmTheme`（设了 `CANTER_HOME` 时是该目录下的 `themes/`；名字用文件名；和内置重名的照 Codex 只列内置的；读不了的那一行标出来，选了也不换、不崩）。当前用着的那项标 `(current)`，打开时光标在它上面。
- `terminal`：就是现在的 `ansi`，用终端自己的 16 色，默认用它，什么都没配时和现在一模一样。
- `auto`：照 Codex，终端背景浅用 `catppuccin-latte`，深或判断不了用 `catppuccin-mocha`；背景深浅沿用 `theme.rs` 现在的判断（含 `CANTER_THEME`）。
- 上下移动时实时换：预览和对话里看得到的代码块、diff 一起换。Enter 保存到 `tui.json` 的 `theme`（照 P2-63 的读写办法，别的项不动），本终端立刻用上，别的开着的终端下次打开才换；Esc 还原成打开前的配色，不存。
- 只换语法的前景色：不铺配色自带的背景，界面别的颜色、diff 加删行的底色、用户消息底色都不变。
- 颜色深度：真彩色终端照配色原色；256 色终端换算成最近的 256 色；16 色终端和 `NO_COLOR` 时只有 `terminal` 起作用，选择框最上面写一句 `This terminal shows 16 colours; only terminal applies`，选别的照样能存，但照 `terminal` 画。
- 打开时 `tui.json` 里的 `theme` 找不到（内置里没有、文件没了或读不了）：照 `terminal` 画，提示一次 `Theme <名字> not found; using terminal`，不改 `tui.json`。
- 窗口矮时照 P2-63 选择框的规矩：先让对话区让地方，列表跟着光标滚、写 `↑ N more` / `↓ N more`，再不够收起预览。
- 帮助弹框的命令一节、README 讲颜色的地方补上。

## 怎么算做完

- （用户 10-27）照 Codex 做 `/theme`；只换代码和 diff 的配色；内置全部 32 个；`terminal` 和 `auto` 两个都给，默认 `terminal`；支持 `~/.canter/themes/` 的 `.tmTheme`。
- 验证只做这些：
  - 针对上面写测试，先写、确认在现在的代码上失败：列表顺序和 32 个名字；上下移动预览跟着换、Esc 还原、Enter 存进 `tui.json` 且别的项不变；重开终端读到存的配色；`auto` 在深、浅背景下各选对；自定义文件出现在列表里、和内置重名的不重复列、坏文件不崩；`tui.json` 里名字找不到时退回 `terminal` 并提示；16 色终端只照 `terminal` 画。
  - 什么都没配时现有画面测试和期望画面不变。
  - 做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的编译目录）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不换界面颜色（强调色、成功失败、diff 和用户消息底色），不铺配色的背景。
- 不改服务端、协议、核心；不碰 `config.json`。不加依赖，不升级已有依赖（two-face、syntect 现有的版本和功能够用；要开新的 cargo 功能先停下来问）。
- 不搬 Codex 的代码。
- 不读用户真实的 `~/.canter` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，不碰用户正在跑的服务端。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括某个配色在 two-face 里没有、换配色要动缓存或性能明显变差、和 P2-63 选择框的写法打架），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-77-theme` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
