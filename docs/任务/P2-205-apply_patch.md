# 任务：第二期-28.4 - apply_patch

2026-11-05，主控建。canter/main 交给 canter/dev-apply-patch（Kimi Code，默认 k3.1-pro；用户 11-05：「依旧派kimi吧」，原排 Codex high）；交叉审查交 Claude Code（opus[1m] / high）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害；用户 11-05 改派 Kimi，对比任务审查用 Claude high，不用读表的 review-codex）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：P2-80 接 GPT 订阅按 `docs/调研/P2-33-第二个模型方案.md` §8 分件，这是第 5 件：搬 Codex 的 apply_patch，接上批准、只读、保护路径、改动记录和撤销，只给支持它的模型。
依赖：P2-204、P2-210、P2-216
对比样本：第 4 件，Kimi 4 件 / Codex 0 件
执行：派出

## 用户原话

- 11-04 用户：「但是我想先做gpt，这样就齐全了。可以安心做eval」。§7 四个问题都选推荐（见 DESIGN 第 6 条 11-04 那一条），其中第④条：做 apply_patch，只给支持它的模型，write/edit 留给别家。
- 11-05 主控列出四件要定的事，用户都选推荐：
  1. 工具组合：「只给 apply_patch」（支持它的 GPT 模型拿 read、apply_patch、shell 等，不再给 write/edit）。
  2. 谁算支持：「看模型目录，config 同名也按目录」（以 OpenAI 模型目录里这个模型的 apply_patch 能力为准；config 手写的同名模型这一项仍从目录取；目录里没有、未核实、没登录拿不到目录时照旧 write/edit）。
  3. 中途失败：「如实报告，不自动退回」（写之前先把所有文件检查一遍，对不上一个都不写；真写到一半出错时报告哪些已改、哪些没改，已改的留着，想退用 /rewind）。
  4. 批准框：「一个框看全部改动」（一个补丁涉及的所有文件和差异一次展示、批一次；删除和改名单独标出）。

## 先读

- `docs/调研/P2-33-第二个模型方案.md` §4「改文件：加入 apply_patch，与 write/edit 共存」全节（推荐的执行边界、来源和许可证），§3 里 Responses 工具和 replay 的部分。
- `docs/DESIGN.md` 第 6 条 11-04 那一条、第 13 条 P2-23（改动备份和 /rewind）、P2-204、P2-210 两段、第 23 条（核心不绑某一家）。
- `docs/任务/P2-216-GPT模型目录.md`（模型目录、config 同名整条覆盖的现状）。
- 代码：`crates/canter-tools/src/lib.rs`（工具清单、`risk`）、`executor.rs`（文件读写）、write/edit 的 prepare、批准和备份那一套；`crates/canter-core/src/prompt.rs`（系统提示词按工具列表写的那几句）、`session.rs`（工具定义、换模型）；`crates/canter-model/src/responses/wire.rs`、`sse.rs`（工具怎么发、输出项怎么拼、replay 白名单）；`crates/canter-server/src/model_catalog.rs`、`config.rs`。
- Codex：在 `/tmp` 下只读克隆 `openai/codex` 的 `rust-v0.163.0`，看 `codex-rs/apply-patch`（语法、解析、匹配、fixtures）和 Responses 里 custom/freeform `apply_patch` 工具、`custom_tool_call` / `custom_tool_call_output` 的写法，以及 `ModelInfo` 里表示 apply_patch 能力的字段。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-205-apply-patch`，分支 `p2-205-apply-patch`（派的时候从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-205-apply-patch`。

## 要做成什么

1. **谁拿 apply_patch**：OpenAI 模型目录里这个模型表明支持 apply_patch（照 Codex `ModelInfo` 的字段）时，这个模型的会话工具里有 `apply_patch`、没有 `write` / `edit`；其余模型（别家、目录里没有、未核实、没登录拿不到目录）工具照旧。config 手写的同名 GPT 模型，这一项仍从目录取。会话中途换模型时，工具列表和系统提示词里按工具写的那几句跟着换（P2-207 再做完整的跨家切换，本件只保证换模型时工具对）。
2. **发给 GPT 的形式**：照 Codex，用 Responses 的 custom（自由文本）`apply_patch` 工具，模型发 `custom_tool_call`，canter 回 `custom_tool_call_output`，不错发成 function 的结果；这些条目进 P2-210 的 replay，同模型同账号照样回传。
3. **补丁本身**：从 Codex 搬语法、解析、匹配和变更逻辑和有价值的 fixtures，支持新建、修改、删除、改名。不搬 Codex 的执行服务、shell 包装、远程执行器和权限实现；实际读写走 canter 现有的执行器，删文件和改名补最小的受控能力。
4. **检查和批准**：一次补丁先全部解析、算出所有路径（删除和改名两端、新文件的父目录都算）、过保护目录和现有的路径规则、算好所有差异，再弹一个批准框看全部改动（删除和改名单独标出）；不是 Allow all 时批一次。同一补丁里重复或冲突的路径、不支持的写法直接拒绝。只读模式、审查轮、Plan 模式照 write/edit 的规矩拒绝。权限归到「改文件」这一类，沿用现有的改动条目和批准决定，不另开后门。
5. **写入**：批准后再核对一遍所有路径和原内容，有对不上的就一个都不写、让模型重读。逐个文件写入，不承诺跨文件原子；真写到一半出错或被取消，如实报告哪些已改、哪些没改，已改的留着，不自动退回、不重放。
6. **改动记录和撤销**：每个被改、新建、删除、改名的文件都照 write/edit 记备份，`/rewind` 能退回，改动列表和 `/diff` 看得到。
7. **来源和许可证**：搬来的每个文件开头注明 `openai/codex`、`rust-v0.163.0`、完整提交、原文件路径和本地改了什么，保留原许可证声明；按 Apache-2.0 新建并维护 `NOTICE`，只写实际搬进来的内容的归属。

屏幕上的文字都用英文。

## 怎么算做完

- 用户定的四条都做到：支持的 GPT 模型只有 apply_patch、没有 write/edit；按目录判断、config 同名也按目录；写到一半出错如实报告、不退回；一个批准框看全部改动。
- 不用 apply_patch 的会话（DeepSeek、Kimi、opencode go 等，以及不支持它的 GPT 模型）：工具列表、系统提示词、发给模型的请求逐字节不变；write/edit 的行为和测试不变。
- 验证只做这些：
  - 搬来的 fixtures 跑通；补丁解析的错误（格式错、上下文对不上、重复路径）都在写之前拒绝、一个文件不动。
  - 用本机假 Responses 服务端：支持的模型工具里有 custom `apply_patch`、没有 write/edit；模型发 `custom_tool_call` → 批准 → 写入 → 回 `custom_tool_call_output`；同模型下一轮 replay 原样回传这些条目；不支持的模型和别家照旧。
  - 安全：保护目录（canter 状态目录等）、工作区外路径、删除和改名的两端、新文件父目录都挡得住；只读、审查轮、Plan 模式拒绝；批准后原内容被改了就一个都不写。
  - 中途失败：模拟第二个文件写失败，报告第一个已改、第二个没改，第一个不退回；`/rewind` 能把已改的退回。
  - 改动记录：新建、修改、删除、改名都能 `/rewind` 退回，`/diff` 看得到。
  - 别家逐字节：现有的 `chat_completions_renders_exact_bytes_*`、`legacy_requests_keep_the_same_bytes_*`、`fork_preserves_request_bytes_*` 等照样过、不改断言；缺的补上。
  - 断言日志、事件、错误里不出现补丁里的文件内容以外不该出现的东西（令牌、密文）。
  - 改到哪个包就跑哪个包的全部测试（`cargo nextest run -p <包>`）；改到发给模型的请求或 prompt，`canter-server` 和 `canter` 两个包也跑。加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。全套由主控合并前跑，你不跑。

## 不要做

- 不改别家的工具、提示词和请求；核心逻辑里不按 DeepSeek、Kimi 或 GPT 的名字、`base_url` 分支，按模型能力判断。
- 不按模型名里有没有 `gpt` 猜能力。
- 不引入 Codex 的执行服务、shell 包装、远程执行器、权限实现；不顺手升级别的依赖。
- 不做跨文件原子事务、不自动退回、不重放补丁。
- 不做 P2-207 的跨家切换历史转换（本件只保证换模型时工具和提示词对）。
- 不改 P2-210 的 replay 规则，只让新的 custom 条目按同一规则走。
- 不连真的 `api.openai.com`、`auth.openai.com`，不做真实请求。
- 不碰用户真实的 `~/.canter`、会话、配置和登录记录，不碰用户正在跑的 canter 服务端；测试用隔离的 HOME 和临时端口、临时目录。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的服务用起的时候记下的 PID。
- 不用 Python。屏幕上的文字用英文。
- 碰到要拿主意的事（包括目录里找不到能力字段、Codex 的写法在 canter 里接不上、要改现有测试断言才能过、四条规则在某条路径上说不通），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-205-apply-patch` 上提交。

## 合并以后（用户和主控做）

- 用户装好新版，在 canter-test-rust 用 GPT 真调：让它用一个补丁改两个文件、新建一个文件、删一个文件，看批准框、改动列表、`/rewind`；再切回 DeepSeek 确认工具是 write/edit。主控看日志，次数和结果记进本文件。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
