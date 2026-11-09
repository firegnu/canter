# 任务：eval 全局记忆题（会话可以换仓库做）

2026-11-09，canter/main 交给 canter/dev-eval-global（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.64）、拿不准、拿不准；推翻：交叉审查和影响面主控定。`clean` 按状态里记的路径强删 worktree，一道题有几个仓库以后删错会删到别的仓库；P2-259 同类改动交叉审查查出过真问题）
类型：功能变更
依据：`memory-2` 只测到项目记忆（规矩是一个仓库的）；本轮让一道题的会话能换仓库做，加一道全局记忆题。不改 canter 本身，老题照旧。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（`memory-2` 验收后）：「等一下，我想测试一下全局记忆。这个怎么设计/」
- 主控提议：第 1 个会话在 canter-test-rust 交代一条对所有项目都适用的偏好，第 2～4 个会话换到 canter-test-ts 做活，看守不守；要改 eval 代码让会话能换仓库。用户：「等一下，我理解这个测试还是在eval上叠加吧？」主控：老题、老结果都不动。用户：「可以，写任务单给我看」
- P2-259 时：「但是如果改了eval，上一个版本的eval是不是就没法使用了？」

## 背景

- 全局记忆在 eval 状态目录的 `memory/`，项目记忆在 `projects/<仓库键>/memory/`。换到别的仓库，只看得到全局记忆和那个仓库自己的项目记忆。
- 前两轮记忆题 48 个会话里：跑过 `git diff --check` 的 0 个；没有一个提交说明带 `Assisted-by:`。

## 先读

- `eval/README.md`「多个会话的题（记忆）」一节和「题目文件」相关的部分
- `eval/tasks/memory-2.toml`（新题照它写）、`eval/tasks/long.toml`（canter-test-ts 的起点和 `setup`）
- `eval/src/task.rs`、`eval/src/paths.rs`（`State`）、`eval/src/prepare.rs`（含 `clean`）、`eval/src/steer.rs`、`eval/src/finish.rs`、`eval/src/grade.rs`、`eval/src/collect.rs`、`eval/src/memory.rs`

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-global`，分支 `eval-global`（已从 main 建好）。只动 `eval/`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-global`。

## 要做的

### 1. eval 工具：一道题的会话可以在不同仓库里做

- 题目文件里，会话可以写在哪个仓库做；不写就是题目的主仓库（现在的 `repo`）。主仓库以外的仓库，它的起点和准备命令（同 `setup`）在题目文件里另写一处。字段名和格式你定，写进 README。
- 同一个仓库的会话共用一个 worktree，接着前面的做；每个仓库各一个 worktree。主仓库的 worktree 路径和现在一样，别的仓库的路径你定，写进 README。
- `prepare`、`trust`、`steer`（找会话）、`finish`（记 HEAD 和命令）、`grade`、`collect`、`clean` 都按会话或检查所在的仓库来；`prepare` 打印的下一步命令里 `--cwd` 是这个会话的 worktree。
- 判分检查可以写在哪个仓库的 worktree 里跑；不写就是主仓库。检查环境里的 `REPO`、`WORKTREE`、`BASE` 是这个检查所在仓库的；`RUN_DIR` 不变。
- `clean` 删掉这次跑建的全部 worktree，只删状态里记着的、这次建的。
- 老题（不写这些新字段）和老的 `state.json`，行为和现在一样。

### 2. 新题 `eval/tasks/memory-global.toml`

- `id = "memory-global"`，名字写明是全局记忆题。主仓库 canter-test-rust，起点、`auto_compact_tokens`、`reasoning` 同 `memory-2`。
- 第 1 个会话在 canter-test-rust：提示同 `memory-2` 第 1 个会话做「Add an edit command」那段，末尾的偏好换成下面这段（照抄）：

  ```
  另外说两个我个人的习惯，以后我所有的项目都这样，不只是这个仓库：
  1. 每个提交说明的最后一行写 Assisted-by: canter。
  2. 每次提交前先跑一次 git diff --check，有问题先改掉。
  ```

- 第 2～4 个会话在 canter-test-ts（起点、准备命令同 `long.toml`），依次做 TODO.md 里的「Fix future-date handling」「Add habit archiving」「Implement CSV export」。提示照 `memory-2` 的写法，换成这个仓库的命令：`npm test 和 npx tsc --noEmit 都通过后，在 TODO.md 勾掉它，再单独提交一次`，其余句子（不要加依赖、中途不要停、不要开子 agent）照抄。
- 判分只看第 2～4 个会话，检查都在 canter-test-ts 的 worktree 里跑，除非下面另说：
  - `three-todos-checked`：这 3 件在 TODO.md 里勾了；
  - `tests-and-types`：`npm test`、`npx tsc --noEmit` 都通过；
  - `dependencies-unchanged`：`package.json` 的依赖和起点一样；
  - `assisted-by-trailer`：第 2～4 个会话的每个提交，说明最后一行是 `Assisted-by: canter`；
  - `diff-check-per-session`：第 2～4 个会话的命令里各有一条 `git diff --check`（判法同 `memory-2` 的 `cargo-doc-per-session`）；
  - `preference-in-global-memory`：第 1 个会话结束时全局记忆里至少有一个文件（看 `sessions/1/memory.json`）；
  - `rules-not-in-repo`：照 `memory-2` 的同名检查，查 canter-test-ts 的 worktree（AGENTS.md、CLAUDE.md、`.agents/`，新增内容里没有 `Assisted-by` 和 `git diff --check` 字样）和 eval 状态目录根的 `AGENTS.md`。
- README 题目组列表加一行。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控用 `memory-global` 跑一轮，保留、清空记忆各 3 次（DeepSeek flash），结果表用户看过觉得能用。
- 老题照旧能用：`long`、`memory`、`memory-2` 三个题目文件没改；主控用合并后的版本重出 `baseline-2026-11-09`、`memory-2026-11-09`、`memory2-2026-11-09` 三张表，和提交的一字不差。
- 验证只做这些：
  - 多仓库：造一道两个仓库的题，`prepare` 建两个 worktree、各自跑了准备命令；第 2 个仓库的会话 `finish` 记的是那个 worktree 的 HEAD；检查在指定仓库的 worktree 里跑，`REPO`、`WORKTREE`、`BASE` 是那个仓库的；`clean` 两个都删掉，状态里没记的目录不动；
  - 老题：不写新字段的题和老的 `state.json`，现有测试照过；
  - 新题：一条读题测试（会话数、各会话的仓库、第 1 个会话有偏好原文、后 3 个没有），一条判分测试照 `memory_2_checks_pass_and_fail_on_the_expected_worktree` 的造法，每条新检查各有一个过、一个不过的例子；
  - `cargo nextest run -p canter-eval`、`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `eval/tasks/long.toml`、`memory.toml`、`memory-2.toml`，不改 `eval/results/` 下已有的表，不改 `crates/` 下的任何代码。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话），不起任何 canter 服务端，不调真实模型接口。测试只用临时目录和造的仓库。
- 不加上面没写的检查、字段和命令。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如老的 `state.json` 怎么兼容、两个仓库的会话交替时 worktree 怎么接），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-global` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
