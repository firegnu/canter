# 任务：eval 记忆题第二版（规矩换成仓库里看不出来的）

2026-11-09，canter/main 交给 canter/dev-eval-memory2（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：轻 / 交叉审查不要 / 影响面：改行为（路由：轻（置信 0.32）、不要、改行为）
类型：功能变更
依据：第一轮记忆题（`eval/results/memory-2026-11-09.md`）清空记忆的 3 次也全过，分不出记忆对完成任务有没有用；本轮加一道新题 `memory-2`，原来的 `memory` 题不动。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（主控讲完第一轮：记忆写了、也读了，对完成任务有没有用看不出来）：「那你打算怎么改才能测出来？」
- 主控提议换成两条仓库里看不出来、模型平时也不做的规矩，另建 `memory-2`，原题留着；用户：「可以，写任务单给我看」
- P2-259 时：「但是如果改了eval，上一个版本的eval是不是就没法使用了？」

## 背景（第一轮的数）

- 清空记忆那组，第 2～4 个会话每个都跑了 `git log`，看到前面 `tc(...)` 的提交标题和已有的 `docs/commands.md`，照着做就对了；按文件名 `git add` 模型本来就这么做。三条规矩都不用记也能守。
- 第一轮 24 个会话里：改了 `README.md` 的 15 个；跑过 `cargo doc` 的 0 个。所以下面两条规矩，不知道的会话大多守不住，仓库里也留不下能照抄的痕迹。

## 先读

- `eval/README.md`「多个会话的题（记忆）」一节
- `eval/tasks/memory.toml`（整份，新题照它写）
- `eval/tests/eval.rs` 里 `memory_task_has_four_sessions_and_the_rules_in_the_first`、`memory_checks_pass_and_fail_on_the_expected_worktree`，和它们用到的 `memory_repo`、`memory_commits`、`run_check`

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-memory2`，分支 `eval-memory2`（已从 main 建好）。只动 `eval/tasks/memory-2.toml`（新建）、`eval/tests/`、`eval/README.md`。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-memory2`。

## 要做的

新建 `eval/tasks/memory-2.toml`，`id = "memory-2"`，名字写明是记忆题第二版、规矩从仓库里看不出来。

1. 仓库、起点、`auto_compact_tokens`、`reasoning`、4 个会话各自做哪件 TODO 的那段提示词，和 `memory.toml` 一样。
2. 第 1 个会话末尾的长期规矩换成下面这段（照抄）：

   ```
   另外这个仓库有两条长期的规矩，以后在这里做的每件活都要守：
   1. README.md 由人维护，不要改它。
   2. 每次提交前跑一次 cargo doc --no-deps，要通过。
   ```

3. 判分只看第 2～4 个会话（第 1 个会话是交代规矩的地方）：
   - 照抄 `memory.toml` 的 `four-todos-checked`、`tests-and-lints`、`dependencies-unchanged`；
   - `readme-untouched`：第 2、3、4 个会话的提交都没改 `README.md`（用 `sessions/<n-1>/head` 到 `sessions/<n>/head`，同 `memory.toml` 的写法）；
   - `cargo-doc-per-session`：第 2、3、4 个会话的 `commands.txt` 里各有一条 `cargo doc` 命令（单独一条，或和别的命令用 `&&`、`;` 连着都算）。
   - 不要 `tc-commit-subjects`、`commands-doc-per-session`、`no-blanket-git-add`。
4. `eval/README.md` 的题目组列表加 `memory-2` 一行，一句话写清和 `memory` 的区别（第一轮清空记忆也全过：那三条规矩从 `git log` 和已有文件看得出来）。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控用 `memory-2` 跑一轮，保留、清空记忆各 3 次（DeepSeek flash），结果表用户看过觉得能用。
- 旧题照旧能用：`eval/tasks/memory.toml` 一个字没改。
- 验证只做这些：
  - 一条读题测试：`id`、仓库、起点、会话数，第 1 个会话有上面两条规矩的原文、后 3 个会话没有；
  - 一条判分测试，照 `memory_checks_pass_and_fail_on_the_expected_worktree` 的造法：造的 4 个会话提交和命令下五条检查都过；第 2～4 个会话里任一个改了 `README.md`，`readme-untouched` 不过；任一个会话的命令里没有 `cargo doc`，`cargo-doc-per-session` 不过；
  - `cargo nextest run -p canter-eval`、`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `eval/tasks/memory.toml`、`eval/src/`、`crates/` 下的任何代码。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话），不起任何 canter 服务端，不调真实模型接口。测试只用临时目录和造的提交、命令。
- 不加别的规矩、别的检查（用户这次只要这两条）。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如照抄的检查在新题里跑不通、两条规矩的判法有歧义），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-memory2` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
