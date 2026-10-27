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
