# 任务：第二期 - Sub-agents 框每行加「深度」和「任务」两列，任务可弹框看全文

2026-11-08 写好，canter/main 交给 canter/dev-agent-depth-task（Claude Code，`opus[1m]` / xhigh，高档界面和设计格）。
路由：重 / 交叉审查不要 / 影响面：改行为（路由：重（置信 0.88）、拿不准、拿不准；主控判：协议只加可选字段，不碰并发、安全和数据迁移，按改行为、不交叉审查）。
类型：功能变更
依据：P2-248「主动开只读子 agent 并行调研」的后续。本轮给 `spawn_agent` 加 `depth` 参数，在 Sub-agents 框显示深度和任务、弹框看全文；不动子 agent 的调度、等待和叫醒。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-08 用户（找不到派给子 agent 的完整任务后）：「不能写到最下面sub-agents中每一个subagent所在的行吗？只是加一列 所谓的排出去的深度。然后全文任务再加一列，点击之后使用对话框展开。这么设计不行吗？」
- 主控提议：`spawn_agent` 加一个必填的 `depth` 参数（quick / medium / thorough），服务端把对应说明加进子 agent 的任务，界面直接显示；选中一行按 `t` 或鼠标点任务那一列，弹框显示全文；Enter 仍是打开子 agent 的会话。附了下面「样子」里的两张示意。用户：「可以」。
- 11-08 用户：「写P2-255的任务文件吧」
- 11-08 用户：「是不是先出设计给我看，chrome」。主控出了线框 `docs/任务/P2-255-线框.html`（4 个问题各两种样子，标了推荐），用户：「都按推荐」。

## 现状（主控查过的，main `a594c46`）

- **深度现在只在任务正文里**：`spawn_agent` 的参数是 `task`、`type`、`name`（`crates/canter-core/src/agent_tools.rs` 的 `AgentCommand::SpawnAgent` 和 `definitions()`）。说明里让模型「say how deep to look: quick (…), medium (…), thorough (…)」，模型自己写进任务正文，写法不固定：GPT 写过 `medium depth — cover these questions and then stop`，DeepSeek 写过中文「深度：medium」。所以现在没法可靠地拆成一列。
- 同样的说法还出现在：带自定义角色的那份说明（`definitions_for_roles()`）、系统提示词 `crates/canter-core/src/prompt/core.txt` 第 20 行 `[spawn_agent]`。这些文字原样出现在请求样本里：`crates/canter/tests/fixtures/no_mcp_request.json`、`crates/canter-core/tests/sandbox-disabled-request.json`、`crates/canter-core/tests/fixtures/p2-223-main-requests.jsonl`。
- **服务端开子 agent**：`crates/canter-server/src/agent_sessions.rs` 收到 `SpawnAgent` 后调 `spawn_agent_of_type(parent, task, kind, name)`。
- **协议**：`AgentSummary`（`crates/canter-protocol/src/types.rs` 约 390 行）已经有 `task`、`name`、`agentType`、状态和时间，父会话通过 `agent.updated` 推给客户端（DESIGN P2-179）。没有深度字段。
- **Sub-agents 框**：`crates/canter-tui/src/agents.rs`。
  - `rows()` 画框，`Tree::row()` 画每一行：树线、状态记号、名字、类型、状态、用时。
  - 宽度不够时先缩名字，名字最少留 12 列（`MIN_NAME`）。
  - 按键在 `crates/canter-tui/src/app.rs` 的 `agents_key()`（约 2884 行）：↑↓ 选、Enter 打开子会话、`s` 停掉在跑的子 agent、Esc 收起列表。
  - 鼠标在 `agents_click()`（约 2943 行）：点一行和 Enter 一样，打开那个会话。
  - 第一行 `Main session · <标题>` 是主会话，不是子 agent。
- 对话里 `Spawned` 卡片按 Ctrl+O 显示任务全文（11-08 已做），本件不动。

## 样子（以线框 `docs/任务/P2-255-线框.html` 里标「推荐」的样子为准，用户 11-08「都按推荐」）

- 深度和任务各占一列，一个子 agent 一行；深度颜色 quick 灰、medium 黄、thorough 品红，没有深度的显示灰色 `—`。
- 窄终端先缩任务列，剩不到 10 列整列不显示。
- 弹框盖在对话区上面，下面的列表仍看得到。
- 新加 `t`、点任务列打开弹框，Enter 和点一行其它位置照旧打开会话。

框里每行，界面文字用英文：

```
╭ Sub-agents · 1 running · 1 done ──────────────────────────────────────────────────────╮
│   Main session · 对比 /tmp/codex 和 /tmp/pi-mono …                              (here) │
│ › ├ ● codex tool timeout study   explorer  medium  running  8m 52s  只研究 /tmp/codex 这… │
│   └ ✓ pi-mono timeout study      explorer  medium  done     4m 30s  只研究 /tmp/pi-mono … │
╰─────────────────────────── ↑↓ select · Enter open · t task · s stop · Esc close ╯
```

选中一行按 `t`，或鼠标点这一行的任务那一列，弹框：

```
╭ Task · codex tool timeout study · medium ──────────────╮
│ 只研究 /tmp/codex 这个项目（Rust 里的 codex CLI…）是如何 │
│ 处理「工具调用超时」的。不要看 /tmp/pi-mono。             │
│ 要回答三个问题，每条给出 file:line 证据：                 │
│ …                                                       │
╰──────────────────────────────── ↑↓ scroll · Esc close ╯
```

## 主控定的

1. **`depth` 参数**：`spawn_agent` 加必填参数 `depth`，只能是 `quick`、`medium`、`thorough`。
   - 每种子 agent 都要填，包括 general 和自定义角色。
   - 服务端把一句英文说明接在模型写的任务后面交给子 agent：quick 找到主要位置就报告；medium 每个问题给 file:line 证据，然后停；thorough 照任务要求查全。措辞你定。
   - 工具说明和系统提示词里，「say how deep to look」那段改成「设 `depth`」：medium 是默认，thorough 只在用户要求查全时用。不再让模型把深度写进任务正文。
2. **协议**：`AgentSummary` 加可选的 `depth`。旧服务端没有这个字段，旧会话里已有的子 agent 也没有，界面上这一列显示 `—`。只加字段，不改已有字段的意思。
3. **任务列**：显示模型写的任务（不含服务端接上的那句深度说明），只显示第一行，截到能放下的宽度。弹框也显示模型写的任务全文，深度放在弹框标题里。
4. **宽度不够时**：先缩任务列，任务列少于 10 列就整列不显示；然后照现在的样子缩名字。深度列宽度固定，和类型列一样一直显示。
5. **按键和鼠标**：
   - `t`：选中的是子 agent 时打开弹框；选中的是 `Main session` 那一行时不做事。
   - 鼠标点子 agent 行的任务那一列：打开弹框。点这一行的其它位置：照现在的样子打开那个会话。
   - 弹框里：↑↓、PgUp、PgDn 滚动；Esc 或 `t` 关掉弹框，回到列表，选中的还是原来那一行。
   - **弹框里的 Esc 只关弹框**：不收起列表，不打断主会话的这一轮，不停任何子 agent。
   - 框底部的按键提示加上 `t task`。
6. **范围**：只改 Sub-agents 框。对话里的 `Spawned` 卡片、`/export` 不加深度。

## 要做的

1. 照「主控定的」第 1、2 条改 `spawn_agent` 的参数、服务端开子 agent、协议，以及工具说明和系统提示词；请求样本照新文字更新。
2. 照第 3～5 条改 Sub-agents 框：加深度列、任务列、`t` 键、点任务列、弹框。
3. `docs/DESIGN.md`：
   - 协议一节（P2-179 那段附近）写 `AgentSummary.depth` 和 `spawn_agent.depth`。照规矩写清楚和 Codex `CollabAgentToolCall` / spawn 参数比加了什么、为什么。
   - 子 agent 终端界面一节写新的两列和弹框。
   - 都注明 P2-255 和用户原话。

## 怎么算做完

- 用户原话的要求：Sub-agents 框每个子 agent 那一行多一列派出去的深度，再多一列任务；点任务那一列，用弹框展开全文。
- 主控定的第 1～6 条都做到了：`t` 键能打开弹框，弹框里的 Esc 只关弹框，窄终端下任务列先让出位置，旧子 agent 的深度显示 `—`。
- 验证只做这些：
  - 针对这件活的测试：先写测试，确认失败，再改。至少覆盖：
    - `spawn_agent` 不带 `depth` 被拒，带了以后子 agent 收到的任务末尾有对应说明；
    - 框里显示深度和任务两列，窄宽度下任务列先缩、再整列不显示；
    - `t` 和点任务列都能打开弹框；弹框里 Esc 只关弹框，主会话这一轮照跑、子 agent 没停；
    - 没有 `depth` 的旧列表项显示 `—`。
  - 改了哪个包，就跑那个包的全部测试：`cargo nextest run -p <包> --all-targets`。至少跑 `canter-protocol`、`canter-core`、`canter-server`、`canter`、`canter-tui`。`canter` 和 `canter-server` 有请求回放和样本测试，工具说明一变，那里会失败。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-255-agent-depth-task`。
  - 不录屏，不截图。觉得不够，在回复里说，不要自己加。

## 在哪里干活

- worktree：`../canter-worktrees/p2-255-agent-depth-task`，分支 `p2-255-agent-depth-task`（已从 main 建好）。
- 只动 `crates/canter-protocol`、`crates/canter-core`、`crates/canter-server`、`crates/canter-tui`、上面列的三个请求样本、`docs/DESIGN.md` 和本任务文件。协议改了，照仓库现有方式更新生成的类型（如果有）。

## 不要做

- 不改子 agent 的调度、`wait_agent`、叫醒和合并（P2-101、P2-178、11-08 被叫醒不干等那处兜底）。
- 不改 Esc 在主会话里连带停子 agent 的行为（HANDOFF 已记，用户说先不做）。
- 不改对话里的 `Spawned` 卡片和 `/export`。
- 界面文字一律英文，不出现中文。
- 核心逻辑不按模型名、`base_url`、请求字段或缓存行为分支，不绑定 DeepSeek。
- 测试不调真实的模型接口，用假服务端回放。不用 Python。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录。停自己起的服务用起的时候记下的 PID。
- 碰到要拿主意的事（比如框太窄、弹框和别的弹出层同时出现、自定义角色有自己的说明时深度那句放哪），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-255-agent-depth-task` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-11-08，canter/dev-agent-depth-task（Claude Code opus[1m] xhigh）。

### 做了什么

- 协议：新增 `AgentDepth`（`quick` / `medium` / `thorough`，另有 `unknown` 接以后的新值，照协议里别的枚举）；`SessionParent` 和 `AgentSummary` 各加可选 `depth`，`AgentSummary::from_snapshot` 从 `parent.depth` 取。重新生成了 `generated/`（多了 `AgentDepth.ts`）。
- 核心：`spawn_agent` 加必填 `depth`（schema 里 `required: task, type, depth, name`），缺了是 serde 的 `missing field depth`，别的值报 `depth must be quick, medium, or thorough`，都不开子 agent。`Session::spawn_agent_of_type` 加 `depth: Option<AgentDepth>`：存进子会话的 `parent.depth`，子会话第一条输入是「模型的任务 + 空行 + 一句英文」，`parent.task` 仍是模型的任务本身。两份工具说明和 `core.txt` 那一行改成 `set depth instead of writing it into the task: …`。服务端模型工具宿主把 `Some(depth)` 传下去。
- 界面：`/agents` 每行在类型后加深度列（quick 暗、medium 黄、thorough 品红，没有为暗 `—`），用时后加任务列（第一行有字的那行，暗色，截断带 `…`）；先缩任务列，少于 10 列整列不显示，再缩名字。`t` 或点任务列开任务框，盖在对话区最下面，标题 `Task · 名字 · 深度`，↑↓/PgUp/PgDn 滚，Esc 或 `t` 只关框。底部提示加 `t task`。`Report` 加了 `page_up` / `page_down`。
- 请求样本三份照新文字和新 schema 更新；`docs/DESIGN.md` 协议一节（P2-179 前面）、模型工具一节（P2-248 后面）、子 agent 终端界面一节（卡片后面）各加一段。
- 返工（主控 11-08 定，用户「可以，就这么定」）：深度那句只接在只读子 agent（explorer、只读的自定义角色）的任务后面；能改文件的（general、能改文件的自定义角色）照样必填 `depth`、列表照样显示，但任务原样交给它，因为 medium 那句是调研口气，接在「把 X 改好」后面可能让它只汇报或改到一半就停。改在核心 `spawn_agent_of_type` 用已有的只读判断；工具说明和系统提示词里没有「接在任务后面」的话，没改，请求样本也不用动；DESIGN 三处写明只对只读的。

### 验证了什么

- 先写测试再改：协议、核心的新测试先因为没有 `depth` 编译不过；服务端 `model_spawn_needs_a_depth_and_the_child_hears_it_after_the_task` 在去掉接那句的情况下失败（子 agent 收到的只有任务本身）；新的界面测试 `t_or_a_click_on_the_task_opens_it_in_a_box_that_esc_alone_closes` 在 main 的界面代码上失败（找不到深度和任务列）。
- 新测试覆盖：不带 `depth` 被拒且不开子 agent、带了以后子 agent 最后一条用户消息是「任务 + 空行 + medium 那句」、列表项 `depth` 和 `task`（不含那句）；三种类型（explorer、general、自定义角色）都要填、填错被拒；框里两列和颜色、`—`、窄宽度下任务列先缩再整列不显示再缩名字；`t` 和点任务列开框、PgUp/PgDn/↑↓ 滚、Esc 和 `t` 只关框（主会话这一轮仍是 Working、没有返回打断或停子 agent 的动作、没发任何请求）、`Main session` 那行 `t` 不做事、点别的行的任务列换内容、点行的其它位置照旧打开会话。
- 整包：`canter-protocol` + `canter-store` + `canter-core` 365 过（23 个默认跳过）；`canter-server` 418 过（5 个默认跳过）；`canter` 229 过；`canter-tui` 721 过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 干净。
- 返工：新测试 `an_editing_child_gets_its_task_without_the_depth_sentence`（general 子 agent 带 `depth: medium`，收到的任务就是任务本身，列表仍显示 medium）先在旧代码上失败（收到的带着 medium 那句），改后通过；explorer 那条 `model_spawn_needs_a_depth_and_the_child_hears_it_after_the_task` 仍要求带那句。返工只跑了这两条和 `canter-core` 的 `agent_tools`、`agents`、`roles` 测试，加 clippy、fmt、diff-check，没再跑整包。
- 没调真实模型接口（0 次），没录屏、没截图。

### 拿主意的地方

- 深度那三句英文：quick `Find the main places, report them, and stop.`，medium `Answer each question with file:line evidence, then stop.`，thorough `Cover everything the task asks for before you report.`，都以 `Depth: <值>.` 开头，空一行接在任务后面。
- 自定义角色：那句照第 1 条接在任务后面（子会话的第一条用户消息），角色自己的说明仍在系统提示词里，两处不冲突，没有停下来问。（返工后只对只读的自定义角色接，见「做了什么」最后一条。）
- 深度存在 `SessionParent` 上（随 `session.parent` 存盘），列表项从这里取；核心接口用 `Option`，只有测试里的旧入口传 `None`，模型工具总是有值。SubagentStart 钩子的 `task` 仍是模型写的任务。
- 深度列宽按各行最宽的值定（和类型列一样），全是旧子 agent 时只有一列 `—` 宽。
- 任务框沿用 `/context` 等报告框（上下各一行空、`↑ N more` / `↓ N more`），左右各留 2 列（窗口窄于 40 列不留），放在对话区最下面几行。Ctrl+C、Ctrl+D 在框里和 Esc 一样只关框；Enter、`s` 在框里不做事；点框里面不做事（不让底下看不见的对话被选字或开链接）；点任务列会把那一行设为选中。批准框出来时列表本来就不画，框跟着不画。
- 越出「只动」范围的一处：`crates/canter-store/tests/agents.rs` 构造 `SessionParent` 字面量，加字段后必须补一行 `depth: None` 才编译得过；没别的办法，已跑 `canter-store` 全部测试。

### 没做的事

- 鼠标滚轮在框上滚的仍是底下的对话，不滚框（只按要求做了键盘滚动）。
- 框开着时状态行仍写 `Esc to interrupt`，和列表开着时一样，没改。
- 终端太矮、框画不出来时，按键照样先给框（Esc 关掉它）。
- `Spawned` 卡片、`/export`、调度、`wait_agent`、叫醒、主会话 Esc 连带停子 agent 都没动；`crates/canter-protocol/README.md` 没写（协议说明只写进 DESIGN）。

### 踩过的坑

- core `agents.rs` 那段在很长的 `crate::session::owned(async move { … })` 里，rustfmt 不格式化，内联的 `match` 很难看；改成先算 `let first` 再传。
- 新界面测试里「往下滚一行后第一行看不到了」一开始失败：列表那一行的任务列显示的正是同一句。改成只认框里带左边框的 `│ ` 那一行。

## 主控审查

- 开发：canter/dev-agent-depth-task-1（Claude Code，`opus[1m]` / xhigh），第一轮约 32 分钟，没有停下来问。用户在开发中提出「能改文件的子 agent 也会收到查完就停」，主控定、用户同意「可以，就这么定」：深度那句只接给只读子 agent，交回改了一次（`26e82b1`），这不是未达标返工。
- 看了 diff：协议只加可选 `depth`；只读子 agent 收到「任务 + 空行 + 深度那句」，能改文件的原样；列表和弹框显示模型写的原文；弹框里的按键排在打断主会话的处理之前，Esc 只关弹框。越出「只动」范围的 `canter-store` 测试补一行 `depth: None` 是编译所需，认可。
- 合并前全套：`cargo nextest run --all-targets` 2023 个全过，`cargo test --doc --workspace` 通过。
- 开发记下没做的三处（状态行仍写 `Esc to interrupt`、滚轮滚的是底下对话、终端太矮时按键仍给弹框）记进 HANDOFF，等用户试用后定。
- 结果：一次过
