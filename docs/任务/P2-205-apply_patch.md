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

- 用户装好新版，在 canter-test-rust 用 GPT 真调：让它用一个补丁改两个文件、新建一个文件、删一个文件，看批准框、改动列表、`/rewind`；再切回 DeepSeek 确认工具是 write/edit。（11-05 交叉审查建议 7：切回 DeepSeek 放在同一个会话里做，看历史里的补丁调用会不会被拒。）主控看日志，次数和结果记进本文件。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

### 做了什么

- 从 openai/codex `rust-v0.163.0`（完整提交 `83086188ae0614bd882818ff1aa23f3194962012`）迁入 apply_patch 的语法、解析、匹配、变更逻辑、Lark grammar 和 24 个场景 fixtures；新增 `NOTICE` 及逐文件来源说明，本地执行仍走 canter 的 Executor。
- 只有目录项 `apply_patch_tool_type == "freeform"` 的模型启用 apply_patch，并隐藏 write/edit；config 同名模型也按目录。新建、恢复、热重载和换模型都更新工具与请求 grammar。
- Responses 接入 custom 工具、`custom_tool_call` 增量事件、`custom_tool_call_output` 和白名单 replay；诊断只记类型，不记补丁输入或密文。其它接口不写 custom 字段。
- 补丁支持新增、修改、删除、改名；全部路径和原文在批准前预检、批准后复查，一个批准框展示所有 diff，并标出删除和改名。写入逐文件进行，部分失败如实报告且不自动回滚。
- 每个实际变更文件分别记录 rewind 备份，删除和改名源支持“预期不存在”的比较；部分失败只保留已写路径的备份。改动项目、used files、PostToolUse 和 LSP 通知覆盖多文件。

### 验证了什么

- `cargo nextest run -p canter-model`：45 通过；包括 custom 工具定义、流式调用、结果类型和同模型同账号 replay，以及既有字节兼容测试。
- `cargo nextest run -p canter-tools`：206 通过、22 跳过；迁入 fixtures、格式/上下文/重复路径预检、保护路径、工作区外、Plan、批准后原文变化和第二次写失败均通过。
- `cargo nextest run -p canter-core`：246 通过、23 跳过；新增本机假 Responses 服务的真实 Core 工具循环，覆盖一次批准、增删改移动、custom 结果与 replay、全部 `/rewind`、部分失败后只回滚已写文件、审查/Plan 拒绝及换模型。
- `cargo nextest run -p canter-server`：366 通过、4 跳过；目录 freeform、缺字段、未知值和 config 同名模型行为通过。
- `cargo nextest run -p canter-tui`：679 通过；`cargo nextest run -p canter`：208 通过。
- `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。所有命令均在前台完成，未跑 workspace 全套测试。

### 拿主意的地方

- `Request` 增加不序列化的 `custom_tools` 映射，避免给所有 ToolDefinition 构造点加字段，也保证 Chat Completions 字节不变；ToolCall 沿用统一结构，由当前 custom 工具表决定 Responses 的结果类型。
- 上游 015 fixture 用第二个操作引用缺失文件来制造中途失败，与本件“全部预检，失败一个都不写”冲突。本地删除其 `expected/created.txt`，并在 fixtures README 说明；真正的第二文件写失败另用故障注入 Executor 和 Core 权限故障测试覆盖。此项请主控复核。
- 部分失败的 FileChange 只保留实际已提交路径的 diff；改名目标写成功但源删除失败时保留目标和源的实际状态，不伪装成完整改名。

### 没做的事

- 未连接真实 OpenAI/OAuth 服务，未碰用户真实 `~/.canter` 或运行中的服务；未做 P2-207 跨家历史转换。
- 未做跨文件原子事务、自动回滚或补丁重放；未合并 main、未推送。真实模型和终端人工验收留待合并后按上文执行。

### 踩过的坑

- 多文件 PostToolUse 改造最初把既有单文件提示改成了绝对路径，服务端回归测试发现后已恢复原有相对路径输出，最终完整服务端测试通过。
- 本机假服务会校验测试 Authorization 头；初次集成测试用了不同的占位值导致重试，改用共享测试常量后通过。

### 返工

2026-11-05，按交叉审查只改指定的三条：

- `file_rules` 先计算补丁所有路径的规则结果，再优先处理 Forbidden 和 Allow all 下不可读的规则，最后才处理 Prompt，避免前一个 Prompt 掩盖后一个 Forbidden。新增回归测试覆盖“前 Prompt、后 Forbidden”：初次检查及 `resolve(Accept)` 都禁止，不弹批准框且不写任何文件。
- `apply_patch.lark` 移除来源注释，与 Codex 原文件 `diff` 无差异；来源说明移到 `mod.rs`。Responses 集成测试断言实际发送的 grammar definition 不含来源、提交号和许可证注释。
- write/edit 的批准框文件行恢复 main 的原文；Create/Delete/Move 动作前缀只用于 apply_patch。新增断言固定 write 新建文件仍显示 `foo.txt  +1`，apply_patch 才显示 `Create foo.txt  +1`。

定向验证：canter-tools 的 apply_patch 7 项、file_rules 14 项，canter-core 的 Responses custom patch 集成测试，canter-tui 的新批准框单元测试均通过。随后运行 Clippy、fmt 和 diff 检查；不跑全套。

## 主控审查

2026-11-05，canter/main。开发：Kimi Code 默认 k3.1-pro（canter/dev-apply-patch-1），派出 15:07 → DONE 16:19，约 72 分钟，中途没停下问；返工 16:29 → 16:38，约 9 分钟。用户在它窗口里有过三次输入（15:58、16:06、16:36）。交叉审查：Claude Code opus[1m] / high（canter/review-apply-patch-1），一轮加一次复核。
- 主控看 diff：范围和完成记录对得上；015 fixture 改成「一个都不写」同意（用户定的是先全部检查、对不上一个都不写）。
- 交叉审查必须改 1 条：一个补丁里先碰到「询问」规则的路径就不再查后面「禁止」规则的路径，询问模式下点同意会写进被禁止的文件（漏查路径，权限）。连同建议 2（发给 OpenAI 的 grammar 多了来源注释，改成和 Codex 逐字节一致）、建议 3（write 新建文件的批准框多了 `Create`，恢复原样）一起交回；复核：可以合并。
- 建议 4～8 没改，记进 HANDOFF。
- 合并前全套（返工后的 `6fd92f6`）1824 项全过、49 跳过，文档测试通过。代码质量：搬来的部分保持 Codex 原样、来源写全；接入部分改动集中，单文件路径和原逻辑等价。

结果：返工 1 次

## 用户试用

2026-11-05，用户装好 `c9867df` 后在 canter-test-rust 试（会话 `s-1793868728754315000-13199-1`）。新版服务端 16:51 重拉目录，8 个 GPT 模型的 `apply_patch_tool_type` 都是 `freeform`。
- gpt-6.1-sol、Allow all：一个补丁改 README.md、Cargo.toml，新建 notes/patch-test.md，删 TODO.md，一次写好；日志里 GPT 输出 `custom_tool_call`，后续请求前缀接上。
- 切到 Always ask：改名加改 README.md 的补丁只弹一个 `Apply this patch?` 框，改名单独一行 `Move … → …`，批准后写好。
- 同一会话 `/model` 切到 opencode-go/deepseek-v4.1-flash，用 `edit` 改 README.md，历史里带着两次补丁调用也没被拒（交叉审查建议 7 实测没出问题）。
- `/rewind` 这次没手动试。
- 真调：GPT 11 次（其中 3 次是输入框建议的后台请求：1 次网络失败、2 次取消），DeepSeek 1 轮。
- 思考：两轮档位都发了 high；第一轮 5 次请求里 2 次有思考（20、14 token，各 1 段摘要），第二轮 4 次全是 0，是模型没想，不是界面没显示。
- 缓存：第一轮补丁结果交回 GPT 的那次请求命中 0，下一次只接到补丁之前；只这一次，先记着，后面多用再看日志。
- 用户：「都对着呢」。
