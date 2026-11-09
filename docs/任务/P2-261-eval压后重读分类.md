# 任务：eval 压后重读按原因分类，工具失败分项和改计划次数上表

2026-11-09，canter/main 交给 canter/dev-eval-reread（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.91）、拿不准、拿不准）
类型：功能变更
依据：P2-151「11-09 第一轮」里主控用一次性 jq 脚本做的压后重读分类，搬进 `canter-eval`，每轮自动出；不改 canter 本身。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（主控说明压后重读的分类是一次性脚本、不在 eval 工具里之后）：「把压后重读的分类加进 eval 工具」
- 11-09（主控说 `result.json` 里还有每种工具的失败次数和改计划的次数没上表）：「每种工具的失败次数和改计划的次数。这个如果有用的话，也加进去吧。」

## 先读

- `eval/README.md`「结果表」一节
- `eval/src/collect.rs`（已经在读 `<eval home>/sessions/<id>/events.jsonl`，见 `event_counts`）、`eval/src/report.rs`
- `crates/canter-core/src/stats/signals.rs` 的 `fill`：`rereadFiles` 的口径（只读，不改）
- `docs/任务/P2-151-eval.md`「11-09 第一轮」

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-reread`，分支 `eval-reread`（已从 main 建好）。只动 `eval/`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-reread`。

## 要做的

现在结果表的「压后重读」只有一个数：每次压缩到下次压缩之间，重读了几个压缩前读过的文件（按文件去重，各次压缩相加），来自 `canter stats` 的 `compactions[].signals.rereadFiles`。要把这些文件按原因分成三类，每轮跑完 `collect` 自动算出：

1. **改过**：这个文件在压缩前最后一次被读之后、这次重读之前，有过一次完成的 `fileChange`（改了这个路径）。重读是正常的。
2. **读回仍重读**：没改过，但这次压缩的 `contextCompaction` 项的 `restoredFiles` 里有它（压缩时 canter 已经自动读回来了），模型还是又读了一遍。
3. **没改没读回**：其余的。这类才算压缩丢了内容。

口径要和 `canter stats` 一致：被分类的文件集合就是那次压缩的 `rereadFiles`，三类加起来等于它的个数。主控当时用的 jq 脚本（只作口径参考，按 Rust 写，不要搬 jq）：

```jq
def rel($cwd): if startswith($cwd + "/") then .[($cwd|length)+1:] else ltrimstr("./") end;
(.[0].settings.cwd // "") as $cwd
| [ .[] | select(.type == "item.completed") | .seq as $s | .item
    | if .type == "fileRead" and .status == "completed" then {s: $s, k: "R", p: (.path | rel($cwd))}
      elif .type == "fileChange" and .status == "completed" then (.changes[]? | {s: $s, k: "C", p: (.path | rel($cwd))})
      elif .type == "contextCompaction" then {s: $s, k: "X", r: [(.restoredFiles // [])[] | .path | rel($cwd)]}
      else empty end ] as $t
| [ $t | to_entries[] | select(.value.k == "X") | .key ] as $xs
| [ range(0; $xs|length) as $i
    | $xs[$i] as $xi
    | (if $i + 1 < ($xs|length) then $xs[$i+1] else ($t|length) end) as $xn
    | $t[:$xi] as $before
    | $t[$xi].r as $restored
    | ([ $t[$xi+1:$xn][] | select(.k == "R") ] | group_by(.p) | map(min_by(.s))) as $firsts
    | $firsts[]
    | .p as $p | .s as $rs
    | ([ $before[] | select(.k == "R" and .p == $p) | .s ] | max) as $lastread
    | select($lastread != null)
    | ([ $t[] | select(.k == "C" and .p == $p and .s > $lastread and .s < $rs) ] | length) as $changes
    | { compaction: ($i + 1), path: $p,
        kind: (if $changes > 0 then "changed"
               elif ($restored | index($p)) != null then "restored-yet-reread"
               else "unchanged-not-restored" end) } ]
```

（这个脚本没跳过失败的压缩；canter 的 `fill` 只算成功、有摘要的。照 canter 的来。）

写到哪里：

- `result.json`：每次压缩带上三类的个数和对应的文件路径（相对 worktree），合计也有三类个数。
- 结果表：「压后重读」一列写成 `合计（改过 a · 读回 b · 没改 c）` 这样能一眼看懂的样子；表后「压缩」一节每次压缩那行后面也带上这三个数。具体字样你定，用中文、和表里其他列一致。
- README「结果表」一节说明三类各是什么，并写明：用 shell 命令（`sed -i`、`git checkout` 之类）改的文件看不出来，算进「没改」，所以「没改」只会偏多。

另外两样上表（`result.json` 里已经有，现在没进结果表）：

- 每种工具的失败次数：`metrics.toolFailuresByName`。「工具失败」一列写成合计加分项，比如 `14（edit 6 · shell 5 · read 3）`，按次数从多到少。
- 改计划的次数：`metrics.updatePlanCalls`，加一列「改计划」。
- README「结果表」的列说明跟着补。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控拿第一轮 `baseline-2026-11-09` 的 3 次会话重跑 `collect` 和 `report`，三类个数和 P2-151「11-09 第一轮」记的一致（`long-1` 改过 31 · 读回 0 · 没改 14；`long-2` 23 · 0 · 17；`long-3` 34 · 0 · 16），合计仍是 45、40、50；表里有工具失败分项和「改计划」一列。
- 验证只做这些：一条针对分类的测试（造一段事件，三类各至少一个，含一次失败的压缩不算进来）；`cargo nextest run -p canter-eval`；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `crates/` 下的任何代码，也不改 `canter stats` 的口径。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话；用真数据核对由主控做），不起任何 canter 服务端，不调真实模型接口。测试只用临时目录和造的事件。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事（比如路径对不上、口径和 canter 的 `fill` 有出入），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-reread` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

做了什么：`collect` 读事件时多扫一遍（和原来的护栏、被拦计数合成一次 `scan_events`），把每次压缩的 `rereadFiles` 按改过 / 读回 / 没改分类，每次压缩的三类路径写进 `CompactionRow.reread`，合计写进 `Metrics.reread`（都是相对 worktree 的路径，带 `serde(default)`，旧 `result.json` 照读）。`report` 的「压后重读」列写成 `5（改过 2 · 读回 1 · 没改 2）`，「压缩」一节每次后面带 ` · 压后重读 N（…）`；新增「改计划」列，「工具失败」列写出分项 `14（edit 6 · shell 5 · read 3）`。README「结果表」补了三类说明、shell 改动只看得到「没改」的提醒和两列口径。

验证了什么：`cargo nextest run -p canter-eval` 19 个全过（含新的一条分类测试：造的事件里改过、读回、没改各一个，另有在失败压缩之后才重读的文件，验证它不会被切出窗口、失败压缩本身不算进来）；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都干净。另外用临时 eval 目录里手写的 `result.json`（新格式和缺 `reread` 字段的旧格式）各跑了一次 `report`，看过表的样子：旧格式显示原来的整数，新格式显示分项；没碰 `~/.canter-eval`。

拿主意的地方：分类的文件集合直接用 `canter stats` 给的 `rereadFiles`（不再从事件重算），这样三类相加一定等于那一列；按 itemId 找这次压缩的 `restoredFiles`，按 `stats` 的口径（`overview.cwd`、和 `fill` 一样的路径规范化）对读和改的 seq。只有 `succeeded != false` 且有摘要的压缩算数、才切窗口，失败的和 `fill` 一样跳过（它的事件仍会被扫到，只是不设边界）。事件里找不到该文件的上一次读时放进「没改」，宁可多算也不丢。`result.json` 里存路径列表，个数用列表长度（不另存一份一数）。

没做的事：没用第一轮 `baseline-2026-11-09` 的真会话核对 31/0/14 等数（按任务留给主控）；没改 `crates/`，没动 `eval/results/`；没合并、没推送。

踩过的坑：一次编辑 `report.rs` 时 oldText 选错，误删了 `cost()` 开头三行，编译前看 diff 发现并补回；clippy 的 `redundant_guards` 要求把 `if matches!(...)` 写成结构体字段里的 or 模式；用 `rg -rn` 搜索时 `-r` 是替换而不是行号，输出被改得看不出原样，换回 `rg -n` 才看对。

## 主控审查

- 改动只在 `eval/`（`collect.rs`、`report.rs`、测试、README），`crates/` 没动。分类用 stats 给的 `rereadFiles` 名单、只算成功有摘要的压缩，和 canter 的 `fill` 同口径。
- 验收（主控提、用户 11-09「派吧」同意）：主控用分支版本对 `baseline-2026-11-09` 三次重跑 `collect`、`report`，三类为 31/0/14、23/0/17、34/0/16，合计 45、40、50，和 P2-151 记的一致；表里有工具失败分项和「改计划」列。合并后用 main 版重出了第一轮的表。
- 合并前全套：`cargo nextest run --all-targets` 2043 个全过，`cargo test --doc --workspace` 全过。
- 顺带发现（P2-257 留下，记进 HANDOFF）：「用时」到会话最后一条事件为止，会算进一轮结束后的建议请求和关闭时的状态，早收和晚收差十几二十秒；这次重出的表三次用时因此各多了 16～24 秒。

结果：一次过
