# 任务：压缩时显示 Compacting

2026-10-27，canter/main 交给 canter/dev-compacting（Claude Code，opus[1m] / high）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.83）、拿不准、拿不准；主控判不碰要害、改行为：要认出压缩没成功）
类型：功能变更
依据：10-26 用户问自动压缩在界面上有什么表现。现在压缩一开始对话里就出现 `── Context compacted ──`（还在压也这么写），状态行只写通用的 `Working`；压缩没成功时对话里照样写 `Context compacted`。照 Claude Code、Codex：压缩期间明确写 Compacting，压完再说压好了，没压成功说没成功。自动压缩和手动 `/compact` 一样。只改终端界面。
依赖：P2-59（两件都改 `canter-tui/src/app.rs`，排在它后面）
执行：派出
优先：高

## 用户原话

- 10-26：「现在自动压缩有什么ui上面的表现吗？我记得claude code或者codex是一个compacting...」；主控说现在只有一行 `── Context compacted ──` 和状态行 `Working`，建议状态行写 `Compacting context`、对话里压缩中写 `Compacting context…`、压完写 `Context compacted`。用户：「记成 P2-62，赶在这次装版之前做」。
- 10-26：用户：「但是自动压缩之后是不是会接着干活？」（会，自动压缩压完同一轮接着发请求；手动 `/compact` 是单独一轮。）「我希望的是compacting...也是流光的效果」；主控问对话里那行要不要也扫，用户：「只扫状态行」。

## 样子

```
  ── Compacting context… ──                         ← 压缩中（对话里）
 ⠋ Compacting context · 3s · Esc to interrupt        ← 压缩中（状态行，P2-56 的流光扫在这个词上）

  ── Context compacted ──                            ← 压完
  ── Compaction did not finish; the original context is kept ──   ← 没压成功
```

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `crates/canter-core/src/compaction.rs`（压缩什么时候发 `item.started` / `item.completed`、没成功时 `summary` 写什么）、`runner.rs`（自动压缩在哪两处触发）。只读，不改。
- `crates/canter-tui/src/cells.rs` 里 `Item::ContextCompaction` 怎么画；`app.rs` 的 `doing`、`activity`（状态行）；`shimmer.rs`（P2-56 状态行流光扫哪个词）。
- `crates/canter-tui/README.md` 讲状态行和对话里各种行的地方。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-62-compacting`，分支 `p2-62-compacting`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-62-compacting`。
- 只动 `crates/canter-tui/`（含测试和 README）和本任务文件末尾的完成记录。
- 没有同时在跑的开发任务。P2-59 已合并：撤回时正在进行的自动压缩被取消、不发 `item.completed`，但整轮随 `turn.retracted` 从快照里拿掉，对话里不会留下压缩那行，不用另做。

## 要做的

- **状态行**：这一轮正在压缩（自动的、手动 `/compact` 的都算）时，转圈后面写 `Compacting context`，后面照旧用时和 `Esc to interrupt`；P2-56 的流光扫整个 `Compacting context`，和扫 `Thinking` 同一套效果（用户 10-26：「我希望的是compacting...也是流光的效果」「只扫状态行」）。对话里那行不扫。自动压缩压完同一轮接着干活，状态行回到 `Thinking` 这类词。
- **对话里那一行**：压缩中写 `── Compacting context… ──`；压完写 `── Context compacted ──`；没压成功写 `── Compaction did not finish; the original context is kept ──`，不再写 `Context compacted`。被 Esc 打断的照没压成功处理。
- 「没压成功」现在核心只在 `summary` 里写一句固定的话，终端怎么可靠地认出来由你看；要改协议或核心才认得出时，停下来问。
- README 跟着改。

## 怎么算做完

- （用户 10-26）压缩期间界面上明确显示在压缩（状态行 `Compacting context` 带流光、对话里 `Compacting context…` 不带），压完显示 `Context compacted`；没压成功不写压好了。
- 验证只做这些：针对验收写测试，先写、确认在现在的代码上失败（TestBackend：压缩中、压完、没压成功三种的对话行和状态行）；现有画面测试和 `tests/expected/replay.txt` 有变化的逐屏看过、说明；做完跑一次 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带编译目录）。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改压缩什么时候触发、怎么压；不改核心和协议（要改先停下问）。
- 不加依赖，不升级已有依赖。
- 不读用户真实的 `~/.canter` 和配置；测试用临时目录和隔离的 HOME、`CANTER_HOME`，不碰用户正在跑的服务端（别读、别停它的 pid）。不调真实模型。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事（包括要动范围以外的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在自己的分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- **做了什么**：`cells.rs` 里压缩那行按状态写三种：进行中 `── Compacting context… ──`，压完 `── Context compacted ──`，没压成功 `── Compaction did not finish; the original context is kept ──`；`app.rs` 的 `doing` 给进行中的压缩项返回 `Compacting context`，状态行照旧走 P2-56 的流光（整个两个词扫，对话里那行不扫）。自动压缩压完后下一项开始，状态行自然换回 `Thinking` 这些。README 的状态行、新加的「压缩」条目和流光表跟着改。
- **怎么认出没压成功**：不改核心和协议。核心没压成功（出错、被 Esc 打断）时在 `item.completed` 的 `summary` 里写固定一句 `Compaction did not finish; the original context is kept.`，终端按这句原样比对；另外一轮已经结束、这项却一直没收到 `item.completed`（`summary` 还是开始时的空串）也算没压成功。压成功的摘要核心保证不为空（空的会报错走没压成功那条）。风险：核心以后改了这句话的措辞，终端会把失败显示成压好了；终端这边在 `cells.rs` 的常量上写了出处。
- **验证**：新加 `crates/canter-tui/tests/compaction.rs` 三个 TestBackend 测试（压缩中的对话行、状态行和流光只在状态行；压完后对话行、自动压缩接着干活状态行回到 `Thinking`、`/compact` 单独一轮；没压成功的三种：失败、Esc 打断、没收到完成就结束）。先在原代码上跑：压缩中、没压成功两个失败（显示 `Context compacted`、状态行 `Working`），压完那个本来就过（行为没变，留作回归）。改完三个都过。现有画面测试和 `tests/expected/replay.txt` 都没有变化。`cargo nextest run --all-targets` 559 个全过，`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 都过（都带编译目录）。
- **拿主意的地方**：没压成功的那行沿用原来的不折行写法（`Row::text_from`），窗口窄于 63 列时行尾被截掉，和原来那行一样不折；空摘要也算没压成功（见上）。
- **没做的事**：没改核心、协议和压缩触发；`crates/canter/tests/tui.rs` 里几处 `wait_for("Context compacted")` 都是压成功的场景，不用改。
- **踩过的坑**：没有。

## 主控审查

- 看了 diff：只动了 `crates/canter-tui/`。状态行在压缩项上写 `Compacting context`（流光照 P2-56 扫整个词），对话行按项目有没有完成、摘要是不是核心那句固定的失败话分三种写法。没改核心和协议。
- 它拿的主意都同意：空摘要也算没压成功；失败那行不折行，窄窗口截掉行尾。
- 已知限制（记 HANDOFF）：终端按核心那句失败话原样比对，核心改了措辞，失败会显示成压好了。要彻底，得给压缩项加状态字段（协议改动），这次不做。
- 重跑全套：559 个全过；文档测试、clippy、fmt、`git diff --check` 都过。
- 结果：一次过
