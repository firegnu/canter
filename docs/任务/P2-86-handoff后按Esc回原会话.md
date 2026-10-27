# 任务：/handoff 开出的新会话里，没发消息前按两次 Esc 回原会话

2026-10-27，canter/main 交给 canter/dev-handoffback（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.71）、不要、改行为）
类型：功能变更
依据：DESIGN 第 19 条末尾「handoff 后回原会话（P2-86…）」一句。`/handoff` 写完说明后开新会话、把说明放进输入框；现在不想交接了只能 Ctrl+C 清掉再 `/resume` 选回去。本件让新会话里没发消息前按两次 Esc 直接回原会话。只改终端界面。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27 用户问：「handoff之后，进入handoff的界面，加入我这时候不想handoff了，如何取消啊？」主控说明现在只能 Ctrl+C 再 `/resume`，提议改进；用户：「另开一件吧，新会话里没发前按 Esc 回原会话」。
- 10-27 主控问了两处，用户都选推荐：按两次 Esc（第一次提示，约 2 秒内再按才回，防手滑丢掉说明）；handoff 开的空会话留着不管（只改终端，`/resume` 列表里留一条 `(no messages yet)`）。

## 先读

- `docs/DESIGN.md` 第 19 条末尾「handoff 后回原会话（P2-86…）」一句；第 8 条讲交接（P2-10）那一行。
- `crates/canter-tui/src/run.rs`：主循环里 `take_handoff` 之后调 `new_session(…, Some(note))`（约 493 行）；`new_session`（约 818 行，`source` 就是原会话 ID）；`/resume` 切会话的 `pick_session` 和 `show`。
- `crates/canter-tui/src/app.rs`：`handoff_draft`、`take_handoff`；`key_action` 里 Esc 的先后（选中高亮、搜索、btw、批准框、弹框、列表先拿 Esc，约 950–1080 行）；状态栏上现有的临时提示怎么显示、怎么过期。
- 根目录 `README.md` 讲 `/handoff` 那一条（约 55 行）和 `crates/canter-tui/README.md` 讲 Esc 的地方。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-86-handoff-back`，分支 `p2-86-handoff-back`（派的时候从 main 建好）。
- 只动 `crates/canter-tui/`（代码、测试、README）和根目录 `README.md` 讲 `/handoff` 的那一条。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-86-handoff-back`。

## 要做的

- 只在 `/handoff` 刚开出的那个新会话里、这个会话还没发过消息（没有轮、没有排队的输入）时生效：
  - 第一次 Esc：状态栏照现有临时提示的写法显示 `Esc again to return to the original session`，别的不变。
  - 约 2 秒内再按 Esc：切回原会话（和 `/resume` 选它一样显示原会话的对话），输入框里的交接说明丢掉。超过 2 秒再按，算新的第一次。
  - 新会话留在服务端，不删不关；`/resume` 列表照旧有它。
- 在新会话里发出第一条消息之后、或者已经切到别的会话（`/new`、`/resume`、回过原会话）之后，这个功能就没了，Esc 照现在的规矩。
- Esc 的先后不变：选中的高亮、搜索框、btw、批准框、提问框、弹框、命令列表、`@` 列表等开着时照旧先拿 Esc，不算进这两次。
- 原会话打不开（比如服务端报错）时，用现有的警告写法提示，留在新会话，说明不丢。
- 帮助弹框（P2-53）讲 Esc 的地方、两份 README 讲 `/handoff` 的地方各补一句。

## 怎么算做完

- （用户 10-27）新会话里没发前按 Esc 回原会话；按两次（第一次提示、约 2 秒内再按才回）；handoff 开的空会话留着不管。
- 验证只做这些：
  - 针对上面写测试，先写、确认在现在的代码上失败：两次 Esc 回到原会话、看到原会话的对话；只按一次只出提示、还在新会话、说明还在；隔了 2 秒以上再按不回；发过消息后两次 Esc 不切会话；有弹框开着时 Esc 只关弹框；不是 handoff 开的会话（`/new`）两次 Esc 不切会话。
  - 现有画面测试和期望画面不应变。
  - 做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的编译目录）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改服务端、协议、核心；不加删除或关闭会话的接口；不在 `/resume` 列表里藏空会话。
- 不改 `/handoff` 生成说明那一段（生成中 Esc 取消照旧）。
- 不加依赖，不升级已有依赖。
- 不读用户真实的 `~/.canter` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，用本机假服务端，不碰用户正在跑的服务端。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括 Esc 和现有某个用法打架、切回原会话的现有办法用不上），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-86-handoff-back` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
