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

## 完成记录

做了什么：

- `eval/src/task.rs`：题目文件加 `[[repos]]`（`name`、`base`、`setup`），`[[sessions]]` 和 `[[checks]]` 加可选的 `repo`（不写是主仓库的 `repo`）；加载时校验仓库名不重复、不重复 `repo`、名字能放进路径、`repo` 指向的仓库确实在 `[[repos]]` 里。
- `eval/src/paths.rs`：`state.json` 加 `repos`（`name`、`repo`、`baseCommit`、`worktree`，没有时按空处理）；`State::all_repos` / `repo_by_name` / `session_repo`。主仓库还是原来那几个字段。别的仓库的 worktree 是主仓库那个的兄弟目录 `<run>-<仓库名>`（`Local::other_worktree_path`）。
- `prepare`：第 1 个会话时给每个仓库建 worktree、各跑自己的准备命令（环境是那个仓库的 `REPO`、`WORKTREE`、`BASE`）；`prepare` 打印的 `--cwd` 是这个会话所在仓库的 worktree，多个仓库时多一行 `session repo:`；`finish` 记会话所在仓库的 HEAD；`steer` 在那个 worktree 里找会话、按那个仓库的起点数终点提交；`grade` 按检查的 `repo` 选 worktree 和环境变量；`clean` 删 `state.json` 里记的全部 worktree（先把所有路径都验一遍在不在 worktree 根下，再动手）；`trust` 把这次的 worktree 全部设为信任。
- 新题 `eval/tasks/memory-global.toml`：主仓库 canter-test-rust，`[[repos]]` 里是 canter-test-ts；第 1 个会话交代两个对所有项目适用的习惯，第 2～4 个会话换仓库做三件 TODO；7 条检查照任务文件写。README 加了题目组列表、worktree 位置、「多个会话」一节、`[[repos]]`/`repo` 的写法和环境变量。

验证了什么：

- 新测试：`memory_global_task_has_four_sessions_in_two_repositories`（会话数、各会话的仓库、第 1 个会话有偏好原文、后 3 个没有、检查 id 和所在仓库）；`memory_global_checks_pass_and_fail_on_the_expected_worktree`（7 条检查各有一过一个不过的例子）；`prepare_makes_a_worktree_for_every_repository_and_finish_uses_the_session_one`（两个 worktree、准备命令只在第二个仓库跑、`summary` 的 `--cwd` 对、`finish` 记各自仓库的 HEAD、检查在各自仓库跑且 `REPO`/`WORKTREE`/`BASE` 是那个仓库的、`clean` 两个都删、状态里没记的目录不动）；`trust_trusts_every_worktree_of_the_run`；`an_old_state_file_without_repositories_still_loads`（老的 `state.json`）。
- `cargo nextest run -p canter-eval`：41 个全过（老题相关的测试一个没改）。`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都干净。

拿主意的地方：

- 字段名和 worktree 路径：别的仓库的 worktree 放 `<worktree 根>/<轮次>/<次>-<仓库名>`，主仓库不变。
- `clean` 先把所有 worktree 都验一遍（都在根下、根本身不动）再删，不边验边删，免得一个状态坏了删掉一半。
- 第 2～4 个会话换到了另一个仓库，它们的 `head` 不在同一个仓库里，所以 `assisted-by-trailer` 里第 2 个会话的提交范围从那个仓库的 `$BASE` 算起（README 也写了一句，免得别人照 `head1..head2` 写）；`preference-in-global-memory` 不碰 worktree，留在主仓库跑。
- 老 `state.json` 没有 `repos` 就当只有主仓库（那时也不可能有别的仓库），新测试把它钉住。
- `assisted-by-trailer` 要求每个会话至少有一个提交，免得范围空着也算过。
- 没给单会话题（`prompt`）加 `repo`：能换仓库的是 `[[sessions]]`，老题不写新字段就和以前一样。

没做的事：

- 没在真的 canter-test-rust / canter-test-ts 上跑（验收由主控跑，任务也没要求）；没改 `long`、`memory`、`memory-2` 三个题目文件和 `eval/results/`，没改 `crates/`。

踩过的坑：

- 判分检查里 `test "$WORKTREE" = "$(pwd)"` 在 macOS 上会挂：`sh` 的 `pwd` 给的是物理路径（`/private/tmp/...`），`WORKTREE` 是 `/tmp/...`。多仓库测试里的检查改成两边都取 `pwd -P` 再比。
- 给测试里这些 `State { … }` 加字段时手滑删了两行 `let temp = TempDir::new().unwrap();`，下一轮编译就暴露了，已改回；现在 `git diff` 里除了一行 `use` 没有删改。

## 返工 1

交叉审查（P2-264-交叉审查）提了两条必须改，主控决定第 1 条改、第 2 条记成建议不改。

改了什么：

- 附加仓库的 worktree 路径从 `<轮次>/<次>-<仓库名>` 换成 `<轮次>/@repos/<次>/<仓库名>`：`@` 不在 `check_label` 允许的字符里（只有字母、数字、`-_.+`），所以这个目录段永远不可能是一个合法 run 的主 worktree（主路径还是 `<轮次>/<次>`），两次跑的附加路径也按 run 各自分层，两者都不会重叠。主仓库路径不变。
- `clean` 在原来的「在 worktree 根下」之外再加一条所有权检查（`paths::owned_worktree`）：记着的路径必须是这次跑的 `<轮次>/<次>` 或 `<轮次>/@repos/<次>/<仓库名>`，否则报 `refusing`、一个都不删。旧分支那种旧布局的 state 会被拒绝而不是拿来删别人的目录（那个分支没合并，没有真实 state 受影响）。
- `prepare` 给每个 worktree（包括附加仓库的）建父目录（`git worktree add` 不会建多级父目录）。README 的 worktree 位置、`clean` 的说法跟着改。

验证了什么：

- 新增回归检查 `clean_of_one_run_never_removes_another_runs_worktree`：prepare `a`（两个仓库）→ `clean --run a` → prepare `a-canter-test-ts`（主仓库就是 canter-test-ts，名字恰好拼出旧布局的路径）→ 再 `clean --run a`，后者路径是 `<轮次>/a-canter-test-ts`，旧布局会和 `a` 的附加路径重合；现在 B 的 worktree 和里面的未提交文件都在。再把一份 state 的 `repos[0].worktree` 改成 B 的路径去 clean，报 `refusing` 且 B 不动；最后 `clean` B 能正常删掉。
- 多仓库测试的路径断言改成新布局（`worktrees/r1/@repos/multi-1/canter-test-ts`）。`cargo nextest run -p canter-eval` 42 个全过；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 干净。

没做的事：第 2 条（多仓库 `prepare` 中途失败会留下没有 `state.json` 的 worktree、`clean` 收不了尾）按主控决定不改，主控记成建议。

踩过的坑：附加路径带 `@` 后，`summary` 里打印的 corral 命令会把 `--cwd` 加上单引号（`quote` 的允许字符里没有 `@`），测试里相应地按带引号的形式断言；命令本身没问题。

## 主控审查

- 改动只在 `eval/` 和任务文件；`long`、`memory`、`memory-2` 题目文件、已有结果表、`crates/` 没动。主控重跑 `cargo nextest run -p canter-eval` 41 个全过；看了 `clean` 先校验全部路径在 worktree 根下再用 `git worktree remove`。
- 交叉审查（Codex `gpt-6-astra` xhigh，`P2-264-交叉审查.md`）提 2 条必须改：第 1 条附加 worktree 路径 `<run>-<repo>` 会和别的 run 撞上、旧 run 再 clean 删掉新 run，接受、返工 1 次（改成 `<轮次>/@repos/<次>/<仓库名>`，clean 校验路径属于这次 run，补回归测试）；第 2 条多仓库 prepare 半途失败留下没记录的 worktree，主控降为建议，记进 HANDOFF。复核 1：可以合并。
- 验收第 2 条：用合并版本重出 `baseline-2026-11-09`、`memory-2026-11-09`、`memory2-2026-11-09` 三张表，和提交的一字不差（返工前后各核一次）。
- 合并前全套：`cargo nextest run --all-targets` 2067 个全过（50 跳过），`cargo test --doc --workspace` 没有失败。
- 验收第 1 条（用 `memory-global` 跑一轮，保留、清空各 3 次，用户看结果表）：合并后主控跑。

结果：返工 1 次

## 验收（11-09，主控跑）

- 轮次 `memglobal-2026-11-09`（DeepSeek v4.1 flash，canter `e42dce0`），保留 `mg-keep-1..3`、清空 `mg-clear-1..3`，24 个会话，表 `eval/results/memglobal-2026-11-09.md`。花费合计约 $0.39。
- 保留 3/3 全过（7/7）；清空 0/3，每次都只差 `assisted-by-trailer`、`diff-check-per-session` 两条。换到 canter-test-ts 后守不守习惯，取决于有没有全局记忆。
- `preference-in-global-memory` 6 次都过：6 次的第 1 个会话都把习惯写进全局记忆（1～2 个文件，文件名各次不同），项目记忆一条没写。`rules-not-in-repo` 6 次都过。
- 用户 11-09：「现在的这个做完如果没有问题就派265」，主控看过结果没有问题。
- 跑的时候碰到的：驱动脚本是主控临时写的（不进仓库），第 2 个会话起 `prepare` 打印的下一步从 1 编号，脚本只认第 3 步，停过一次，改后接着跑，没有重跑会话。`clean` 删了 worktree，但留下空的 `@repos/<次>/` 父目录，主控用 `rmdir` 清掉；不影响下次跑。
