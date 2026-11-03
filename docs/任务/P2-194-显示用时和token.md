# 任务：第二期-25.4 - 界面：干活那一行醒目、显示 token，每轮结束显示用时和 token

2026-11-03，主控建。
2026-11-03，canter/main 交给 canter/dev-turn-stats（Claude Code，opus[1m] / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.36）、拿不准、拿不准；主控判：界面显示加协议里只加字段，不碰数据模型、并发、核心规则，算改行为、不交叉审查）
类型：功能变更
你是被委派的 agent：照本文件做，不要再开别的 agent。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依据：11-03 用户说看不到一轮跑了多久、用了多少 token，拿 Claude Code 干活那一行和 Codex 每轮结束那一行做样子。canter 干活那一行其实已经有计时，但整行太淡，用户没看出来。本件改终端里干活那一行的样子、加 token 数，并在每轮结束后留一行用时和 token。
依赖：P2-192（P2-192 合并后马上做，排在 P2-193 前面）
执行：派出

## 用户原话

- 11-03 用户发了 Claude Code 干活那一行的截图（`+ Frolicking… (4m 44s · ↓ 26.2k tokens · thinking with xhigh effort)`）和 Codex 一轮结束的截图（`Worked for 21m 45s • 15:07`）：「其实我想加这么两个功能，第一个是claude code的，主要是显示当前用了多少token，跑了多长时间。（现在我完全不知道这两个信息），还有第二张图是codex做完任务之后的显示 运行了多少时间，消耗了多少token我觉得是不是也可以显示。这两块你要设计一下。先弄成一个任务二期的」
- 主控说 canter 干活那一行已经有计时，用户发了 canter 截图（`⠋ Thinking · 24m 56s · Esc to interrupt`）：「但是现在有一个问题是现在的这个thinking太不明显了。我说的总的这个thinking」
- 主控问了三件，用户的选择：
  - token 显示哪个数：「干活时只显示输出，结束时全显示（推荐）」。
  - 子 agent 的 token 算不算进这一轮：「不算，只算这个会话自己的（推荐）」。
  - 先后：P2-192 合并后马上做。
- 干活那一行怎么改醒目：「动词上色加粗，数字正常色（推荐）」。

## 示意

主控 11-03 给用户看过、用户选定的样子：

```
干活时（状态行）：
 粗体彩色          正常色                      暗色
 ↓                 ↓                           ↓
 ⠋ Thinking…  ·  4m 44s · ↓ 26.2k tokens  ·  Esc to interrupt

一轮结束（留在对话里，每轮一行，灰色）：
 Worked for 21m 45s · ↑ 1.27M (98% cached) · ↓ 26.2k
```

## 先读

- 仓库根的 `CLAUDE.md`（项目规矩）。
- `docs/DESIGN.md` 里写状态行、一轮里各阶段排法（P2-188）、界面美化（P2-79、P2-187、P2-189）的段落。
- `crates/canter-tui/src/app.rs`：`activity`（干活那一行，约 5780 行）、`working_since`、`doing`；`crates/canter-tui/src/shimmer.rs`（动词的扫光）；`crates/canter-tui/src/theme.rs`。
- 数据：`crates/canter-protocol/src/types.rs` 的 `Turn`（`started_at`、`completed_at`、`duration_ms`、`usage`）、`TokenUsage`、`TokenUsageBreakdown`；`crates/canter-protocol/src/reducer.rs` 里 `SessionUsage` 和 `TurnCompleted` 怎么更新快照；`crates/canter-core/src/runner.rs` 约 870 行一轮的用量怎么累计。
- 现有终端测试：`crates/canter-tui` 里测状态行和一轮各阶段的那些，`crates/canter/tests/tui.rs`。

## 在哪里干活

- worktree：`../canter-worktrees/p2-194-turn-stats`，分支 `p2-194-turn-stats`（派出前由主控从 main 建好）。
- 编译目录：cargo 命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-194-turn-stats`。

## 要做的

### 1. 干活那一行醒目

- 旋转符号和动词（`Thinking`、`Running`、`Reading`、`Working` 等）用醒目的颜色、加粗，深浅两种背景下都要一眼看得到；参考 Claude Code 的橙色，颜色由你从主题里挑或在主题里加一个，不加新的配置项。扫光保留，改成在这个颜色上变亮。
- 用时和 token 数用正常字色（不淡化）。
- `Esc to interrupt`、`Tab to steer`、`ctrl+b to run in background`、`Ctrl+O to fold` 这些按键提示照现在的暗色。
- 正在跑的命令（`Running` 后面的命令）照现在。
- `NO_COLOR` 下：动词只加粗。
- 其它状态（`Reconnecting…`、`Interrupting…`、等批准、等回答）照现在。

### 2. 干活时显示 token

- 用时后面加这一轮到目前为止的输出 token：`· ↓ 26.2k tokens`。输出包括思考的 token。
- 数字每次模型请求做完更新一次（canter 的用量是一次请求做完才有，不要求像 Claude Code 那样连续涨）；这一轮还没有用量时不显示这一段。
- 只算这个会话自己的请求，不算子 agent，不算顺口问（`/btw`）。这一轮里的压缩请求照服务端现在给这一轮记的算。
- 窗口窄时，token 这一段先于用时被省掉。

### 3. 每轮结束留一行用时和 token

- 每一轮结束后（做完、被打断、失败都算），在这一轮末尾留一行灰色（和 `✻ Thought for 2s` 同一档灰）：`Worked for 21m 45s · ↑ 1.27M (98% cached) · ↓ 26.2k`。
  - `↑` 是这一轮所有请求的输入 token 合计，括号里是其中缓存命中的比例；`↓` 是输出 token 合计（含思考）。
  - 用时用这一轮从开始到结束的时间，格式和干活那一行一致。
  - 统计范围同第 2 节。
- 恢复会话、往上翻历史时，以前的轮也照样显示；老数据缺用量的只显示用时，缺用时的不显示这一行。
- 这一轮正在进行时不显示这一行（干活那一行已经在显示）。

### 4. 数据从服务端来

- 用时和 token 用服务端给的数据（`Turn` 的时间和用量、事件流），不在终端里自己估，以后的桌面和网页客户端能拿到同样的数。
- 干活中这一轮到目前的用量，终端现在可能拿不到（服务端记在这一轮上，但事件里给的是整个会话的）；怎么拿由你定，比如在快照归并里把用量加到进行中的那一轮，或者事件里带上这一轮的用量。可以加字段，不改已有字段的含义，老数据照常能读。

### 5. 文档

- DESIGN 里写状态行和一轮各阶段的地方补上这几条，写上用户原话；`crates/canter-tui/README.md` 有相关描述的照改。

## 怎么算做完

- 用户原话：「显示当前用了多少token，跑了多长时间」「做完任务之后的显示 运行了多少时间，消耗了多少token」「现在的这个thinking太不明显了」；选定的：「干活时只显示输出，结束时全显示」「不算，只算这个会话自己的」「动词上色加粗，数字正常色」。上面第 1～5 节做到，屏幕上的样子和示意一致。
- 验证只做这些：
  - 写针对的测试（照现有终端测试的写法，用假的事件和用量，不调真实模型），先确认它们在改之前失败：干活那一行动词是醒目颜色加粗、用时是正常字色、有用量后出现 `↓ N tokens`、`NO_COLOR` 下动词只加粗；一轮结束后有 `Worked for …` 那一行，数字和给的用量一致，子 agent 和 `/btw` 的用量不算进去；老数据缺用量只显示用时。
  - 跑 `cargo nextest run -p canter-tui -p canter-protocol`，改到别的包就加上那个包；`crates/canter/tests/tui.rs` 里有断言状态行或一轮样子的，`-p canter` 也跑。
  - 加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改发给模型的请求、系统提示词的拼接、压缩、缓存相关的逻辑、模型历史；不改服务端怎么计用量（只是把已有的数送到终端、显示出来）。
- 不把子 agent 的用量汇总进来。
- 不改用户自己配的状态栏命令（`statusLine`）收到的内容、`/export`、`canter stats`。
- 不加新的配置项。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）；缓存命中比例照通用的 `cached_input_tokens` 算。
- 不调真实模型接口。
- 界面文字一律英文；改动里终端字符串不能有中文。
- 实现、测试和辅助命令不用 Python、perl 或别的脚本语言，只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录、临时端口和临时 socket。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：停自己起的进程用记下的 PID。
- 不在 Cargo 里引用 ranch、paddock、cairn 的包，不从别的项目搬代码。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。包括：主题里挑不出深浅背景都看得清的醒目颜色；服务端给这一轮记的用量和第 2、3 节说的范围对不上（比如混进了子 agent 或 `/btw`）；要改协议里已有字段的含义。
- 跑 cargo 命令都带上面的 `CARGO_TARGET_DIR`，不要在 worktree 里生成 `target/`。
- 不合并到 main，不推送。只在 `p2-194-turn-stats` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
