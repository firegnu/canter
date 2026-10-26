# 任务：压缩时显示 Compacting

类型：功能变更
依据：10-26 用户问自动压缩在界面上有什么表现。现在压缩一开始对话里就出现 `── Context compacted ──`（还在压也这么写），状态行只写通用的 `Working`；压缩没成功时对话里照样写 `Context compacted`。照 Claude Code、Codex：压缩期间明确写 Compacting，压完再说压好了，没压成功说没成功。自动压缩和手动 `/compact` 一样。只改终端界面。
依赖：P2-59（两件都改 `canter-tui/src/app.rs`，排在它后面）
执行：派出
待用户：P2-59 合并后主控补上交给谁、路由和 worktree，用户看过说派才派。P2-48、P2-59、P2-60、P2-61、P2-62 全部做完后一起装版
优先：高

## 用户原话

- 10-26：「现在自动压缩有什么ui上面的表现吗？我记得claude code或者codex是一个compacting...」；主控说现在只有一行 `── Context compacted ──` 和状态行 `Working`，建议状态行写 `Compacting context`、对话里压缩中写 `Compacting context…`、压完写 `Context compacted`。用户：「记成 P2-62，赶在这次装版之前做」。

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

（P2-59 合并后补。）

## 要做的

- **状态行**：这一轮正在压缩（自动的、手动 `/compact` 的都算）时，转圈后面写 `Compacting context`，后面照旧用时和 `Esc to interrupt`；P2-56 的流光扫在 `Compacting` 这个词上（或整个 `Compacting context`，照 P2-56 扫动词的同一规则）。压完回到别的词。
- **对话里那一行**：压缩中写 `── Compacting context… ──`；压完写 `── Context compacted ──`；没压成功写 `── Compaction did not finish; the original context is kept ──`，不再写 `Context compacted`。被 Esc 打断的照没压成功处理。
- 「没压成功」现在核心只在 `summary` 里写一句固定的话，终端怎么可靠地认出来由你看；要改协议或核心才认得出时，停下来问。
- README 跟着改。

## 怎么算做完

- （用户 10-26）压缩期间界面上明确显示在压缩（状态行 `Compacting context`、对话里 `Compacting context…`），压完显示 `Context compacted`；没压成功不写压好了。
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
