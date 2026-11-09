# 任务：eval 记忆题组（一次跑多个会话，记忆保留或清空对比）

2026-11-09，canter/main 交给 canter/dev-eval-memory（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：常规 / 交叉审查要 / 影响面：碰要害（路由：常规（置信 0.52）、要、碰要害。会话之间要清记忆，删错目录会删到不该删的东西）
类型：功能变更
依据：P2-151 方案、P2-256（Anthropic 记忆文章的 CL-Bench 做法）。本轮加记忆题和 eval 工具要补的功能，不改 canter 本身；真实一轮由主控合并后跑。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-257
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09：「就这三组题（但是你要说清楚这三组题的来源）」
- 11-09（主控给了题目草稿：canter-test-rust 连着 4 件、第 1 件口头交代三条规矩、记忆开和关各 3 次）：「我知道了。你按照正常的流程走吧！」
- 11-09：「这个任务应该不会改canter吧？」「但是如果改了eval，上一个版本的eval是不是就没法使用了？」（主控答：只加不换，长程题照旧能跑、第一轮结果照旧能读能比，写进验收）「可以，写任务单给我看」

## 先读

- `eval/README.md` 全文，`eval/tasks/long.toml`
- `eval/src/` 下 `prepare.rs`（`clear_memories`）、`paths.rs`（状态目录和 worktree 根的检查）、`steer.rs` 和 `server.rs`（`find_session`）、`grade.rs`、`collect.rs`、`report.rs`
- `docs/DESIGN.md` 记忆一节（第 365～373 行附近：记忆目录、项目 key、模型用 write/edit 写记忆）和 eval 一节
- 题目仓库（只读，`git show` 看）：`~/Developer/personal_projs/canter-test-rust` 的提交 `e9ee31c` 的 `TODO.md`、`AGENTS.md`、`Cargo.toml`

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/eval-memory`，分支 `eval-memory`（已从 main 建好）。只动 `eval/` 和 `docs/DESIGN.md` 的 eval 一节。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-memory`。

## 要做的

### 一、题目：`eval/tasks/memory.toml`

- 仓库 `canter-test-rust`，起点 `e9ee31c`（不用最新的 `1a487dd`：它加了 `anyhow` 依赖，和仓库 AGENTS.md「只用标准库」冲突）。压缩线 60000、思考 high，和长程题一样。
- 一次跑连着 4 个会话，在同一个 worktree 里接着做，每个会话做 TODO 里的一件：
  1. Add an edit command
  2. Support weekly reports
  3. Add CSV export
  4. Add monthly project comparisons
- 每个会话的开跑提示（`<件名>` 换成上面那件的英文标题原文）：

  ```
  做 TODO.md 里的「<件名>」这一件：实现、补测试，cargo test 和 cargo clippy -- -D warnings 都通过后，在 TODO.md 勾掉它，再单独提交一次。不要加任何依赖。中途不要停下来问我，自己拿主意继续做。不要开子 agent，全部自己做。
  ```

  只有第 1 个会话的提示后面再接一段（原文）：

  ```
  另外这个仓库有几条长期的规矩，以后在这里做的每件活都要守：
  1. 提交说明的标题写成 tc(<子命令名>): <说明>，比如 tc(edit): add edit command。
  2. 提交时按文件名 git add，不要用 git add -A 或 git add .。
  3. 每加一个子命令或新选项，在 docs/commands.md 里加一节，写用法和一个例子；没有这个文件就建一个。
  ```

  canter 的开跑提示要能放进 corral 的 `--prompt`；多行有问题的话，停下来问主控，不要自己改写原文。
- 判分（只看第 2～4 个会话做的事）：
  - TODO.md 里这 4 件都勾了；
  - `cargo test` 和 `cargo clippy -- -D warnings` 都过；
  - `Cargo.toml` 的依赖和起点一样；
  - 第 2～4 个会话的每个提交，标题都匹配 `^tc\([a-z][a-z-]*\): `；
  - 第 2、3、4 个会话各自的提交都改了 `docs/commands.md`；
  - 第 2～4 个会话用过的 shell 命令里，一次都没有 `git add -A`、`git add --all`、`git add .`。
- 只记数：每个会话结束时两层记忆里共几个 `.md`、这个会话写了几次记忆、读了几次记忆（见下面三）。

### 二、eval 工具：一次跑多个会话、记忆保留或清空

- 题目文件能写多个会话（每个会话自己的开跑提示）；`long.toml` 不用改，没写多个会话的题照旧是一个会话。
- `prepare` 加一个记忆跑法的选项：「保留」（会话之间不清）或「清空」（每个会话开始前清 eval 状态目录的两层记忆）。每次跑开始时照旧清一次。跑法记进这次跑的状态，结果表要显示。
- 主控照 README 一个会话一个会话地跑：准备这个会话（清空跑法时清记忆）→ corral 开 canter → 找到这个会话的 ID → 等它停下 → 关掉 → 记下这个会话结束时的 `HEAD` 和记忆文件 → 下一个会话。具体命令你定，写进 README，`prepare` 打印的下一步跟着改。
- 找会话 ID：同一个 worktree 里会先后有好几个会话，要认出这次新开的那个（之前记过的不算，子 agent 的不算）；还是多于一个就报错，不猜。
- 判分命令多一个环境变量指向这次跑的目录，里面每个会话有：结束时的 `HEAD`、这个会话用过的 shell 命令（每行一条，取自会话事件里的 `commandExecution`）。上面的判分照这些写。
- `collect` 按会话收：用时、请求、花费、token（和现在的口径一样），写记忆次数（完成的 `fileChange` 改到 eval 状态目录记忆目录里的 `.md`），读记忆次数（完成的 `fileRead` 读的是记忆目录里的 `.md`），结束时记忆文件个数和名字。整次跑的合计照现在的列写。
- 结果表：多会话的题加一列「记忆」（保留 / 清空）；表后加一节，每次跑按会话列用时、花费、写记忆、读记忆、记忆文件数；「通过次数」按跑法分开统计。只有一个会话的题，表和现在一样。

### 三、清记忆的安全

- 只清 eval 状态目录里的 `memory/` 和 `projects/*/memory/` 下的文件，沿用 `clear_memories` 和 `paths.rs` 现有的检查（拒绝用户 home、canter 日常状态目录、它里面和上级的目录）；不碰其它任何目录。
- 清完不需要重启 eval 服务端：canter 打开会话时才读记忆。如果你发现必须重启，停下来问主控。

## 怎么算做完

- 用户 11-09 定的：改了 eval 以后，上一版能用的照旧能用：
  - 长程题 `long.toml` 不改，照原来的命令和步骤能跑；
  - 主控用新版对第一轮 `baseline-2026-11-09` 重出结果表，和现在 main 上那张一字不差（`report` 不重跑 `collect`）；
  - 新版跑出来的长程题结果能和第一轮用 `report --against` 并排。
- 主控提议、待用户确认：合并后主控跑记忆题第一轮（DeepSeek flash，保留、清空各 3 次，约 1.5 小时、约 $1～2，用户在场），出一张结果表，用户看过觉得能用。
- 验证只做这些（碰要害一档）：针对新功能的测试（多会话题的准备和找会话 ID：同一 worktree 先后两个会话能认对、多于一个报错；清空跑法每个会话前清、保留跑法不清，清的只有两层记忆目录、旁边的文件不动；判分环境变量和每个会话的命令文件；按会话收数据：造事件，写记忆、读记忆、普通文件读写分得开；旧的单会话 `result.json` 照样出表）；`cargo nextest run -p canter-eval`；`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `crates/` 下的任何代码，不改 `canter stats` 的口径。
- 功能不绑定 DeepSeek（DESIGN 第 23 条）：模型名只从 `local.json` / `--model` 来，代码里不按模型名分支。
- 不读、不改 `~/.canter`、`~/.canter-eval`，不改 canter-test-rust 仓库（只用 `git show` 看）；不起任何 canter 服务端，不调真实模型接口。测试只用临时目录、临时 git 仓库、假服务端和造的事件。
- 不改长程题的题目文件和它的判分。
- 不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（包括提示原文放不进 `--prompt`、必须重启服务端、口径和上面写的对不上），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不换别的办法。
- 不合并到 main，不推送。只在 `eval-memory` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

**做了什么**

- `eval/tasks/memory.toml`：canter-test-rust（起点 `e9ee31c`）连做 4 件：Add an edit command、Support weekly reports、Add CSV export、Add monthly project comparisons；第 1 个会话的提示后面接任务单给的三条长期规矩原文。六个判分照任务单：4 件勾掉、`cargo test` 和 `cargo clippy -- -D warnings` 过、依赖不变、第 2～4 个会话的提交标题都是 `tc(...): `、第 2～4 个会话各自改了 `docs/commands.md`、这些会话的 shell 命令里没有 `git add -A` / `--all` / `.`。
- 题目文件支持多个会话（`[[sessions]]`，每个一段开跑提示）；没写多个会话的 `long.toml` 照旧一个 `prompt`，没动。
- `prepare` 加 `--memory keep|clear`（缺省 `clear`）和 `--session n`。清空只删 eval 状态目录的 `memory/` 和 `projects/*/memory/` 里的东西，沿用原来的状态目录检查；canter 打开会话时才读记忆，清完不重启服务端。
- 每个会话在 `runs/<轮次>/<次>/sessions/<n>/` 记 `session.json`、`head`、`commands.txt`（完成的 `commandExecution`，每行一条）、`memory.json`；新增 `canter-eval finish` 记这些。`steer --session n` 找新会话时排除这次跑已经记过的 ID 和子 agent 的会话，仍不止一个就报错。
- `grade` 的检查环境多一个 `RUN_DIR`；`collect` 按会话取 stats 和事件，合计照原来的列，每个会话另记用时、花费、写记忆、读记忆、记忆文件个数和名字；`report` 对多会话题加「记忆」列和「会话」一节，「通过次数」按保留 / 清空分开统计。单会话题的表不变。
- `eval/README.md` 和 `docs/DESIGN.md` 的 eval 一节写清多会话流程、记忆跑法、run 目录里的文件、`RUN_DIR`、结果表新增的列和节。

**验证了什么**

- `cargo nextest run -p canter-eval`：31 个全过（新增 7 个集成测试、4 个单元测试）。
- `cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt -p canter-eval -- --check`、`git diff --check` 都过。
- 多会话准备：清空跑法每个会话前清、保留跑法只在整次跑开始时清；只动两层记忆目录，旁边的文件不动；前一个会话没记时拒绝下一个；跑法和 `--memory` 不一致、重复准备都拒绝。
- 找会话：同一个 worktree 里先后两个会话认对新 ID（排除已记的和子 agent 的），两个都没记过时报错且不列已记的。
- 判分：用真的 `memory.toml` 在临时 cargo 仓库上跑，六条全过；逐条造失败（TODO 没勾、加了依赖、提交标题不合、会话没改 `docs/commands.md`、命令里 `git add -A`、测试失败）都判不过；`RUN_DIR` 由判分环境给到。
- 按会话收数据：造事件，完成的 `fileChange` / `fileRead` 碰到两层记忆里的 `.md` 才算写 / 读，普通文件读写不算；多会话的用时、请求、峰值、平均、token、花费、工具失败合计正确。
- 旧结果：把 `sessionCount` / `memoryMode` / `sessions` 从 result.json 删掉后出表，和带这些字段的单会话结果一字不差，表里没有「记忆」和「会话」。

**拿主意的地方**

- `--memory` 缺省 `clear`，和以前单会话一样；`keep` 也在整次跑开始时清一次（任务单：「每次跑开始时照旧清一次」）。
- 会话记录放 `sessions/<n>/`；旧跑没有这些目录时，`collect` 的第 1 个会话仍退回 `state.sessionFile`，旧结果照读。
- `finish` 不覆盖已记的 `head`；`prepare --session n` 要求前一个会话的 `head` 已经在。
- 多会话的 `corral start` 打印时去掉 `--unique`：同一个名字要跨会话复用，`--unique` 在名字被占时会补 `-1` 后缀，后面的 wait/stop 就对不上；单会话保持原来的 `--unique`。
- `commands.txt` 把命令里的换行改成空格，保证每行一条；多会话题的 `--stats-json` 是目录，每个会话一份 `<n>.json`。
- 结果表只在 `sessionCount > 1` 时加「记忆」列和「会话」一节；「写记忆 / 读记忆」只数完成的 `fileChange` / `fileRead` 碰到的两层记忆目录里的 `.md`。
- 多行开跑提示：corral 的 `--prompt` 是单个 argv，打印的命令用单引号包住整段（原文里没有单引号）；没有起真实 agent 验证整条链路（任务单不许调真实模型 / 服务端）。

**没做的事**

- 没有真跑记忆题（合并后由主控按任务单跑）；没有起过 canter 服务端、没有调模型。
- 没改 `crates/`、长程题 `long.toml` 和它的判分、`canter stats` 的口径。
- 没跑全套（任务单只要求 `-p canter-eval` 和那三项检查）。

**踩过的坑**

- 结果表「会话」一节的行尾少了一个 ` |`，测试当场抓到，补上了。
- 临时仓库的 TODO 标题在 `**...**` 里带句号，fixture 和检查一开始少了句号，勾不上；按 TODO 原文改了。
- 集成测试在同一个临时目录里起两个假服务端，第二次 bind 报 `Address already in use`；改成两个子目录。另外 shadow 掉的 `Fake` 不会当场 drop，监听还在，旧 socket 不能重用。
- 旧的 `State`、`SteerOptions`、`RunResult` 构造在测试里都要补新字段；会话文件从 run 根挪到 `sessions/1/`，相关测试一起改了。

## 返工

2026-11-09，按主仓库交叉审查（`P2-259-交叉审查.md` 的「## 审查意见」）返工 7 条，都在本分支、未合并。

**改了什么**

1. （必须改 1）清记忆和列记忆文件不跟随符号链接：`memory.rs` 用 `symlink_metadata` 逐组件检查 `memory`、`projects`、`projects/<key>/memory`，本身是链接就报错不动；`projects/` 里的项目目录是链接也报错；目录里的条目用 `file_type` 判断，链接只删链接本身、不计入 `.md` 快照。加了链接目录和链接条目的测试，断言外面的目标原样还在。
2. （必须改 2）单会话旧状态先读 `state.sessionFile`（run 根的 `session.json`），不再把当前会话当历史会话排除；新增 `paths::session_id_file`，`steer`、`finish`、`collect` 共用。多会话报歧义时打印 `sessions/<n>/session.json` 的准确路径。保留一条旧状态格式的回归测试（state.json 没有 `sessionCount` / `memoryMode`，ID 在 run 根；假服务端同时列出这个 ID 和另一个候选，断言直接用旧 ID、不列会话、不写 `sessions/1/`）。
3. （必须改 3）`commands.txt` 里命令内的换行改成 `; `（先归一 CRLF / CR），保留命令边界；`memory.toml` 的 `no-blanket-git-add` 改成按参数边界匹配的正则：`git` 后可有多个空格和任意多个 `-C <目录>` 之类的全局选项，然后 `add`，参数是单独的 `-A`、`--all` 或 `.`，后面接空白、`;`、`&`、`|` 或行尾。测试覆盖 `git add -A`、`git  add  -A`、`git add --all`、`git add .`、`cargo test && git add .`、`cargo test; git add -A`、`git -C "$WORKTREE" add .`（都判违规）和 `git add .gitignore`、`git add ./src/cli.rs`、按文件名 `git add src/cli.rs TODO.md`（都放过）。
4. （必须改 4）`dependencies-unchanged` 改成比较起点和现在的 `Cargo.lock` 里 `[[package]]` 的 name 集合（`git show "$BASE:Cargo.lock"` 对当前文件，`sed` 取 `name = "..."` 行排序比较），不再用 awk 读 `Cargo.toml`。测试仓库的起点提交带 `Cargo.lock`；用 `[dependencies.foo]` 子表加依赖、锁文件里加 `foo` 包时判不过，恢复后判过。
5. （必须改 5）单会话 `collect --session <id>` 显式指定时以它为准，跳过与已存 ID 的冲突校验；没给时仍用已存 ID 并严格校验。多会话照旧按每个会话记录的 ID 严格校验、忽略这个选项。测试：已存 `s-other`、`--stats-json` 是 `s-collect` 时，不给 `--session` 报冲突，给了就按 `s-collect` 收数。
6. （建议改 6）`PrepareOptions.task` 改成 `Option<String>`：第 1 个会话必填，第 2 个及以后从 `State.task_path` 读题目，`--task` 给了且和这次跑不一致才报错。`prepare` 打印的下一会话命令和 README 现在照原样可直接执行。
7. （建议改 7）「通过次数」只对多会话题按 `(轮次, 题, 记忆跑法)` 分组；单会话题照原来的 `(轮次, 题)` 分组，同一轮同一题的旧结果（无 `memoryMode`）和新结果（`clear`）合成一行。测试用一份删掉新字段的旧结果和一份带 `clear` 的新结果，断言只有一行 `2/2`、`2.0/2`。

另外把 DESIGN 的 eval 一节和 README 的流程、`commands.txt` 说明、`prepare` 的 `--task` 说明一并更新。

**验证了什么**

- `CARGO_TARGET_DIR=…/.target/eval-memory cargo nextest run -p canter-eval`：34 个全过（新增 3 个：旧状态 steer 回归、链接目录清理、单会话显式 `--session`；扩了依赖、`git add` 各形式、通过次数分组的断言）。
- 同一编译目录的 `cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt -p canter-eval -- --check`、`git diff --check` 都过。
- 没有起 canter 服务端、corral agent，没调真实模型，没读 `~/.canter` / `~/.canter-eval`；判分测试只在临时 cargo 仓库里跑。

**拿主意的地方**

- 符号链接按「层目录本身及 `projects/` 下的项目目录是链接就报错，目录里的条目是链接只删链接」处理；列记忆文件同一规则，链接条目不进快照。
- `no-blanket-git-add` 的正则不做 shell 解析，按主控定的参数边界识别；引用里的 `git add .`（比如 `echo "git add ."`）如果前面不是空白、`;`、`&`、`|` 就不会误中。
- 后续 `prepare` 的 `--task` 给了相同值也接受，只有不一致才报错；`--tasks` 目录在后续会话里不影响，题目从 `state.task_path` 读。
- 通过次数里单会话行在多会话表同时出现时「记忆」列写 `—`。

**没做的事**

- 没有真跑记忆题、没起真实 agent 服务端；没有改 `crates/`、`long.toml` 和 `canter stats` 口径。
- 没有跑全套（按任务单只跑 `-p canter-eval`、clippy、fmt、`git diff --check`）。

**踩过的坑**

- 一次 `edit` 里第三个替换的原文对不上，整次没写入；重新读文件确认后逐段改，`prepare` 的签名一度和调用对不上，靠编译发现。
- 记忆测试把 `clear` 和 `files` 放进同一个数组遍历，两个 `Result` 的成功类型不同，编译不过；改成分别断言。
- 新加的旧状态 steer 测试里假服务端需要 `mut` 才能读请求，编译提示后补上。

## 返工 2

2026-11-09，按主仓库交叉审查「## 复核 1」的两条必须改返工，仍在 `eval-memory` 分支、未合并。

**改了什么**

1. `no-blanket-git-add` 的正则改成在 `add` 后面的参数列表里找禁用参数，不再只看第一个参数：`add` 后可以跟任意多个非分隔符参数（普通选项、`--`、文件名），只要其中有一个单独的 `-A`、`--all` 或 `.` 就算违规；仍然按参数边界匹配，不做 shell 解析。`git add -- .`、`git add -v -A`、`git add --verbose --all`、`git add src/lib.rs -v -A` 现在都判违规；`git add .gitignore`、`git add ./src/cli.rs`、`git add -- .gitignore`、`git add -v .gitignore` 仍不算违规。
2. 单会话 `collect` 的预期 ID 改成 `explicit.or(recorded)`，再和 `stats.sessionId` 比较：显式指定 B、记录是 A、stats 是 B 时照旧收数；显式指定 B、stats 实际是 C 时报错，不再跳过一致性检查。多会话仍用各自记录的 ID 严格校验。

**验证了什么**

- `cargo nextest run -p canter-eval`：34 个全过。`git add` 的失败/放过断言已补上面几种；`collect_extracts_metrics_and_counts_events` 里加了「显式 `s-wanted`、stats 是 `s-collect` 必须报错」，原来「记录是 `s-other`、显式 `s-collect` 成功」的断言保留。
- `cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt -p canter-eval -- --check`、`git diff --check` 都过。
- 没起 canter 服务端、corral agent，没调真实模型，没读真实 eval 状态目录。

**拿主意的地方**

- 正则把 `--` 后面的参数也当作参数检查：`git add -- .` 必须抓到；代价是 `git add -- --all` 这种给文件起名叫 `--all` 的极端写法也会判违规，这个题组的仓库里不会出现。
- 显式 ID 不一致的报错写 `asked for as <B>`，记录不一致仍写 `recorded as <A>`，方便区分是哪一种失配。

**没做的事**

- 没做 shell 分词或引号求值，按主控定的参数边界办法；没跑全套。
