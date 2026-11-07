# 任务：第三期 - 项目里没有 AGENTS.md 时读 CLAUDE.md

2026-11-07 写好，canter/main 交给 canter/dev-claude-md（canter，`canter --yolo`，模型用服务端配置的 `opencode-go/deepseek-v4.1-flash`，用户定）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.6）、拿不准、拿不准；只改读哪个说明文件和 `/init` 的提示词，不碰权限、沙箱、数据格式，交叉审查定不要、影响面定改行为）。
类型：功能变更
依据：canter 只读 `AGENTS.md`，用户的项目大多只有 `CLAUDE.md`（canter 仓库自己就是），在这些项目里用 canter 时看不到项目规矩。pi、omp 都会读 `CLAUDE.md`。本轮只改项目层的读法、`/context` 的标题和 `/init`，全局不变。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-07 主控对比 pi、omp 时指出这个缺口，建议另开一件小活。用户：「好，写ast_grep任务文件给我看，CLAUDE.md也排上」
- 11-07 P2-245 派出后，用户：「242看看能不能并行派出去。能的话，并行派出去」。主控查过两件改的文件不重叠。

## 现状（主控查过的）

- 读说明文件在 `crates/canter-core/src/context.rs` 的 `prefix()`（`dir.join("AGENTS.md")` 那两处）：全局读 `<CANTER_HOME>/AGENTS.md`；项目从 Git 仓库根到会话目录，每层读 `AGENTS.md`。每个文件最多 64 KiB，只在打开会话时读（DESIGN 第 17 条缓存规矩）。每段带 `path`，进系统提示词时是 `<project_instructions path="…">`。
- 终端 `/context` 的标题在 `crates/canter-tui/src/context.rs` 第 35–36 行，写死成 `Global AGENTS.md`、`Project AGENTS.md`。
- `/init`：`crates/canter-tui/src/commands.rs` 第 145 行起，提示词 `crates/canter-tui/assets/init_prompt.md`，总是让模型新建或改进 `AGENTS.md`。项目只有 `CLAUDE.md` 时一跑 `/init` 就会新建 `AGENTS.md`，按下面第 1 条，那一层的 `CLAUDE.md` 从此不再被读。
- 用户文档：`docs/usage.md`「Instructions and AGENTS.md」。设计：`docs/DESIGN.md` 第 309 行附近（分段、全局说明）。
- pi 的做法（`/tmp/pi-mono/packages/coding-agent/docs/configuration.md`）：每个目录按 `AGENTS.override.md`、`AGENTS.md`、`AGENTS.MD`、`CLAUDE.md`、`CLAUDE.MD` 找。

## 用户定的（11-07，都照主控建议）

1. 同一层两个都有：只读 `AGENTS.md`，不读 `CLAUDE.md`（两个常是同样内容或软链接，都读会重复）。
2. 全局：不读 Claude Code 的 `~/.claude/CLAUDE.md`（那是别的工具的目录，里面是写给 Claude 的话）；全局仍只读 `<CANTER_HOME>/AGENTS.md`。
3. `CLAUDE.md` 里 Claude Code 的 `@路径` 引入写法：当普通文字，不展开。
4. `/context` 等显示照实写读的是哪个文件名。
5. `/init`（用户选 A）：会话目录只有 `CLAUDE.md`、没有 `AGENTS.md` 时，`/init` 改进 `CLAUDE.md`，不另建 `AGENTS.md`。
6. 子 agent 工具说明里「用户或 AGENTS.md 明确要求」那句（`crates/canter-core/src/agent_tools.rs`）：用户没选，不改。

## 先读

- 项目规矩 `CLAUDE.md`。
- `docs/DESIGN.md` 第 17 条（缓存规矩）、第 309 行附近（分段）。
- 上面「现状」列的代码。

## 在哪里干活

- worktree：`../canter-worktrees/claude-md`，分支 `claude-md`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/claude-md`。
- 只动 `crates/canter-core/src/context.rs`、`crates/canter-tui/src/context.rs`、`crates/canter-tui/assets/init_prompt.md`（和 `/init` 的一句话说明）、受影响的测试、`docs/usage.md`「Instructions and AGENTS.md」一节、`docs/DESIGN.md`。
- 另有 `canter/dev-footer-three` 同时在分支 `footer-three` 改底部状态栏（`crates/canter-tui/src/statusline/`、`crates/canter-tui/src/app.rs`、`docs/usage.md` 状态栏一节）。你不要动这些文件和章节。

## 要做的

1. **项目层**：从 Git 仓库根到会话目录，每一层先找 `AGENTS.md`；这一层没有 `AGENTS.md`、有 `CLAUDE.md` 时读 `CLAUDE.md`。两个都有只读 `AGENTS.md`。大小限制、读不了时的警告、只在打开会话时读，都和 `AGENTS.md` 一样。
2. **全局不变**：只读 `<CANTER_HOME>/AGENTS.md`，不读 `~/.claude/CLAUDE.md`，也不读 `<CANTER_HOME>/CLAUDE.md`。
3. **`@路径` 不展开**：`CLAUDE.md` 原样当文字放进系统提示词。
4. **照实显示文件名**：系统提示词里的 `path` 是实际读到的文件；`/context` 的标题照实写 `Project CLAUDE.md` 或 `Project AGENTS.md`（全局照旧 `Global AGENTS.md`）。屏幕上的字用英文。
5. **`/init`**：提示词改成：会话目录有 `AGENTS.md` 就照旧改进它；没有 `AGENTS.md`、有 `CLAUDE.md` 就改进 `CLAUDE.md`，不新建 `AGENTS.md`；两个都没有就照旧新建 `AGENTS.md`。`/init` 的一句话说明照实改。提示词用英文。
6. **文档**：`docs/usage.md` 写清读法；`docs/DESIGN.md` 记这次改动和用户定的几条。

## 怎么算做完

- 用户要的结果：项目里只有 `CLAUDE.md` 时，canter 能看到里面的项目规矩；`/context` 能看出读的是哪个文件。
- 验证只做这些：
  - 针对性测试：某层只有 `CLAUDE.md` 时读它；同层两个都有只读 `AGENTS.md`；一层 `AGENTS.md`、另一层 `CLAUDE.md` 时两层都读、顺序照旧；全局目录里的 `CLAUDE.md` 不读；`/context` 标题照实写文件名。
  - 改过的 crate 跑全部测试：`cargo nextest run -p canter-core -p canter-tui`。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不读 `AGENTS.override.md`、`AGENTS.MD`、`CLAUDE.MD`、`CLAUDE.local.md`、`.claude/` 目录里的任何东西；不展开 `@路径`。

## 完成记录

- 做了什么：`prefix()`（`crates/canter-core/src/context.rs`）项目层每层先取 `AGENTS.md`，不存在时改取同层的 `CLAUDE.md`；全局仍只取 `<CANTER_HOME>/AGENTS.md`。`/context` 的段标题（`crates/canter-tui/src/context.rs`）改成从段的 `path` 取文件名，照实写 `Project AGENTS.md` / `Project CLAUDE.md`，全局照旧；`path` 是 prefix 实际读到的文件，系统提示词不用改。`/init`（`assets/init_prompt.md`、`commands.rs` 的 about）：有 `AGENTS.md` 改进它，只有 `CLAUDE.md` 就改进它、不新建 `AGENTS.md`，两个都没有才新建。文档改了 `docs/usage.md` 两处和 `docs/DESIGN.md` 第 20 条补一条。任务文件要求的验证照做（核心四条针对性测试 + 两个 crate nextest 全量 + clippy + fmt + git diff --check）。
- 验证了什么：`cargo nextest run -p canter-core -p canter-tui` 971 个全过（含新增的四个 prefix 测试和 /context 的 `Project CLAUDE.md` 标题断言）；先写测试确认两个新行为测试在旧代码上失败（RED）；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（先 fix 过一次缩进）、`git diff --check` 都干净。
- 拿主意的地方：①层选择「有 `AGENTS.md`」用 `Path::exists()`（目录或不可读的情况照旧走读文件报警告的路，不退化去读 `CLAUDE.md`）；②`/context` 里项目段没有 `path` 时标题写 `Project instructions`（正常不会出现，只是不让用行崩）；③`/init` 的一句话说明写成 `Create or improve AGENTS.md or CLAUDE.md for this project`：第一版带 「when no AGENTS.md exists」在 `?` 帮助框 100 列宽下被截断，该测试要求整行配得上，选了缩短说法（更长的原样写进了 `/init` 提示词正文和 usage.md）。
- 没做的事：没改 `agent_tools.rs`、状态栏相关文件（留给 P2-243）；没全局读、没展开 `@路径`；没合并、没推送。
- 踩过的坑：`/init` 说明过字被帮助框截断（已上）；另一个是在同层两文件并存的测试里预期元组类型不匹配的编译错，改闭包返回 `&str` 即好，属笔误不是真坑。
- 不改子 agent 工具说明（`agent_tools.rs`）、信任提示、`--add-dir` 的规矩（加进来的目录照旧不读说明文件）。
- 不改系统提示词的分段顺序和包法（DESIGN 第 17、20 条），只改读哪个文件。
- 不动底部状态栏那些文件（见「在哪里干活」）。
- 不读用户真实的 `~/.canter`、`~/.claude`；测试用临时目录和隔离的 HOME。测试不调真实模型接口。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `claude-md` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
