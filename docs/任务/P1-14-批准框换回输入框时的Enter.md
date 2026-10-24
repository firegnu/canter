# 任务：批准框被别的终端答掉、换回输入框的一瞬间，Enter 不把草稿发出去

2026-10-24，canter/main 交给 canter/dev-approval（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.98）、拿不准、拿不准）
类型：Bug 修复
依据：P1-10 第 5 条（P1-7 留下的已知问题）的后半条。前半条（终端卡顿超过 300 ms 时保护期挡不住）程序看不到终端什么时候真正显示，没有可靠的修法，P1-7 用户定保护期时已接受，不在本件。
提示：依据证据定位并修复导致问题的原因，保持无关行为不变。
依赖：P1-12
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 用户 10-24 听了 P1-10 五条后：「照建议，先做 4 和 5，两件一起派」。主控说的第 5 条后半：「批准框被别的终端答掉、换回输入框的那一瞬间，你按的 Enter 会把输入框里没写完的话发出去。只是发消息，不会执行命令。」

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/任务/P1-7-终端客户端二和命令行.md`「审查后修改（复核第 2 轮：用户决定加保护期）」：现在的帧号保护和 300 ms 保护期是怎么定的。
- `crates/canter-tui/src/app.rs`（`ANSWER_GUARD_MS`、`guard_until`、按键处理）、`crates/canter-tui/tests/approval_box.rs`。

## 在哪里干活

- worktree：`../canter-worktrees/p1-14-approval-enter`，分支 `p1-14-approval-enter`（已从 main 建好）。只动 `crates/canter-tui/src/app.rs` 和 `crates/canter-tui/tests/`（`crates/canter/tests/tui.rs` 要加测试也可以）。编译目录 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p1-14-approval-enter`。
- 并行任务：canter/dev-closereason 在分支 `p1-13-close-reason` 改协议、服务端和 `crates/canter-tui/src/run.rs`、`client.rs` 的重连逻辑。你不要动这些文件。

## 要做的

- 批准框因为别的终端答掉（或请求被取消）而换回输入框时，照批准框出现时的办法给输入框也加一段保护：这段时间里的 Enter（发送）丢掉，不发草稿；还在按键就顺延。时长沿用 `ANSWER_GUARD_MS`。打字、删字、Esc、Ctrl+C 等照常。
- 用户自己在本终端答完批准框换回输入框，算不算也要保护，你看现有代码定，写进完成记录。
- 保护期内输入框要不要有提示（例如像批准框那样画暗），照 P1-7 批准框的做法，不要另起一套样式。

## 怎么算做完

- 批准框被别的终端答掉、换回输入框的一瞬间按 Enter，不会把输入框里的草稿发出去；之后再按 Enter 照常发送。（用户 10-24「照建议」，主控按问题描述写）
- 验证：先写测试复现（两个终端，一个答掉批准框，另一个紧接着按 Enter），确认它因为这个问题失败；改完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`。不调真实模型。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改批准框本身的保护期和帧号保护，不改重连逻辑和协议。
- 不调真实模型，不碰用户的 `~/.canter`；测试用隔离目录和假服务端。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的进程用记下的 PID。
- 碰到要拿主意的事（包括要改上面「并行任务」里的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p1-14-approval-enter` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-10-24，canter/dev-approval。

- **做了什么**（只动 `crates/canter-tui/src/app.rs` 和测试）：
  - 新加 `send_guard_until`，和批准框的 `guard_until` 分开记。某一帧从批准框换成输入框、上了屏，就从那一刻起算 `ANSWER_GUARD_MS`（300 ms）。
  - 保护期内输入框里的 Enter 不发送，草稿留着。任何键进来都从这个键起再算 300 ms。打字、删字、Shift+Enter / Ctrl+J 换行、Esc、Ctrl+C / Ctrl+D 都照常。
  - 提示照 P1-7：保护期内输入框的几行画暗，文字不变。换回输入框的第一帧就是暗的，过后下一次重画恢复。
  - 批准框的保护期、帧号保护、重连和协议都没动。
- **拿主意的地方**：
  - 本终端自己答完批准框、换回输入框，**也加保护**。理由：现有批准框保护期不管是谁引起的换框（本终端答了 A、B 跟着出来，也算），只看屏幕上能按的东西变没变；连按两下 Enter 时，第二下落在输入框那一帧上，和被别人答掉是同一个风险。
  - 请求被取消（例如服务端重启后）换回输入框，同样加保护。
  - 没有和 `guard_until` 共用一个时间，因为共用会多出一个无关的改动：批准框出现前按在输入框那一帧上的 Enter，到达时间如果晚于批准框上屏（还在它的保护期内），会从「发送」变成「丢掉」。分开记就不会。
- **验证**：
  - 先写复现、确认失败，再改：
    - `crates/canter/tests/tui.rs` 新加 `enter_right_after_another_terminal_answers_does_not_send_the_draft`：两个终端都用真服务端、假模型；B 输入框里有草稿，A 答掉批准框，B 一看到输入框回来就按 Enter。改之前失败：草稿被发出去，会话里多了第 2 轮（`(2, 0)` 对 `(1, 0)`）。改之后草稿没发；过一秒再按 Enter 照常发出，两个终端都看到回复。
    - `crates/canter-tui/tests/approval_box.rs` 新加 `enter_right_after_the_box_closes_does_not_send_the_draft`（TestBackend，测试控制时间）：保护期内 Enter 不发；打的字照常进输入框并顺延保护期；快到期时 Enter 仍不发；画暗和恢复；之后 Enter 发出 `草稿!`。改之前失败。
    - 新加 `enter_right_after_answering_does_not_send_the_draft`：本终端答完后马上再按 Enter 不发，过后照常发。
  - 反向检查：去掉「按键顺延」后，`enter_right_after_the_box_closes_does_not_send_the_draft` 在「顺延了保护期」那条断言失败。
  - 原有的 `approval_box_matches_the_server_after_a_restart` 在批准框被取消、换回输入框后马上打字回车，新的保护期把这次 Enter 丢掉了，测试因此失败。照 P1-7 的做法，打字前先等保护期过去（`read_the_box`），没有删测试，也没有放宽断言。
  - `cargo test --all-targets`：144 个全过；`cargo test -p canter --test tui`：10 个，连跑 3 次都过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 都过。
  - 真实模型接口调用 0 次。测试起的服务端都停了；机器上还在跑的 `~/.cargo/bin/canter serve` 是用户装的那份，没碰。
- **代价**：
  - 和批准框一样：换回输入框后一直不停地按键，保护期就一直往后推。停手 300 ms 以后，Enter 才发得出去。如果框一消失就连着打完一句话、紧接着回车，这次回车会被丢掉（草稿是暗的），要再按一次。
  - 终端卡顿超过 300 ms 时挡不住（同 P1-7，不在本件）。
- **没做的事**：没改批准框保护期、帧号保护、重连、协议和 DESIGN（DESIGN 不涉及按键）。
- **踩过的坑**：
  - 新测试第一次用 `x` 当草稿，`dim("x")` 找到的是命令 `first-proof.txt` 那一行，换成 `稿`。
  - 我在一条追加测试的 shell 命令末尾误带了一个 `python3 -`，它等着标准输入、卡到超时，被我停掉。没有执行任何东西，测试文件内容核对过是对的。
