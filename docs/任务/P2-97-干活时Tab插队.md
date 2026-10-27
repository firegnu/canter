# 任务：干活时 Tab 插队

2026-10-27，canter/main 交给 canter/dev-steer（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.5）、拿不准、拿不准；主控按协议和核心已支持插话、本件只改终端，定不交叉审查；按键和显示，按「界面和设计」）
类型：功能变更
依据：用户 10-27 插的一件。模型干活时，Enter 照旧排队（这一轮结束后另开一轮），新加 Tab 插队：用协议已有的 `mode: steer` 把这句话塞进正在跑的这一轮，模型下一次请求时看到。服务端和核心已支持插话（DESIGN 第 8 条），终端一直只发 `Queue`（`crates/canter-tui/src/run.rs` 第 1228 行）。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27 用户：「稍等，插一个任务。我记得codex和Claude code都有一个功能，如果当前正在工作，正常情况下，如果再次发出一个prompt，默认是排队，但是可以使用快捷键进行插队。你查一下。我觉得这个功能对我也很有用。现在canter支持到什么程度？」
- 主控查了：Codex `rust-v0.163.0` 干活时 Enter 是插队（`turn/steer`）、Tab 是排队；canter 协议和核心已支持插话，终端只发排队。用户选「Enter 排队，Tab 插队」。

## 先读

- `docs/DESIGN.md` 第 8 条「送话」「`input.removed`」「手动压缩期间的插话按排队处理」「审查输入始终排队」那几句；第 19 条里 P2-59 Esc 撤回刚发的消息、P2-53 `?` 帮助、P2-25 输入历史那几句。
- `crates/canter-tui/src/run.rs`（发送，第 1212 行附近）、`crates/canter-tui/src/app.rs`（按键：Tab 在斜杠、`@` 列表里补全，第 1128、1151 行附近；排队消息怎么显示，`Queued`）、`crates/canter-tui/src/retract.rs`（已认 `InputRemovalReason::Steered`）。
- `crates/canter-core/src/session.rs` 第 571 行附近（收到插话怎么处理：有活动轮且不是审查、不是手动压缩才插，否则排队）、`crates/canter-core/src/runner.rs`（插话什么时候进上下文）。只读，不改。
- 参考 Codex `rust-v0.163.0`（`/tmp/r49-src/codex`，只读）：`codex-rs/tui/src/keymap.rs` 第 1690 行附近（`submit` Enter、`queue` Tab）、`codex-rs/tui/src/app/thread_routing.rs` 第 750 行附近（`turn_steer`，轮刚好结束时的处理）、`codex-rs/tui/src/bottom_pane/footer.rs`（底部提示 `tab to queue`）。只参考，不搬代码。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-97-steer`，分支 `p2-97-steer`（派的时候从 main 建好）。只动 `crates/canter-tui`、`docs/DESIGN.md` 第 19 条和本文件。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-97-steer`。

## 要做的

1. **按键**：模型在干活时（一轮在跑，包括等批准、等回答），输入框有字、没有斜杠或 `@` 列表开着，按 Tab 用 `mode: steer` 发出；Enter 照旧 `Queue`。列表开着时 Tab 照旧补全。空闲时 Tab 不做事（照现在）。
2. **哪些不插**：斜杠命令不插（照现在的规矩处理）；审查轮、手动压缩时核心会自动改成排队，终端照服务端的回复显示（排队就显示成排队），不另做判断。带图片的消息照常可以插。
3. **显示**：
   - 发出后、进上下文之前，显示成待插入的样子，和排队的分得开（比如标 `↳ steering` 一类的灰字）。
   - 核心把它放进这一轮（`input.removed` 原因 `Steered`）后，这条用户消息显示在这一轮里它进来的位置，照已发的用户消息画（P2-48 底色）。
   - 服务端回复说排了队（没给 `turnId`，比如这一轮刚好结束了），就照排队显示。
   - 重开会话、`/resume` 后显示一样。
4. **底部提示**：干活时输入框有字，状态行或底部提示写一句 `Tab to steer`（照 Codex 底部提示的位置和样子），空闲时不显示。
5. **和已有功能**：Esc 撤回（P2-59）对插队的消息照现在核心的规矩，不另做；P2-25 输入历史照记；发送失败照 P2-82 放回草稿。
6. 加进 `?` 帮助；DESIGN 第 19 条补一句定法（Enter 排队、Tab 插队，和 Codex 相反的理由：用户要「默认排队、快捷键插队」），提交说明里写。

## 怎么算做完

- （用户 10-27）正在工作时再发一个 prompt 默认是排队，可以用快捷键插队；用户选「Enter 排队，Tab 插队」。
- 验证只做这些：针对 1、3、4 写测试（用本机假服务端或现有的终端测试办法，看发出去的 `mode` 和显示）；再跑一次项目的标准测试：`cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改协议、服务端、核心插话的规矩、存盘格式（要改就停下问）。
- 不做按键配置，不做「把已排队的改成插队」、不做 Codex 的「拿回最后一条排队的改」。
- 测试用隔离的 HOME 和 `CANTER_HOME`、临时端口，不读用户真实的 `~/.canter`，不调真实的模型接口。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-97-steer` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- **做了什么**：
  - 按键（`app.rs`）：这一帧画着一轮在跑（`Shown.turn`，包括等批准、等回答时输入框露着的情况），没有斜杠、`@` 列表开着，Tab 发 `Action::Steer`，`run.rs` 照 Enter 的发法发出，只是 `mode: steer`；Enter 照旧 `Queue`。列表开着 Tab 照旧补全；空闲时、空输入框、斜杠命令（`commands::parse` 认得的）、`!命令` 按 Tab 不做事。带图片的照常插。Enter 原来发消息那段抽成 `send(steer)`，两边共用（交接中、图片还在发、换行号历史、`sending_text` 草稿都一样）。
  - 显示（新文件 `steer.rs`）：服务端回复的 `eventSeq` 和 `input.queued` 事件的 seq 是同一个，两者先后不定；回复带 `turnId` 的那条排队项记成插话，画成灰色 `  ↳ Steering: …`，排在 `· Queued` 前面；回复没带 `turnId` 就照排队显示。核心放进这一轮（`input.removed`）后标记去掉，这条消息由已有的 `UserMessage` 画法画在这一轮里（P2-48 底色），这部分原来就这样画，没改。`/resume` 切走再切回时，标记跟着带过去（`with_steering_from`）。
  - 底部提示：干活时输入框有能插的字、上面没有列表，状态行 `Esc to interrupt` 后面加 ` · Tab to steer`（等批准的那一行同样加在用时后面）。Codex 是写在输入框下面的底栏；canter 输入框下面两行是状态栏（可由脚本或 `/statusline` 定制），放进去会和用户定制的内容打架，所以放在任务允许的状态行。
  - `?` 帮助输入框一节加 `Tab (while working)`；DESIGN 第 19 条补了一条定法和理由。
- **验证了什么**：`run_steer_tests.rs` 11 个测试，走真的本机 HTTP（沿用 `run_retract_tests.rs` 的 `Harness`，把它几个方法改成 `pub(super)`）：Tab 发 `mode: steer`、Enter 发 `mode: queue`；回复先到、事件先到两种顺序都显示 `↳ Steering`；回复没 `turnId` 显示 `Queued`；插话排在排队前面；带图片可以插；空闲、斜杠命令、`!命令`、空白时 Tab 不发；斜杠列表开着 Tab 照旧补全；提示只在干活且有字时出现；发送失败放回草稿、记进输入历史；重开会话后插进来的消息画在这一轮中间、铺用户底色；切回会话后标记还在。临时关掉 Tab 那一支时 6 个发送相关的测试失败，恢复后全过。全套：`cargo nextest run --all-targets` 810 个全过，`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 都过。
- **拿主意的地方**：
  - 「重开会话、`/resume` 后显示一样」：已进上下文的插话从快照画，一样；还没进上下文的插话，协议的排队项不分排队和插话，只有发出它的终端知道。`/resume` 切回时带着标记；重开 canter、别的终端看，会显示成 `Queued`，等模型下一次请求时变成这一轮里的用户消息。要补全得在协议里给排队项加个标记，任务说不改协议，没做，写进 DESIGN。
  - 斜杠命令按 Tab：理解为「照现在 Tab 的规矩」，什么也不做（不当 Enter 执行），提示也不显示。
  - 发出后到服务端回复之间，这条先按 `Queued` 画（事件通常先到），回复到了改成 `Steering`，本机通常只有几毫秒，没去消这一下。
- **没做的事**：照任务不做按键配置、把已排队的改成插队、拿回最后一条排队的；没改协议、服务端、核心。
- **踩过的坑**：没有。

## 主控审查

- diff 只动了 `canter-tui`、DESIGN 第 19 条和本文件；协议、服务端、核心插话规矩、存盘没动。Tab 和 Enter 共用一段发送代码，输入历史、发送失败放回草稿照旧；列表开着时 Tab 照旧补全；斜杠命令、`!` 命令、空输入框按 Tab 不做事。
- 重跑标准测试：nextest 810 个，809 过，1 个失败是 `canter-model::streaming cancellation_interrupts_retry_after_and_local_backoff`（HANDOFF 已记的偶发，本件没碰 `canter-model`），单独复跑通过；文档测试、clippy、fmt 过。
- 取舍同意：重开 canter 或别的终端看，还没进上下文的插话显示成 `Queued`（要补得改协议，空档很短，不另开）；斜杠命令按 Tab 不执行；刚发出时先闪一下 `Queued`；`Tab to steer` 放在状态行，不放输入框下面（那两行是用户自定的状态栏）。
- 建议改（不挡合并）：判断 `!` 命令用的是输入框显示的文字（`steerable` 用 `text()`），整段 `!命令` 被收成 `[Pasted …]` 标记时，Tab 会把它当普通消息插给模型。少见。
结果：一次过

## 用户试用

- 10-27 装 `b78205d` 后照验收清单试，用户：「我测试了一下，基本没什么问题」。
