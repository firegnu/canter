# 任务：eval 加上下文指标（峰值、平均、组成）

2026-11-09，canter/main 交给 canter/dev-eval-context（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.96）、拿不准、拿不准）
类型：功能变更
依据：P2-151「用户点名要的指标」里「长程任务里上下文窗口的变化」还没进结果表；本轮只加峰值、平均、组成三样，不改 canter 本身。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（主控列了现有指标、建议补上下文三样）：「可以，写任务单给我看」
- P2-151 用户点名要的指标之一：「长程任务里上下文窗口的变化」

## 先读

- `eval/README.md`「结果表」一节
- `eval/src/collect.rs`（`metrics` 已经在用 `StatsResponse`）、`eval/src/report.rs`
- `crates/canter-protocol/src/stats.rs` 里请求那一项：`purpose`、`usage`、`context`（`systemTokens`、`toolDefinitionTokens`、`conversationTokens`、`toolResultTokens`、`estimated`）。只读，不改。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-context`，分支 `eval-context`（已从 main 建好）。只动 `eval/`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-context`。

## 要做的

`collect` 从 `canter stats --json` 的 `requests` 里，只取 `purpose` 是 `conversation` 的请求（压缩、建议请求不算），算三样，写进 `result.json` 的 `metrics`，并上结果表：

1. **上下文峰值**：这些请求里 `usage.inputTokens` 的最大值。
2. **平均上下文**：这些请求 `usage.inputTokens` 的平均值。没有 `usage` 的请求不算进去。
3. **组成**：这些请求 `context` 四项各自相加，算各占总和的百分比（系统提示词、工具定义、对话、工具结果）。

峰值和平均用服务商报的真实输入 token；组成用 canter 估算的数（`estimated: true`，估算比真实输入多三成左右），只看比例，不当绝对数。

上表：

- 结果表加两列「上下文峰值」「平均上下文」，写法和现有 token 列一致（比如 `49.7k`）。
- 组成放一列「上下文组成（估算）」，比如 `系统 17% · 工具定义 13% · 对话 45% · 工具结果 26%`；字样你定，用中文、和表里其他列一致。
- README「结果表」的列说明跟着补，写明峰值和平均取真实输入 token、组成是估算的比例、只算对话请求。
- 旧的 `result.json` 没有这几项，`report` 照样能出表（显示 `—`）。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控拿第一轮 `baseline-2026-11-09` 的 3 次重跑 `collect` 和 `report`，数和主控事先用 jq 从 `canter stats --json` 算的一致：
  - `long-1`：峰值 49710、平均 34148，组成 系统 17% · 工具定义 13% · 对话 45% · 工具结果 26%（209 个对话请求）
  - `long-2`：峰值 48047、平均 33379，组成 17% · 13% · 49% · 21%（181 个）
  - `long-3`：峰值 48803、平均 35523，组成 16% · 12% · 51% · 21%（257 个）
  - （平均四舍五入到整数，百分比四舍五入到整数；表里按 k 显示时以 `result.json` 里的整数为准。）
- 验证只做这些：一条针对这三样的测试（造一份 stats JSON：含压缩和建议请求、一个没有 `usage` 的对话请求，确认它们都不算进来）；`cargo nextest run -p canter-eval`；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `crates/` 下的任何代码，也不改 `canter stats` 的口径或估算方法。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话；用真数据核对由主控做），不起任何 canter 服务端，不调真实模型接口。测试只用临时目录和造的 stats JSON。
- 不加首字等待、重试次数这类别的指标（用户这次只要上下文三样）。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事（比如字段对不上、`purpose` 的取值和上面写的不一样），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-context` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：`collect` 从 `requests` 里只取 `purpose` 是 `conversation` 且带 `usage` 的请求；峰值取 `usage.inputTokens` 最大、平均取平均（四舍五入），组成把这些请求的 `context` 四项各自加总后算百分比，写进 `metrics` 的 `contextPeakTokens`、`contextAverageTokens`、`contextShare`（`systemPercent`、`toolDefinitionPercent`、`conversationPercent`、`toolResultPercent`）。`report` 在「压缩」后加三列，缺字段显示 `—`；README「结果表」补了三列的说明和计算口径。
- 验证了什么：新增 `context_metrics_use_conversation_requests_that_reported_usage`，造的 stats JSON 里有压缩、建议、没有 `usage` 的对话请求（带大块假 context）和一条没有 `context` 的对话请求，确认前三种都不算，平均四舍五入（4001/3 → 1334），组成 30/10/50/10；`cargo nextest run -p canter-eval` 20 个全过；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过。
- 拿主意的地方：组成也只算带 `usage` 的对话请求，和峰值、平均是同一个集合（按任务里测试要求「没有 `usage` 的对话请求……都不算进来」）；百分比各自四舍五入，四项加起来可能不是 100（比如 101），不做归一；有 `usage` 但没有 `context` 的请求只进峰值和平均。
- 没做的事：没有拿 `baseline-2026-11-09` 的真数据核对（要读 `~/.canter-eval`，任务里说不读，由主控做）；没给旧 `result.json` 缺字段专门加测试（任务说只加一条测试），向后兼容靠新字段的 `#[serde(default)]`，现有 report 测试两头都带字段，没覆盖缺字段那一头。
- 踩过的坑：插入测试时没注意原文件里 `counts_malformed_lines` 前面已有 `#[test]`，多了一个，`clippy -D warnings` 报 duplicated attribute，删掉后过。别的没有。
