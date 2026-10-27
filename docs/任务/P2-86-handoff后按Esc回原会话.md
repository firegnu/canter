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

## 完成记录

- 做了什么：`App` 记下 `/handoff` 开出的新会话的原会话 ID（`handoff_draft` 时设），只在这个会话没有轮、没有排队输入、本终端还没按 Enter 发过消息时生效。Esc 在主输入框分支里最后才判断（选区、搜索、btw、批准框、选择框和弹框、`/` 和 `@` 列表都在前面先拿走），第一次用状态行临时提示（和 `Mode: …`、`Copied …` 同一个 `copier.show`）显示 `Esc again to return to the original session`，2 秒后消失；2 秒内再按发 `Action::ReturnFromHandoff(原会话)`。`run.rs` 收到后和 `/resume` 选它一样 `resume` + `show`；把 `pick_session` 里这一段抽成 `Chat::open` 两处共用。打不开时照旧 `warning`（`Could not return to the original session: …`），界面不换、说明还在。切会话后整个 `App` 换新，功能自然没了。帮助弹框 Conversation 节加 `Esc twice`，根 `README.md` 的 `/handoff` 条、`crates/canter-tui/README.md` 讲 Esc 撤回那一段后面各补一句。
- 验证了什么：新加 `src/run_handoff_tests.rs` 6 个测试（两次 Esc 回原会话并看到原会话对话、说明丢掉；一次只提示、说明还在、还在新会话；隔 2 秒以上不回；发过消息后不切；弹框开着时 Esc 只关弹框、不算次数；`/new` 的会话不切）。先在只加了 `Action` 变体和 `Chat` 方法、没有 Esc 逻辑的代码上跑：4 个失败在提示或动作那一行，「发过消息后不切」「`/new` 不切」两个本来就过（它们守的是不该变的行为）。实现后 6 个全过。第一个测试走真的本机 HTTP（复用 `run_retract_tests.rs` 的假服务端，把它的 `Harness`、`Request` 改成 `pub(super)` 给同级模块用），回放 resume、events、models 三个回复。全套 `cargo nextest run --all-targets` 709 个全过，`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 都过，`tests/expected` 没有变化。
- 拿主意的地方：2 秒按「严格小于」算（和临时提示消失的时刻一致，2000 毫秒整算新的第一次）；中间按了别的键不清掉第一次 Esc（任务没要求）；按 Enter 发出消息就把功能关掉，发送失败草稿回来也不再恢复（「发出第一条消息之后就没了」）；`/btw`、`/compact` 等命令不算发消息，但会产生轮的命令（`/review`）产生轮后按快照判断也关掉。帮助那一行写 `Esc twice` / `After /handoff, before sending: back to the original session`。`crates/canter-tui/README.md` 原来没有讲 `/handoff` 的地方，补在讲 Esc 撤回那段末尾。
- 没做的事：没有为「原会话打不开时留在新会话、说明不丢」写测试（不在验证清单里，代码沿用 `/resume` 失败的同一条路）；没改服务端、协议、DESIGN。
- 踩过的坑：用 perl 正则改 `key_on` 里的 `matches!` 时 `|` 没转义，替换拼坏了一行，编译报错后用编辑器改回。

## 主控审查

- diff 只动了 `crates/canter-tui/` 和根目录 README 的 `/handoff` 一条：`handoff_draft` 记下原会话，主输入框的 Esc 在会话没有轮、没有排队输入时走两次 Esc；选区、搜索、btw、批准框、弹框、列表先拿 Esc 的顺序没动。切回原会话和 `/resume` 共用抽出来的 `Chat::open`，打不开时提示、留在新会话。
- 取舍：严格小于 2 秒、中间按别的键不清第一次、发过一次后就关掉（发送失败也不恢复）、`/btw` 不算发消息，都同意。「原会话打不开」没写测试，走的是 `/resume` 失败同一条路，任务没要，不补。
- 主控重跑：nextest 709 个全过，doc、clippy、fmt 通过。
结果：一次过
