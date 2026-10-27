# 任务：压缩和 /handoff 的提示词

2026-10-27，canter/main 交给 canter/dev-compactprompt（Codex，gpt-6-astra / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.69）、拿不准、拿不准）
类型：功能变更
依据：P2-57 实现第 3 件：照方案原文换掉压缩和 `/handoff` 发给模型的说明，两者共用 9 个小节。压缩后的历史格式（`[Context summary]`）不变。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
依赖：P2-70
优先：高
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-27：「可以，照你推荐的补全 P2-57 吧」。
- 10-27（P2-57 方案 §9 的问题）：「全按推荐」。

## 先读

- `docs/DESIGN.md` 第 20 条里讲压缩和 `/handoff` 的一条。
- `docs/调研/P2-57-系统提示词方案.md` §0 最后两条（现在怎么做）、§6 全节（原文、两者区别）、§10 第 3 件。
- 代码：`crates/canter-core/src/compaction.rs`（`PROMPT`、给转写留位置的预算循环、压缩后的历史）、`crates/canter-core/src/btw.rs`（`/handoff`）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-71-compactprompt`，分支 `p2-71-compactprompt`（P2-70 合并后从 main 建好）。
- 只动 `crates/canter-core/src/compaction.rs`、`crates/canter-core/src/btw.rs` 和它们的测试，相关 README。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-71-compactprompt`。

## 要做的

照方案 §6 做：

- `SECTIONS`、`RULES` 两个常量放 `compaction.rs`，`btw.rs` 引用；压缩和 `/handoff` 各一个开头。原文照抄，一个字不改。
- 压缩：转写包进 `<transcript>`，有上一段摘要时包进 `<earlier_summary>`（没有时整块不出现），末尾加那句提醒；给转写留位置时把尾句和标签都算进去。不带工具定义、不开思考，照旧。
- `/handoff`：方向放末尾，没给时照旧用 `Continue the current task.`；带工具定义但不执行，照旧。
- 压缩后的历史（保留的最近用户消息 + `[Context summary]\n<摘要>`）不变。

## 怎么算做完

- （用户 10-27「全按推荐」）照 P2-57 方案 §6 换上了压缩和 `/handoff` 的说明。
- 验证只做这些：
  - 针对本件写测试，先写、确认在现在的代码上失败：用假模型记下发出去的文字，查压缩请求里 9 个小节、`<transcript>`、`<earlier_summary>`（有和没有两种）、尾句都在；转写很长、要分段时，每段请求都不超预算；`/handoff` 请求开头要求 `## Task`、末尾是方向（给了和没给两种）。
  - 现有压缩和 `/handoff` 测试只因说明文字变了而改期望的，在完成记录里说明。
  - 做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的编译目录）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改压缩后的历史格式和 `[Context summary]` 标签；不改压缩的触发条件和分段规则（除了预算算上新加的文字）。
- 不改 `/btw` 的说明；不改系统提示词（P2-69、P2-70）；不改方案原文的措辞。
- 不加依赖，不升级已有依赖。
- 不读用户真实的 `~/.canter` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，不碰用户正在跑的服务端（别读、别停它的 pid）。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括原文和代码对不上、要动范围以外的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-71-compactprompt` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：按 P2-57 方案 §6 原文替换两种提示词，`compaction.rs` 保存共用的 `SECTIONS`、`RULES`，`btw.rs` 引用；各有自己的开头。压缩增加转写标签、可选的前段摘要标签和尾句，初始预算、分段适配及 ContextLength 减半重试都带上完整包装。`/handoff` 要求先写 Task，方向仍在末尾，未给时仍用 `Continue the current task.`。相关 README 已补充。
- 验证了什么：新增两条假模型请求测试，直接读取方案 §6 的代码块逐字核对。压缩测试在旧实现上因缺少转写开标签失败，覆盖第一段无 earlier_summary、后续段有 earlier_summary、九个小节、尾句和长转写每段预算；handoff 测试在旧实现上因说明全文不符失败，覆盖指定和默认方向。各自实现后均通过。全套最终 `cargo nextest run --all-targets` 608/608 通过，`cargo test --doc --workspace` 1/1 通过，`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 均通过；命令均使用指定独立编译目录，测试使用临时 HOME、CANTER_HOME，均前台等待结束。
- 现有测试的文字期望：`canter-core/tests/review.rs` 的超限摘要重试测试改为从标签中取转写、按标签检查前段摘要；`canter-core/tests/retract.rs` 的自动/手动压缩撤回测试更新压缩开头匹配；`canter-server/src/http_tests.rs` 的 handoff 假模型识别和说明检查更新开头及小节文字。只因提示词更换调整期望，原有历史、撤回、工具不执行和计费断言保留。
- 拿主意的地方：没有需要主控决定的事项。测试使用方案原文作独立期望，避免另抄一份提示词；未读或搬用别家源码。
- 没做的事：未改压缩历史格式、触发条件和分段规则（预算包含新增文字除外），未改 `/btw`、系统提示词、依赖或方案原文。真实模型调用 0 次，未读用户真实配置、未碰现用服务端，未安装、未合并或推送。
- 踩过的坑：第一条测试命令连带的 `rm -rf` 临时目录清理被自动审批拒绝，命令未启动；去掉清理后正常运行，最后用 `rmdir` 清掉本次创建的九个空临时 HOME。handoff 原文的 `"## Task"` 会提前结束单个井号的 Rust 原始字符串，改用三个井号作定界符。第一次全套在撤回测试的旧文案断言失败，更新该文字期望后重跑全套通过；没有删测试、放宽断言或缩短超时。
