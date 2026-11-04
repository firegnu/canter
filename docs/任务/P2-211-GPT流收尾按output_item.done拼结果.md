# 任务：第二期-28.3a - GPT 真调收尾报错：按 output_item.done 拼本轮输出

2026-11-04，主控建。canter/main 交给 canter/dev-responses-fix（Codex，gpt-6-astra / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.46）、拿不准、拿不准；主控判：只改适配器里的流解析，工具仍在 completed 之后执行、回传和脱敏规则不变，不算碰要害）
类型：Bug 修复
提示：依据证据定位并修复导致问题的原因，保持无关行为不变。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：P2-204 合并后用户真调 2 次都在流收尾时报 `unexpected error`；本件只修这一处收尾，并让核对失败时服务端日志说清是哪一项。跨轮回传密文仍是 P2-210。
依赖：P2-204
执行：派出

## 用户原话

- 11-04 用户真调后贴图：两次都是 `✗ Model request failed: unexpected error`。
- 11-04 用户选「开 P2-211 修」：修法照 Codex，用流里每条输出的 `output_item.done` 拼出结果，完成事件只用来确认结束和取用量。
- 11-04 用户选「要，记一句」：服务端日志记一句是哪一项核对没过（比如 `completed output missing item 2`），只记 canter 自己的说明、不记服务器返回的原文。

## 现场

- 配置 `chatgpt/gpt-6.1-sol`（`"api": "responses"`、`"auth": "chatgpt"`），装的是 main `88bf2d8`。
- 普通问答：文字正常流出，用量记到（↑8.7k ↓42），最后报 `unexpected error`。
- 让它读 README.md：没出文字（↓19，像是只出了工具调用），同样报错，工具没执行。
- `~/.canter/server.log` 没有任何细节。
- 主控看代码的推测（没有原文证实）：用量来自 `response.completed`，说明这个事件到了；之后 `crates/canter-model/src/responses/sse.rs` 的 `State::accept` 拿 `response.output` 和流里 `output_item.added/done` 记下的条目按位置核对 id、类型，再从 `response.output` 里取完整条目，任何一处对不上都是 `ErrorKind::Other`（屏幕上的 `unexpected error`）。`responses.rs` 的回传也从 `event["response"]["output"]` 取。Codex `rust-v0.163.0` 的 `codex-rs/codex-api/src/sse/responses.rs` 只用 `response.output_item.done` 拼结果，`response.completed` 只取 id 和用量，它的测试里 completed 的 `output` 是 `[]`。订阅这条线路的 completed 很可能不带完整 output，或带的和流里不一致。

## 先读

- 本文件「现场」。
- `docs/任务/P2-204-Responses接口.md` 的「要做成什么」、「用户真调」两节；`docs/DESIGN.md` 第 6 条里 P2-204 那几段。
- 代码：`crates/canter-model/src/responses.rs`（`attempt`、回传 `Replay::Responses` 那段）、`responses/sse.rs`、`responses/wire.rs`（`replay_output`）、`responses/edge_tests.rs` 和 `responses.rs` 里的测试。
- 参考 Codex `rust-v0.163.0` `codex-rs/codex-api/src/sse/responses.rs`（`process_responses_event` 和它的测试），只读克隆在 `/tmp/canter-p2-202-codex-reference`。只参考，不搬。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-211-responses-done`，分支 `p2-211-responses-done`（已从 main 建好）。只动 `crates/canter-model/src/responses.rs`、`crates/canter-model/src/responses/`，和这几处对应的测试；日志要落到服务端时可以动 `crates/canter-server/src/model_auth.rs`。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-211-responses-done`。

## 要做成什么

1. 本轮的输出（文字、思考摘要、工具调用、要回传的原样条目）以流里各条的 `response.output_item.done` 为准拼出来，顺序按 `output_index`。`response.completed` 只用来确认这一轮正常结束、取用量；它的 `output` 是空的、缺条目、或和流里不一致时，都不因此报错。
2. 原来的规矩不变：工具调用要等 `response.completed` 到了才放出去执行（整批放）；`failed`、`incomplete`、顶层 `error`、没等到 completed 就断流，仍然报错、不执行工具；本轮回传给下一次请求的条目（含 `encrypted_content`、phase、item id、call_id、原始参数）和现在一样原样带上，只是来源换成 `output_item.done`。
3. 流里的核对没过（比如增量的 item_id 对不上、同一位置换了 id、done 里缺必需字段、工具名不在本轮工具里），屏幕上的错误种类照旧，另外在服务端日志（`~/.canter/server.log`）写一句英文，说明是哪一项没过，例如 `Responses stream: output_item.done at index 2 has no id`。这句只用 canter 自己的固定文字，可带位置序号、事件类型名这类 canter 自己判断出来的东西；不写服务器返回的任何字段值（id、文字、参数、密文、错误原文都不写）。

## 怎么算做完

- 用户原话那三条做到：用 output_item.done 拼结果、completed 只确认结束和取用量；核对没过时日志有一句 canter 自己的说明、没有服务器原文。
- 合并后用户在场再真调 2 次（普通问答、读一个文件），两次都正常回答（这一条由主控和用户做，你不做）。
- 验证只做这些：
  - 先加测试，用假 Responses 服务端放一个 completed 的 `output` 为 `[]` 的流（分别是纯文字、带一次工具调用两种），确认现在因为这个缺陷失败，再改到通过。
  - 加一条：核对没过时日志里有那句说明，且不含假服务端发来的 id 和文字。
  - 原有 Responses 测试（含两次工具循环原样回传、completed 之前不执行工具、各种失败流、密文不进公开事件和日志）照样通过，不放宽断言。
  - `cargo nextest run -p canter-model`；动了 `canter-server` 就再跑 `cargo nextest run -p canter-server`。再加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。全套由主控合并后跑，你不跑。

## 不要做

- 不连真的 `api.openai.com`、`auth.openai.com`，不做真实请求。
- 不改请求格式（`wire.rs` 发出去的部分）、重试和 401 恢复、后台同意、Chat Completions 和别家的任何代码。
- 不做跨轮、重启、压缩的密文回传（P2-210），不做模型目录（P2-206）。
- 不把服务器返回的原文、令牌、密文写进日志、错误或公开事件。
- 不碰用户真实的 `~/.canter`、会话、配置和登录记录，不碰用户正在跑的 canter 服务端。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的服务用起的时候记下的 PID。
- 不用 Python。
- 屏幕上和日志里的文字用英文。
- 碰到要拿主意的事（包括要改现有测试的断言才能过、发现收尾报错另有原因），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-211-responses-done` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
