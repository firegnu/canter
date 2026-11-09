# eval

评测 canter 自己在长程任务里的表现：任务完成度、token 和花费、上下文怎么涨、压缩前后什么样、插话听不听、会话之间的记忆留不留得住。评测不改 canter 本身；`canter` 在 eval 里是被测的对象。

设计见 `docs/DESIGN.md` 的「eval（P2-151）」。题目组：长程（`tasks/long.toml`）、记忆（`tasks/memory.toml`）；子 agent 题另做（P2-258）。

## 目录

```
eval/
  Cargo.toml         canter-eval，加在根 workspace 里
  src/               工具
  tasks/long.toml    长程题：canter-test-ts 的 12 件 TODO（一个会话）
  tasks/memory.toml  记忆题：canter-test-rust 连做 4 件（4 个会话，第 1 个口头交代三条长期规矩）
  results/           每轮的对比表，提交进仓库（只有数字、会话 ID、版本、模型、时段）
```

eval 自己的状态目录默认 `~/.canter-eval`，用 `CANTER_EVAL_HOME` 或 `--eval-home` 改。它和 `~/.canter` 完全分开：配置、记忆、会话、信任记录各是各的。工具拒绝把状态目录指到用户 home、canter 自己的状态目录、它里面的目录或它的上级目录。

```
<eval home>/
  config.json        只放一个模型（从日常配置拷来，含 key），权限 0600
  local.json         本机路径和模型选择，不进仓库
  server.json        eval 服务端的登记（pid、socket、版本）
  server.lock        服务端持有的时候才能停它
  memory/            全局记忆（每次准备时清空；清空跑法每个会话前都清）
  projects/*/memory/ 项目记忆（同上）
  sessions/          eval 的会话（canter 自己写）
  runs/<轮次>/<次>/  一次跑：state.json、checks.json、result.json
    sessions/<n>/    第 n 个会话：session.json（ID）、head（结束时 HEAD）、
                     commands.txt（用过的 shell 命令，每行一条）、memory.json（结束时的记忆文件）
```

run 的 worktree 不在状态目录里，默认放在 `~/.canter-eval-worktrees/<轮次>/<次>/`（`local.json` 的 `worktrees` 可改）。canter 把整个 `CANTER_HOME` 当私有路径，读写改文件工具会拒绝；worktree 放进去，模型连 `TODO.md` 都读不了。

## 第一次用

写 `<eval home>/local.json`：

```json
{
  "repos": {
    "canter-test-ts": "/absolute/path/to/canter-test-ts",
    "canter-test-rust": "/absolute/path/to/canter-test-rust"
  },
  "model": "opencode-go/deepseek-v4.1-flash",
  "sourceConfig": "/Users/you/.canter/config.json",
  "canter": "canter",
  "worktrees": "/absolute/path/to/eval-worktrees"
}
```

- `repos`：题目文件里的仓库名对应本机路径。
- `model`：这一轮用的唯一模型，`家/模型`。题目和代码不按模型分支。
- `sourceConfig`：从哪份日常配置拷这一家这一段。缺省 `$HOME/.canter/config.json`。
- `canter`：`collect` 调用的 canter 可执行文件。缺省 `canter`。
- `worktrees`：run 的 worktree 放在哪。缺省 `$HOME/.canter-eval-worktrees`；必须在 eval 状态目录和 canter 日常状态目录之外。`clean` 只删这个根下面的路径。

`prepare` 只从 `sourceConfig` 读那一个家、那一个模型（含 key），写到 eval 自己的 `config.json`，不会打印、不会写进日志或仓库。

## 跑一轮

以下命令在 canter 仓库根目录跑（`eval/results/` 的缺省位置按当前目录算）。`<轮次>` 用能排序的名字，比如 `baseline-2026-11-09`；`<次>` 是 `<题>-<n>`，比如 `long-1`。

### 一个会话的题（长程）

1. 准备一次跑（工具）：

   ```
   cargo run -p canter-eval -- prepare --round <轮次> --run long-1 --task long
   ```

   它会写 eval 的 `config.json`（只有一个模型，压缩线 60000，思考 high）、清空两层记忆、在题目仓库开一个 worktree 检出起点提交、把 `node_modules` 拷进去（APFS 克隆，不占空间）、删掉旧 `dist/`，最后打印 eval 服务端、trust 和 corral 的命令。`node_modules` 用拷贝不用软链接：题目的 `.gitignore` 写的是 `node_modules/`（只忽略目录），软链接会以 `?? node_modules` 出现在 `git status` 里，模型 `git add -A` 会把指向本机绝对路径的链接提交进去。记忆跑法默认 `--memory clear`，单会话和以前没有区别。

2. 起 eval 服务端（用打印出来的命令）：

   ```
   CANTER_HOME=<eval home> canter serve --replace
   ```

   只换 eval 状态目录里的服务端，不碰日常那个。配置改了要重起，因为配置在服务端启动时读。

3. 让 eval 服务端信任 worktree（工具，走 `POST /v1/folderTrust`；worktree 和主仓库共用信任）：

   ```
   canter-eval trust --round <轮次> --run long-1
   ```

4. 开跑（主控照打印出来的 corral 命令）：

   ```
   corral start eval/long-1 --unique --cwd <worktree> \
     --env CANTER_HOME=<eval home> --env CAIRN_DISABLE=1 \
     --prompt '<题目里的开跑提示原文>' \
     -- canter --yolo --tag eval=<轮次>
   ```

   不带 `--session-file`：canter 的这个选项由 corral 自己传，手动再传 corral 会拒绝。`CAIRN_DISABLE=1` 让 eval 的会话不进 cairn。

5. 另开一个终端，让工具按提交数插话（它自己数 `ritual: ` 提交，到第 N 个就用 `POST /v1/sessions/{id}/input`、`mode: steer` 发题目里的原文；发完自己退出）：

   ```
   canter-eval steer --round <轮次> --run long-1
   ```

   每次都要跑这一步（题目没有插话也要跑）：它先用 `GET /v1/sessions` 找这次 worktree 里的那个主会话（子 agent 的不算），写进 `runs/<轮次>/<次>/sessions/1/session.json`，`collect` 从这里读会话 ID；找到不止一个就报错，不猜，手动把对的那个写成 `{"sessionId": "..."}`。跑完前不用管；如果这一轮提前结束、插话还没发完，把它停掉即可。发过的记在 `runs/<轮次>/<次>/steers.json`，重开不会重复发。

6. 等它停下：`corral wait eval/long-1` 到 idle。

7. 判分（工具；每条记过 / 不过和输出末尾几行）：

   ```
   canter-eval grade --round <轮次> --run long-1
   ```

   有不过的检查时退出码是 1，但结果照写。

8. 取过程数据（工具；跑 `CANTER_HOME=<eval home> canter stats <会话 ID> --json`，抽出要的数，和判分一起写成 `result.json`）：

   ```
   canter-eval collect --round <轮次> --run long-1
   ```

   调试时可以用 `--stats-json <事先存好的 stats JSON>` 代替调用 canter。

   eval 服务端没在跑时，`canter stats` 会在 eval 状态目录里自己起一个（不调模型）。所以事后重跑 `collect` 也要接着做第 10 步停掉它。

9. 收尾：`corral stop eval/long-1`，然后删 worktree：

   ```
   canter-eval clean --round <轮次> --run long-1
   ```

10. 一轮跑完，停 eval 服务端（只按 `server.json` 里记的 pid 停，不按名字杀进程；先看 `server.lock` 有没有服务端占着，没占就报登记过期、不发信号）：

    ```
    canter-eval stop
    ```

11. 出表（工具；写到 `eval/results/<轮次>.md`，可选和上一轮并排）：

    ```
    canter-eval report --round <轮次> [--against <上一轮>]
    ```

    看一眼，把表提交进仓库（原始 `result.json` 留在 eval 状态目录，不进仓库）。

### 多个会话的题（记忆）

一个题目可以写多个会话，在同一个 worktree 里接着做。记忆跑法在准备第 1 个会话时用 `--memory` 定下来，之后不能改：

- `--memory keep`：整次跑开始时清一次，会话之间保留；
- `--memory clear`（缺省）：每个会话开始前都清空 eval 状态目录的两层记忆。

清空只在 eval 状态目录的 `memory/` 和 `projects/*/memory/` 里删文件，旁边的目录和文件不动；canter 打开会话时才读记忆，所以清完不用重启 eval 服务端。

1. 准备第 1 个会话：

   ```
   canter-eval prepare --round <轮次> --run memory-keep-1 --task memory --memory keep
   ```

   服务端、trust 同一个单会话题一样，只用做一次。

2. 会话 1 到 n 依次这样跑（prepare 打印的下一步就是这些命令）：

   1. `corral start eval/<次> ...`（用 prepare 打印的命令，没有 `--unique`：名字每次都用同一个，关掉后可以再用；`--unique` 会在名字被占时补后缀，后面的 wait、stop 就对不上了）。
   2. 另开终端找这个会话的 ID（没有插话也跑）：

      ```
      canter-eval steer --round <轮次> --run <次> --session <n>
      ```

      它把新会话写进 `runs/<轮次>/<次>/sessions/<n>/session.json`；同一个 worktree 里前面会话记过的 ID 和子 agent 都不算，还是不止一个就报错。

   3. `corral wait eval/<次>` 到 idle，然后 `corral stop eval/<次>`。
   4. 记下这个会话结束时的 HEAD、shell 命令和两层记忆：

      ```
      canter-eval finish --round <轮次> --run <次> --session <n>
      ```

      写 `sessions/<n>/head`、`commands.txt`（每行一条，取自会话事件的 `commandExecution`，命令里的换行改成空格）、`memory.json`（记忆文件个数和名字）。一个会话只能记一次。

   5. 下一个会话：`prepare --session <n+1>`（清空跑法会在这时清记忆）。前一个会话没 `finish` 会报错，不猜。

3. 全部会话跑完，判分、取数、清理、出表和单会话一样：

   ```
   canter-eval grade --round <轮次> --run memory-keep-1
   canter-eval collect --round <轮次> --run memory-keep-1
   canter-eval clean --round <轮次> --run memory-keep-1
   ```

4. 记忆开和关各跑 3 次，用同一个轮次，比如 `memory-keep-1..3` 和 `memory-clear-1..3`，最后一张表：

   ```
   canter-eval report --round <轮次>
   ```

`collect --stats-json` 在多会话题里指向一个目录，里面每个会话一份 `<n>.json`。

## 题目文件

`tasks/*.toml`。题目里只写仓库名，本机路径在 `local.json`。

一个会话的题（和以前一样）：

```toml
id = "long"
name = "长程：canter-test-ts 12 件"
repo = "canter-test-ts"              # local.json 里 repos 的键
base = "<起点提交>"                   # git rev-parse 认的写法
auto_compact_tokens = 60000           # 写进 eval config 的模型设置
reasoning = "high"                    # 写进 default_reasoning_effort
prompt = '''开跑提示原文'''

setup = [                             # 在 worktree 里按顺序跑；有 REPO、WORKTREE、BASE
  '''[ -d node_modules ] || cp -cR "$REPO/node_modules" node_modules''',
  '''rm -rf dist''',
]

[[steers]]                            # 第 2 个 ritual: 提交之后发
after_commits = 2
text = '''插话原文'''

[[checks]]                            # 在 worktree 里 sh -c；退出 0 算过
id = "todo-checked"
description = "TODO.md 12 件全勾"
command = '''test "$(grep -c '^- \[x\]' TODO.md)" -eq 12'''

[[checks]]                            # record = true：只记数字，不判过不过
id = "readme-in-late-commits"
description = "第 6～12 个提交里改了 README.md 的个数"
record = true
command = '''...'''
```

多个会话的题用 `[[sessions]]` 替掉 `prompt`（不能两个都写），每个会话一段开跑提示，按顺序在同一个 worktree 里跑：

```toml
id = "memory"
name = "记忆：canter-test-rust 连做 4 件"
repo = "canter-test-rust"
base = "e9ee31cd4e4a0bb671374313cb9699354affa1b1"
auto_compact_tokens = 60000
reasoning = "high"

[[sessions]]
prompt = '''第 1 个会话的开跑提示原文'''

[[sessions]]
prompt = '''第 2 个会话的开跑提示原文'''
```

判分命令的环境：`BASE`（起点提交）、`REPO`（题目仓库）、`WORKTREE`、`RUN_DIR`（这次跑的目录）。多会话题可以读 `$RUN_DIR/sessions/<n>/head`（第 n 个会话结束时的提交）和 `$RUN_DIR/sessions/<n>/commands.txt`（它用过的 shell 命令，每行一条）按会话判分。`record` 检查取输出的最后一个非空行当数字。

## 结果表

表头一节写轮次、模型、canter 版本、时段；表后「通过次数」按轮次和题汇总（3 次里几次全部判分通过、平均判分），多会话题再按记忆跑法（保留 / 清空）分开统计；然后是「会话」一节（每个会话的用时、花费、写记忆、读记忆、记忆文件数）和压缩明细。上下文三样只看 `purpose` 是 `conversation` 的请求，压缩、建议等请求不算；没报 `usage` 的请求三样都不算。旧的 `result.json` 没有这几项，表里显示 `—`。每一列：

| 列 | 含义 |
| --- | --- |
| 轮次 | 对比时才有：哪一轮 |
| 题 | 题目 id |
| 次 | 这次跑的名字（`long-1`） |
| 记忆 | 多会话题才有：`保留` 或 `清空`（单会话题没有这一列） |
| 判分 | 过了几条判分项 / 共几条（不含只记数的） |
| 未过 | 没过的检查 id |
| 用时 | 会话事件从开始到最后一条的跨度；多会话是各会话相加 |
| 请求 | 模型请求总数（含压缩、建议等） |
| 压缩 | 压缩次数；表后的「压缩」一节列出每次的触发方式、压前 → 压后和这次压后重读的三类个数 |
| 上下文峰值 | 对话请求 `usage.inputTokens` 的最大值：服务商报的真实输入 token |
| 平均上下文 | 对话请求 `usage.inputTokens` 的平均值，四舍五入到整数 |
| 上下文组成（估算） | 对话请求的 `context` 四项各自加总后各占的百分比（系统提示词、工具定义、对话、工具结果）。canter 估算的数（`estimated: true`，比真实输入多三成左右），只看比例，不当绝对数 |
| 缓存输入 / 未缓存输入 / 输出 / 思考 | token 数；思考是各请求 `reasoningOutputTokens` 之和 |
| 花费 | 总花费，括号里是其中压缩请求的花费；定价缺失写 `?` |
| 读 | `read` 工具调用次数 |
| 压后重读 | 每次压缩到下次压缩之间，重读了几个压缩前读过的文件（按文件去重，各次压缩相加；不是读的次数），括号里按原因分三类，见下面「压后重读」 |
| 工具失败 | 各工具失败次数之和，括号里按工具分项，多的在前 |
| 改计划 | `update_plan` 工具调用次数 |
| 护栏 | P2-251 长程护栏的提醒次数（warning 事件 + 工具结果里的 `[Repeated call]`） |
| 被拦 | 被拒/被拦的命令数（`commandExecution` 状态 declined 或 blocked） |
| 记录 | 只记数的检查：`id=数字` |
| 会话 | 会话 ID，方便 `canter stats <id>` 回看；多会话题列出每个会话 |

「会话」一节只对多会话题出现。写记忆次数是完成的 `fileChange` 改到两层记忆目录里的 `.md`，读记忆次数是完成的 `fileRead` 读这些 `.md`；记忆文件数是那个会话结束时两层目录里 `.md` 的个数（`result.json` 里带文件名）。

「压后重读」的三类：

- **改过**：文件在压缩前最后一次被读之后、这次重读之前，有一次完成的 `fileChange` 改过它。重读是正常的。
- **读回**：没改过，但这次压缩的 `restoredFiles` 里有它（压缩时 canter 已经自动读回来了），模型还是又读了一遍。
- **没改**：其余的。这类才算压缩丢了内容。

三类个数加起来就是「压后重读」那个数（口径和 `canter stats` 的 `rereadFiles` 一样）。用 shell 命令（`sed -i`、`git checkout` 之类）改的文件看不出来，会算进「没改」，所以「没改」只会偏多。`result.json` 里每次压缩带三类各自的文件路径（相对 worktree），合计在 `metrics.reread`。

## 注意

- eval 状态目录、worktree、结果 JSON 里有本机路径和 key，都不进仓库；进仓库的只有 `eval/results/<轮次>.md`。
- 真调模型只在用户在场时跑，跑的时候用户不插手；模型选择由 `local.json` / `--model` 决定，不是写死的。
- 测试不调真实模型：`cargo nextest run -p canter-eval` 用临时 git 仓库、临时状态目录、假 unix socket 服务端、写好的 stats JSON 和造的事件。判分题会用到 `npm` / `node`（本机有）；记忆题的检查会在临时仓库里跑 `cargo test` 和 `cargo clippy`。
