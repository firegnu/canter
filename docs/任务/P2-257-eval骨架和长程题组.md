# 任务：第二期 - eval 骨架和长程题组

2026-11-09 写好，canter/main 交给 canter/dev-eval-long（canter，`canter --yolo`，模型用 canter 服务端配置的那个）。
路由：常规 / 交叉审查要 / 影响面：改行为（路由：常规（置信 0.86）、拿不准、拿不准；推翻：要从用户配置拷 API key，按安全和隐私算要交叉审查；不改 canter 本身，验证按改行为）。
类型：功能变更
依据：P2-151 方案（`docs/任务/P2-151-eval.md`「方案」一节）、讲解页 `docs/任务/P2-151-讲解.html`。本轮只做 eval 骨架、工具和长程题组，不改 canter 本身。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-29：「还有我想有一个eval的功能，因为我不能在没有eval的情况下没发评估现在的agent到底效果如何？要设计一个eval的方案。……eval要包括任务完成度，token消耗量等等综合指标。。。尤其是长程任务的上下文窗口的变化以及压缩之后是什么样子」
- 11-09：「最后集中精力来做151(我理解其实可以借助corral，让opus主控来直接分配eval的benchmark题委派给canter来做，这样就不会要使用canter -p这种来做了）」
- 11-09 对讲解页 6 个问题：「1. 同意 2. 就这三组题（但是你要说清楚这三组题的来源）3. 我要在场，但是我不会干预 4. 先只用deepseek flash 5. ……6. 放eval」；第 5 条主控解释「上一版」后，用户：「可以，写方案和任务单给我看」。

## 这件做什么（一句话）

在 canter 仓库加 `eval/`：一个小 Rust 程序 `canter-eval`、一份给主控照着跑的说明、和第一组题「长程」。做完后，主控用它跑第一轮（canter 当前版、DeepSeek flash、长程题 3 次），出第一张结果表，当以后每版对比的基准。

## 一轮怎么跑（照这个做工具）

主控照 `eval/README.md` 做，工具负责其中确定的几步：

1. **eval 自己的状态目录**（默认 `~/.canter-eval`，可用环境变量改）：eval 起一个独立的 canter 服务端，`CANTER_HOME` 指向这里。和用户日常的 `~/.canter` 完全分开：配置、记忆、会话、信任记录各是各的。
2. **准备一次跑**（工具）：
   - 写 eval 的 `config.json`：只放一个模型（从用户 `~/.canter/config.json` 拷 `opencode-go` 这家里 `deepseek-v4.1-flash` 的那一段，含 key，只在本机，不打印、不进日志和仓库），不挂 MCP、钩子、状态栏；题目要求的设置（长程题：这个模型的 `auto_compact_tokens` 设 60000）。
   - 清空 eval 状态目录里的两层记忆。
   - 在题目仓库开一个新 git worktree，检出题目定的提交；准备好跑测试要的东西（canter-test-ts 要 `node_modules`，从题目仓库本身拷或链，不联网装）；删 `dist/` 这类旧编译结果。
   - 让 eval 服务端信任题目仓库（用 canter 公开接口 `POST /v1/folderTrust`；worktree 共用主仓库的信任，见 DESIGN 信任一节），这样 canter 打开时不会卡在「是否信任」框上。
   - 打印主控要跑的 `corral start` 命令（下面第 3 步）。
3. **开跑**（主控）：`corral start eval/<题>-<n> --unique --cwd <worktree> --env CANTER_HOME=<eval 目录> --prompt "<开跑提示原文>" -- canter --yolo --tag eval=<轮次> --session-file <eval 目录里这次跑的会话文件>`。
4. **插话**（工具）：在 worktree 里数满 N 个以 `ritual: ` 开头的新提交后，用 canter 公开接口 `POST /v1/sessions/{id}/input`、`mode: steer`（终端按 Tab 插话走的就是它）发题目里写的原文。会话 ID 从 `--session-file` 写的文件里读。主控在后台开着它，跑完前不用管。
5. **等它停下**（主控）：`corral wait` 到 idle。
6. **判分**（工具）：在 worktree 里跑题目文件写的检查，每条记过 / 不过和输出末尾几行。
7. **取过程数据**（工具）：`CANTER_HOME=<eval 目录> canter stats <会话 ID> --json`，抽出要比的数，和判分一起存成这次跑的一份 JSON（放 eval 状态目录，不进仓库）。
8. **收尾**（主控 + 工具）：`corral stop`，删 worktree。一轮跑完，工具按记下的 PID 停 eval 服务端（从 eval 目录的 `server.json` 读 `pid`），不按名字杀进程。
9. **出表**（工具）：把这一轮所有跑的 JSON 汇成 `eval/results/<轮次>.md`（这个文件进仓库），可选和上一轮并排。

工具的命令怎么分、叫什么，你定，写进 `eval/README.md`。

## 长程题（`eval/tasks/` 下一个数据文件，格式你定，TOML 或 JSON）

来源：P2-140 建的测试仓库 canter-test-ts，提交 `8e19ca8 初始骨架`，TODO.md 12 件；10-31 起 P2-144、P2-176、P2-81 在上面跑过多次长程试跑，下面的提示和插话照 11-07 那次（会话 `s-1794028618032124000-1486-1`）原文。

- 仓库：canter-test-ts。**题目文件里只写名字**，本机路径记在 eval 状态目录的本机配置里（canter 是公开仓库，不放本机路径）。
- 起点：`8e19ca8`。
- 设置：`auto_compact_tokens` 60000；思考 high；`--yolo`（权限 Allow all，沙箱关）。
- 开跑提示（原文）：

  ```
  顺序完成 TODO.md 里的全部 12 件任务。每件：实现、补测试、npm test 和 npx tsc --noEmit 都通过后，在 TODO.md 勾掉它，再单独提交一次，提交说明以 ritual: 开头。不要加任何依赖。中途不要停下来问我，自己拿主意继续做。不要开子 agent，全部自己做。
  ```

- 插话（原文；11-07 那次实际都在对应提交后 20 多秒发出，这里固定成「第 N 个 `ritual: ` 提交之后」）：
  - 第 2 个之后：`补一条要求：从现在开始，每个提交说明的最后一行加上 Task: N/12，N 是这件在 TODO 里的序号。`
  - 第 3 个之后：`先记住一个细节，做第 12 件（交互确认）时用：确认提示的文字用 Remove "<habit name>"? [y/N]`
  - 第 5 个之后：`再补一条：之后每件新加的命令或选项，都在 README.md 里加一个用法示例。`
- 判分（照 P2-81「收尾」那几条）：
  1. TODO.md 12 件全勾。
  2. 起点之后正好 12 个提交，都以 `ritual: ` 开头。
  3. 第 3～12 个提交的说明最后一行是 `Task: N/12`（N 对上）。
  4. `npm test` 通过；`npx tsc --noEmit` 通过。
  5. 源码里有确认提示 `Remove "` … `"? [y/N]`。
  6. `package.json` 的依赖和起点一样（没加依赖）。
  7. 记一个数、不判过不过：第 6～12 个提交里改了 README.md 的有几个。
- 过程数据（从 stats 取）：用时、请求次数、压缩次数和每次压前压后、缓存命中 / 没命中的输入、输出（其中思考）、花费（另列压缩花费）、`read` 次数、压缩后重读文件次数、各工具失败次数、`update_plan` 次数、长程护栏（P2-251）的提醒次数、命令被拦次数、最终测试条数。有的 stats 里没有现成的数（比如护栏提醒、被拦），从会话事件里数。

## 用户定的（11-09，四件都按主控建议）

用户先问「我没明白你具体思路是什么以及会不会修改canter的代码」，主控解释：不改 canter 的代码，只在仓库加 `eval/`；canter 在 eval 里是被测的对象。用户：「都按你的建议，派吧」。

1. **插话走 canter 公开接口**：`corral send` 在 canter 干活时退回 7 `not_idle`，`corral keys` 送不了 Tab，所以由 eval 工具用 `POST /v1/sessions/{id}/input`、`mode: steer` 发，和终端按 Tab 一样。corral 只用来开、等、停。
2. **交给 canter**（`canter --yolo`），交叉审查 Claude Code（`opus[1m]` high）。
3. **结果表进仓库**：每轮的对比表 `eval/results/<轮次>.md` 提交进仓库（只有数字、会话 ID、canter 版本、时段，没有本机路径和密钥）；每次跑的原始 JSON 留在 `~/.canter-eval`。
4. **验收**：「主控用 `eval/` 跑完第一轮（canter 当前版、DeepSeek flash、长程题 3 次），出一张结果表，你看过觉得能用」。

## 先读

- 项目规矩 `CLAUDE.md`（不用 Python、测试不调真实模型、不碰用户状态、按 PID 停进程）。
- `docs/任务/P2-151-eval.md` 的「方案」一节；`docs/任务/P2-81-测试长程任务的可靠性.md` 的「怎么跑」「跑完看什么」。
- `docs/DESIGN.md`：状态目录和 `CANTER_HOME`（第 15 条）、信任（找 `trusted_folders.json`）、`--session-file`（P2-191）、`canter exec` 和 `--tag`、`POST /v1/sessions/{id}/input` 的 `mode`。
- `canter stats --json` 的输出（`crates/canter-core/src/stats*`、`crates/canter-protocol/src/stats.rs`）；canter 客户端怎么连 `server.sock`（`crates/canter-tui`、`crates/canter`），插话请求照它发。

## 在哪里干活

- worktree：`../canter-worktrees/eval-long`，分支 `eval-long`（派出时从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/eval-long`。
- 主要动 `eval/`（新 crate，加进根 `Cargo.toml` 的 workspace members）；不动 `crates/` 下 canter 本身的代码。真需要动的，停下来问。

## 要做的

1. `eval/` 下的 `canter-eval`：上面「一轮怎么跑」里标「工具」的几步。
2. `eval/tasks/` 下长程题的数据文件，照上面「长程题」。
3. `eval/README.md`：主控跑一轮的步骤（含要敲的 corral 命令）、eval 状态目录的布局、题目文件的格式、结果表每一列是什么。
4. `docs/DESIGN.md` 加一小节「eval（P2-151）」：eval 状态目录和日常分开、题目和结果放哪、插话走的接口。

## 怎么算做完

- 用户的验收（11-09 用户同意主控建议）：「主控用 `eval/` 跑完第一轮（canter 当前版、DeepSeek flash、长程题 3 次），出一张结果表，你看过觉得能用」。这一步要真调模型，合并后由主控在用户在场时跑，不在开发时跑。
- 开发时的验证只做这些：
  - 工具的测试都用本机的东西：临时 git 仓库当题目仓库、临时目录当 eval 状态目录、本机起的假 unix socket 服务端收插话请求、事先写好的 stats JSON 当过程数据。不调真实模型，不碰 `~/.canter` 和 `~/.canter-eval`。
  - 要测到：准备一次跑（配置只有一个模型、压缩线对、记忆清空、worktree 在对的提交、key 不出现在任何输出里）；数到第 N 个提交才插话、发出的请求是 `mode: steer` 和原文；长程题 7 条判分各有过和不过的例子；从 stats JSON 抽数；两轮并排出表；停服务端只停 `server.json` 里那个 PID。
  - `cargo nextest run -p canter-eval`；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 canter 本身（`crates/` 下的代码、提示词、协议）。缺接口就停下来问。
- 功能不绑定 DeepSeek：模型名、家名都从配置读，题目文件和代码里不按 DeepSeek 分支。第一版只用 DeepSeek flash 是跑的时候的选择。
- 不调真实的模型接口；不跑真的一轮 eval（合并后主控跑）；不起、不停、不替换任何真的 canter 服务端（你自己就跑在用户的 canter 服务端上，`canter serve --replace`、`canter upgrade` 会把你自己停掉）。测试里要服务端就用假的。
- 不碰用户的 `~/.canter`（只在「准备一次跑」时只读地拷那一段模型配置）、真实会话、记忆；不碰 `~/.canter-eval` 以外的地方。
- 不把本机路径、key、令牌写进仓库、日志、测试快照或提交。
- 不用 Python 写任何代码、测试或辅助脚本；判分命令是题目文件里的 shell 命令。
- 不做子 agent 题组（P2-258）、记忆题组（P2-259）、Terminal-Bench（P2-260）。
- 不要按名字批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `eval-long` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

做了什么：加了 `eval/`（workspace member），`canter-eval` 八个命令：`prepare`（写只有一家的 eval config、清两层记忆、开 worktree、跑题目 setup、存 state）、`trust`（POST /v1/folderTrust）、`steer`（数 `ritual: ` 提交，POST /v1/sessions/{id}/input、mode steer，进度存 steers.json）、`grade`（跑题目里的 shell 判分，记过/不过和输出末尾）、`collect`（`canter stats <id> --json` + 会话事件，写 result.json）、`clean`（删 worktree）、`stop`（只按 server.json 的 pid 停）、`report`（`eval/results/<轮次>.md`，可和上一轮并排，带通过次数汇总）。另加 `eval/tasks/long.toml`（canter-test-ts 12 件、三句插话、P2-81 的七条判分 + 最终测试条数）、`eval/README.md`、DESIGN 的「eval（P2-151）」一节。没动 `crates/`。

验证了什么：`cargo nextest run -p canter-eval` 12 个测试全过，覆盖准备一次跑（config 只有一个家一个模型、压缩线 60000、思考 high、记忆清空、worktree 在起点提交、node_modules 链上、dist 删掉、key 不出现在输出里）、第 N 个提交才插话且请求是 `mode: steer` 和原文、重开不重发、长程题每条判分的过/不过例子（record 项改数字）、stats JSON 抽数 + 事件里的护栏/被拦计数、两轮并排出表、按 server.json 的 PID 停且不碰另一个进程、trust 的请求体和路径。另用临时仓库跑了 CLI 的 `prepare` 和 `clean`。`cargo clippy -p canter-eval --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过。

拿主意的地方：

- `prepare` 和 `trust` 分成两条命令。eval 服务端要读 `config.json`，而 config 由 prepare 写，所以顺序只能是 prepare → 起服务端 → trust；prepare 的结尾把这三步和 corral 命令一起打印出来。
- 题目格式用 TOML；repo 只写名字，路径在 eval 状态目录的 `local.json`（`repos`、`model`、`sourceConfig`、`canter`）。判分命令的环境给 `BASE`、`REPO`、`WORKTREE`；`record = true` 的检查取输出最后一个非空行当数字。
- `node_modules` 从题目仓库链、`dist` 删除，写成 `long.toml` 的 `setup` 命令，不写进代码；压缩线和思考档位也来自题目文件。
- `collect` 加 `--stats-json` 以便用事先写好的 stats JSON 测试，生产路径仍是 `CANTER_HOME=<eval home> canter stats <id> --json`。
- 护栏提醒次数 = 三条 P2-251 warning 前缀 + 工具结果里的 `[Repeated call]`；被拦命令 = `commandExecution` 状态 declined 或 blocked；两条都写进 README 的列说明。
- eval config 除模型和题目设置外不加开关（不写 prompt_suggestions），保持 canter 默认；因此 stats 的请求数和花费里会含提示建议请求，result.json 另存了 conversationRequests。要不要为评测关掉建议，留给主控定。
- `stop` 只认 `server.json` 的 pid：先确认 PID 活着，再 SIGTERM，最多等 5 秒；不按进程名找，也不碰别的进程。

没做的事：没跑真实一轮（按任务留给合并后、用户在场时跑）；没起、不停、不替换任何真的 canter 服务端；没写子 agent / 记忆 / Terminal-Bench 题组；没合并、没推送。

踩过的坑：测试里造 12 个 ritual 提交时，一开始按「第 n 个未勾选项」改 TODO，结果第 10 个提交没有改动、`git commit` 直接失败，改成每步按序号重建 TODO。停服务端的测试一开始用 `sleep 60 & echo $!` 拿 PID，后台进程继承了 stdout 管道，`Command::output()` 一直等到 60 秒；改成把后台进程的 stdout/stderr 丢掉。另外冒烟时一条 `rm -rf "$S"` 被 canter 自己的 dangerous command 检查拦下（AllowAll 也拒），改用 `mktemp -d` 新目录。
