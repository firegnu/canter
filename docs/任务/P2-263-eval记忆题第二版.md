# 任务：eval 记忆题第二版（规矩换成仓库里看不出来的）

2026-11-09，canter/main 交给 canter/dev-eval-memory2（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：轻 / 交叉审查不要 / 影响面：改行为（路由：轻（置信 0.32）、不要、改行为）
类型：功能变更
依据：第一轮记忆题（`eval/results/memory-2026-11-09.md`）清空记忆的 3 次也全过，分不出记忆对完成任务有没有用；本轮加一道新题 `memory-2`，原来的 `memory` 题不动。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（旁路 agent 提醒：第 1 个会话可能把规矩写进仓库的 AGENTS.md，清空记忆的会话一开局就能看到；主控核了第一轮 6 次都没动 AGENTS.md，补了 `rules-not-in-repo`）：「这个你考虑了吗？」

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
   - `rules-not-in-repo`：规矩没有写进 canter 开会话时会自动读的地方，后面的会话不靠记忆也能看到（这一项不过，说明这次跑露了底，不能拿来比记忆）：
     - worktree 里任何一层的 `AGENTS.md`、`CLAUDE.md` 和 `.agents/` 下的文件，和起点比都没变（包括没提交的改动和新建的文件）；
     - worktree 里相对起点新增的内容（包括没提交的、新建没跟踪的文件）里没有 `cargo doc` 字样；
     - eval 状态目录根下没有 `AGENTS.md`（canter 的全局说明；`$RUN_DIR` 是 `<eval 状态目录>/runs/<轮次>/<次>`）。
   - 不要 `tc-commit-subjects`、`commands-doc-per-session`、`no-blanket-git-add`。
4. `eval/README.md` 的题目组列表加 `memory-2` 一行，一句话写清和 `memory` 的区别（第一轮清空记忆也全过：那三条规矩从 `git log` 和已有文件看得出来）。

## 怎么算做完

- 验收（主控提议，待用户确认）：主控用 `memory-2` 跑一轮，保留、清空记忆各 3 次（DeepSeek flash），结果表用户看过觉得能用。
- 旧题照旧能用：`eval/tasks/memory.toml` 一个字没改。
- 验证只做这些：
  - 一条读题测试：`id`、仓库、起点、会话数，第 1 个会话有上面两条规矩的原文、后 3 个会话没有；
  - 一条判分测试，照 `memory_checks_pass_and_fail_on_the_expected_worktree` 的造法：造的 4 个会话提交和命令下六条检查都过；第 2～4 个会话里任一个改了 `README.md`，`readme-untouched` 不过；任一个会话的命令里没有 `cargo doc`，`cargo-doc-per-session` 不过；改了 `AGENTS.md`（提交了或只改没提交）、新建了一个写着 `cargo doc` 的没跟踪文件、eval 状态目录根下有 `AGENTS.md`，这三种各自让 `rules-not-in-repo` 不过；
  - `cargo nextest run -p canter-eval`、`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `eval/tasks/memory.toml`、`eval/src/`、`crates/` 下的任何代码。
- 不读、不改 `~/.canter`、`~/.canter-eval`（里面有 key 和真实会话），不起任何 canter 服务端，不调真实模型接口。测试只用临时目录和造的提交、命令。
- 不加上面没写的规矩和检查。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（比如照抄的检查在新题里跑不通、两条规矩的判法有歧义），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-memory2` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：新建 `eval/tasks/memory-2.toml`：仓库、起点、压缩线、思考档和 4 个会话的做题提示照 `memory.toml`，第 1 个会话末尾的长期规矩换成 README.md 由人维护、每次提交前跑 `cargo doc --no-deps`。判分照抄 `four-todos-checked`、`tests-and-lints`、`dependencies-unchanged`，另加 `readme-untouched`、`cargo-doc-per-session`、`rules-not-in-repo`；没有 `tc-commit-subjects`、`commands-doc-per-session`、`no-blanket-git-add`。`eval/README.md` 的题目组列表和目录说明各加一行。`eval/tests/eval.rs` 加读题和判分两条测试；共用的 `memory_repo` 造的起点里补了 `README.md` 和 `AGENTS.md`（新测试要用，`memory.toml` 的判分不碰它们）。
- 验证了什么：`cargo nextest run -p canter-eval` 36 条全过；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过。另外把 `rules-not-in-repo` 的命令抽出来，在 canter-test-rust 的本地克隆（检出 e9ee31c）上试过：改 `src/cli.rs`、`docs/commands.md`、`TODO.md` 并提交后照过；没提交的 `AGENTS.md` 改动、`.agents/skills/.../SKILL.md` 改动、新建 `CLAUDE.md`、新建写着 `cargo doc` 的没跟踪文件、在没提交的 `docs/commands.md` 里加一行 `cargo doc`、eval 状态目录根下放 `AGENTS.md`，各自都不过；`target/` 下被忽略的 `cargo doc` 字样不算。
- 拿主意的地方：
  - `rules-not-in-repo` 的「新增内容」看 diff 的新增行（`git diff "$BASE"` 里 `+` 开头的行）和新建没跟踪文件的全文，不拿改过的文件全文判，免得以后起点里本来就有的 `cargo doc` 算错；被 `.gitignore` 挡住的没跟踪文件不看内容（比如 `target/`），但文件名照样查 `AGENTS.md`、`CLAUDE.md`、`.agents/`。
  - `readme-untouched` 只认仓库根的 `README.md`（`git log --name-only` 里正好是 `README.md`），和 `commands-doc-per-session` 判 `docs/commands.md` 的写法一致。
  - `cargo-doc-per-session` 不认裸的 `cargo doc` 字样，要求它在命令开头或 `;`、`&&`、`|`、`(` 之后，免得 `git commit -m "cargo doc"` 这类蒙过去；`cargo docx` 不算。
  - `eval/README.md` 除了题目组那行，目录清单里也加了 `tasks/memory-2.toml`，不然目录说明不全。
- 没做的事：没跑真实模型，没起 canter 服务端，没读 `~/.canter`、`~/.canter-eval`；没改 `eval/tasks/memory.toml`、`eval/src/`、`crates/`；没合并、没推送。
- 踩过的坑：`$RUN_DIR` 是 `<eval 状态目录>/runs/<轮次>/<次>`，状态目录根在它上面三层，起先写成两层（`../../AGENTS.md`），eval 状态目录根下有 `AGENTS.md` 那种情况没拦住，判分测试抓出来了，改成 `../../../AGENTS.md`。

## 主控审查

- 改动只在 `eval/tasks/memory-2.toml`（新建）、`eval/tests/eval.rs`、`eval/README.md`；`memory.toml`、`eval/src/`、`crates/` 没动。六条检查和任务单一致，`rules-not-in-repo` 查了 worktree 各层 AGENTS.md / CLAUDE.md / `.agents/`（含没提交的、新文件）、新增内容里的 `cargo doc` 字样、eval 状态目录根的 AGENTS.md。
- 主控重跑 `cargo nextest run -p canter-eval` 36 个全过，clippy、fmt、`git diff --check` 过。
- 开发问 `.agents/`、`CLAUDE.md` 要不要单独造例子：和 AGENTS.md 同一条匹配，开发已手试，不加。
- 合并前全套：`cargo nextest run --all-targets` 2060 个全过（50 跳过），`cargo test --doc --workspace` 没有失败。
- 验收（用户确认的提议）：用 `memory-2` 跑一轮，保留、清空各 3 次，结果表用户看过觉得能用——还没跑。审查期间查到 canter 记忆目录的 bug（记忆文件标题后空一行，这条记忆不进目录、也不提示；第一轮 keep-3 就是这样），用户定先修再跑。

结果：一次过
