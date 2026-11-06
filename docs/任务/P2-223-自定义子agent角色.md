# 任务：第二期-29.2 - 自定义子agent角色

2026-11-06，canter/main 交给 canter/dev-agent-roles（Codex，`gpt-6-astra` / high）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害）。表里碰要害格是 xhigh，按 11-05 用户定的对比条件用 high。交叉审查按 11-05 起的对比条件交 Claude Code `opus[1m]` / high。
类型：功能变更
依据：子 agent（P2-78、P2-177～P2-180）现在只有内置的 explorer、general 两种角色，写死在协议里（`crates/canter-protocol/src/types.rs` `AgentType`），子会话一律用主会话的模型和档位。本件让用户在 `~/.canter/agents/` 里自己写角色。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
对比样本：第 8 件，按轮流派 Codex，派后 Kimi 6 件 / Codex 2 件（P2-208、P2-219 不算）。用户可以改派。
依赖：P2-208
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-05 用户问第二期还有没有没考虑到的功能，主控列了五条新发现，用户：「这样新发现的1，2，3，4，5都排上。排到gpt任务完成之后（也在第二期）。」
- 11-06 主控讲了 Claude Code（`~/.claude/agents/*.md`）和 Codex（`~/.codex/agents/` 的 TOML）怎么写角色、主 agent 按说明自己挑也能被点名，用户：「我觉得这个可以做。」
- 11-06 主控问四件事，用户都选推荐：
  - 放哪：只放全局 `~/.canter/agents/`；项目里的那一处等第三期 P2-159 项目级配置。
  - 换模型：角色里能指定模型和思考档位。
  - 写法：照 Claude Code 的 Markdown，一个 `.md` 文件，开头几行写名字、说明、工具、模型、档位，正文是提示词。
  - 界面：只列出，新建和修改直接编辑文件。
- 11-06 主控说 canter 已有的 `/agents` 是列正在跑的子 agent，用户选推荐：新加 `/roles` 命令列角色，`/agents` 照旧。
- 11-06 主控讲了三家什么时候能自己派子 agent（Claude Code 都能、Codex 都要明说、canter 只读的能），用户：「选 1，照现在的分法」——只读角色主 agent 能自己派，会改文件的角色要用户或 AGENTS.md 明确要求委派才派。
- 11-06 用户问会不会碰核心逻辑，主控建议加一条保证：没写任何角色文件时，主会话发给模型的内容和现在逐字节一样，子 agent 行为也和现在一样。用户：「加进验收，然后派」。

## 现状（主控查过的）

- 角色：`AgentType { Explorer, General, Unknown }`，存在子会话的 `parent.agentType`（DESIGN 第 8 条 P2-178 那段）。explorer 用计划模式的只读工具（去掉 ask_user、submit_plan，允许 mcp_search，禁止 mcp_call）；general 用全部工具，去掉四个子 agent 工具和 ask_user。计划模式和审查轮只许开 explorer（`crates/canter-core/src/agents.rs` `spawn_agent_of_type`）。
- 模型：子会话的设置整份复制主会话的（同一文件 `settings = ...settings.clone()`）。几家同时配（P2-200）已能按会话的模型找那一家，窗口、压缩线、看图、搜网页、`apply_patch`（P2-205）、套餐计费（P2-217）跟着会话的模型走。
- 用到角色的地方：服务端、钩子的 `agent_type`、统计、导出、终端子 agent 列表和卡片，`grep -rn AgentType crates` 约 20 个文件。
- 技能已经用 `yaml-rust2` 读 `SKILL.md` 开头那几行（`crates/canter-core/src/skills.rs`），可以照着读角色文件。
- 改配置不用重启（P2-154，DESIGN「改配置不用重启」一节）：`config.json` 每轮开始取一份；MCP、LSP 随轮固定。

## 先读

- `docs/DESIGN.md`：第 8 条子 agent 那几段（搜 P2-177、P2-178、P2-183）、「子 agent 的子会话（P2-177）」「子 agent 终端界面（P2-179）」「钩子（P2-153）」里 SubagentStart/SubagentStop、「改配置不用重启（P2-154）」、第 17 条（照顾缓存命中）、第 23 条（核心不绑 DeepSeek）、P2-200 几家同时配、P2-205 apply_patch、P2-206 思考档位。
- 项目规矩 `CLAUDE.md`。
- 只参考、不搬：Claude Code 子 agent 文档 <https://code.claude.com/docs/en/sub-agents>；Codex `rust-v0.163.0` 的 `codex-rs/core/src/agent/role.rs`、`codex-rs/agent-roles/src/`（只读克隆在 `/tmp/codex-p2-205`）。

## 在哪里干活

- worktree：`../canter-worktrees/agent-roles`，分支 `agent-roles`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/agent-roles`。
- 主要动 `crates/canter-protocol/`、`crates/canter-core/`、`crates/canter-server/`、`crates/canter-tui/` 和 `docs/DESIGN.md`；README 不动（P2-228 再写）。

## 要做的

1. **角色文件**：`~/.canter/agents/*.md`（跟着 `CANTER_HOME`），每个文件一个角色：
   ```markdown
   ---
   name: reviewer
   description: Reviews recent changes for bugs. Use after code changes.
   tools: read, grep, shell
   model: someprovider/some-model
   effort: high
   ---
   You are a code reviewer. Only report issues that cause wrong behavior...
   ```
   - `name`、`description` 必填。名字的字符规则你定，写进 DESIGN；不能叫 `explorer`、`general`。
   - `tools` 可选，写 canter 给模型看的工具名；只能是 general 那套工具里的，不写就和 general 一样。四个子 agent 工具和 ask_user 永远没有。
   - `model` 可选，写 `<家名>/<模型名>`；`effort` 可选。都不写时和现在一样用主会话的模型和档位；只写模型不写档位时用那个模型的默认档位。档位字段名照 canter 现有的叫法定，写进 DESIGN。
   - 正文是额外提示词，加在现有子 agent 身份说明后面，不替换。
2. **派子 agent**：`spawn_agent` 的 `type` 能填自定义角色名；工具说明里列出内置和能用的自定义角色，各带说明，主 agent 按说明挑，用户点名时照用户说的派。什么时候能派照现在的分法：工具全在 explorer 那套之内的角色和 explorer 一样，主 agent 能自己开；会改文件的角色和 general 一样，只在用户或 AGENTS.md 明确要求委派时开（AGENTS.md 里写「写测试时交给 test-writer」就算一直授权）。工具说明里写清哪些角色是哪一种。计划模式和审查轮只许开工具全在 explorer 那套之内的角色（含 explorer）。
3. **按角色换模型**：子会话用角色的模型和档位，窗口、压缩线、看图、搜网页、`apply_patch`、计费跟着子会话的模型走，复用现有按会话找那一家的路子。子 agent 的花费照旧计入主会话目标的上限。
4. **权限只能收紧**：沙箱、批准档位、放行规则、文件读写规则、文件夹信任、目标预算都照旧从主会话来，角色改不了。
5. **角色不能用时**（判断不了一律按不能用）：文件读不了、开头写错、名字重复或撞内置、工具名不认识、模型那一家没配：这个角色不能用，其余照常，服务端照常起。派的时候才发现用不了（GPT 没登录、模型不支持这一档）：这次派失败，给主 agent 一句英文原因，不退回别的模型、不放宽权限。
6. **改了文件不用重启**：新增、修改、删除角色文件不用重启服务端。什么时候生效你定，照 P2-154 的轮边界思路，并且不能让同一会话每次请求的工具定义变来变去（第 17 条缓存）；写进 DESIGN。
7. **协议**：角色从固定两种改成名字。旧会话里的 `explorer`、`general` 照样认得，旧终端连新服务端不出错。钩子的 `agent_type`、统计、导出、子 agent 列表和卡片显示角色名。和 Codex 不同的地方写进 DESIGN 第 8 条。
8. **`/roles` 命令**：输入框上方弹框，样子照 `/skills`、`/mcp`。命令说明 `List built-in and custom sub-agent roles`；第一行 `Roles` 加数量摘要；每个角色一行名字，后面写模型（没写就是 `inherits model`）和 `read-only` 或 `can edit`；说明另起一行灰色；能用的名字上色，不能用的红色、下一行写英文原因；分 `Built-in` 和 `Custom`（`~/.canter/agents`）两节；没有自定义角色时提示 `Add roles as ~/.canter/agents/<name>.md`。`/agents` 不动。
9. DESIGN 第 8 条子 agent 那几段、第 15 条（状态目录）写清：文件放哪和写法、能写哪几项、什么时候生效、不能用时怎么办、和 Claude Code、Codex 不同的地方。

## 怎么算做完

- 照用户 11-06 选的：只放全局 `~/.canter/agents/`；角色能指定模型和思考档位；照 Claude Code 的 Markdown 写法；只列出，新加 `/roles` 命令，`/agents` 照旧。
- 照用户 11-06 选的：只读角色能自己派，会改文件的角色要明说。
- 照用户 11-06 加的：没写任何角色文件时，主会话发给模型的内容（系统提示词、工具定义含 `spawn_agent` 说明、历史）和现在逐字节一样，子 agent 行为也和现在一样。用一条逐字节比对的测试证明（先在 main 上录下现在的请求，再比）。
- 验证只做这些：
  - 每处一条针对它的测试（先确认现在的行为让它失败）：读到自定义角色并能派；子会话用角色的模型和档位、不写时用主会话的；工具只剩写的那些；计划模式下能开只读角色、不能开会改文件的；角色放宽不了沙箱和批准档位；坏文件、撞名、不认识的工具、没配的家标成不能用且不影响其他；派时模型用不了就失败不退回；改文件不重启生效；旧会话的 explorer/general 照常打开；`/roles` 弹框显示。
  - 测试用本机假服务端回放，两家假的提供方（一家 Chat Completions、一家 Responses），不调真实接口。
  - 改到的包跑全部测试：`cargo nextest run -p canter-protocol -p canter-core -p canter-server -p canter-tui -p canter`（`canter-server`、`canter` 里有逐字节比对请求和工具定义的测试），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不做项目里的角色（`.canter/agents/`），不做插件带来的角色，不在界面里新建、修改、删除角色，不做让模型起草角色。
- 不改内置 explorer、general 的工具和规矩；不放开子 agent 再开子 agent、不改同时最多 4 个。
- 第二期核心不绑 DeepSeek：不按 DeepSeek 的名字、地址、请求字段、缓存行为分支；只有某一家才有的能力（`apply_patch`、套餐）照现有开关和退路走。DeepSeek 这些别家的测试一条都不许改，碰到了停下来问。
- 不碰用户真实的 `~/.canter`：测试用隔离的 `CANTER_HOME`、临时端口和 socket。不调真实的模型接口。
- 界面上的字一律英文。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（和现有协议或 P2-154 规矩冲突、上面没写到的取舍），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `agent-roles` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-11-06，canter/dev-agent-roles，在 `agent-roles` 分支完成。

- **做了什么**：读取服务端 CANTER_HOME 下 `agents/*.md` 的五个字段和额外提示词，逐文件校验并展示不可用原因；父会话轮边界刷新目录，轮内固定派发说明。自定义角色沿用子会话的权限、目标预算和模型路由，限制工具定义及实际调用；支持角色模型和档位、只读分类、派发前拒绝未登录、无效档位及目标模型不提供的工具。协议保存角色名字，旧内置值和旧客户端 Unknown 兼容；新增只读角色接口及 `/roles` 弹框，列表、卡片、导出、统计和钩子沿用角色名字。已更新 DESIGN 和协议生成文件。
- **主控决定和其他取舍**：主控明确选择 A：派出时把有效工具、额外提示词、模型、档位及只读分类保存进子会话私有历史；已有子会话继续跑、关闭后重开都用快照，改文件或删文件只影响以后新派的子 agent。理由是权限不会因改文件中途变化，也与原有子 agent 保留派出时模型一致；已写入 DESIGN 第 8、15 条及自定义角色小节。名字定为 1～64 个小写 ASCII 字母、数字或连字符、字母开头，保留 explorer/general/unknown；单文件上限 64 KiB，未知字段禁用文件。没有待主控决定的问题。
- **针对性验证**：新增 8 条 core 集成测试，覆盖文件解析与派发、工具定义和执行限制、计划模式、权限继承、坏文件隔离、文件刷新及删除后重开快照、模型档位继承和覆盖、无角色请求字节基线。补充协议新旧角色兼容、服务端角色列表、Chat Completions 父会话到 Responses 子会话及未登录拒绝、TUI 弹框的检查。实际记录过的 RED 包括旧实现拒绝自定义派发、角色接口不存在、命令未注册、未登录仍允许派出、目标模型没有工具仍允许派出，以及命令排序错误，修复后对应检查通过。帮助和斜杠菜单的 69 项回归全部通过；清单增加 `/roles`，整页帮助测试高度随新增命令增加一行，没有删除或放宽原断言。
- **逐字节基线**：改生产代码前在 main 同提交 `2a6d466956a01b95f8f95b8b39dd0f8696f10afd` 上录下两轮实际请求，保存为 `crates/canter-core/tests/fixtures/p2-223-main-requests.jsonl`。只归一化临时根目录、scratch 路径、项目路径哈希、开局日期和平台；完整系统提示、工具定义（含 spawn_agent）、历史和请求结构逐字节比较，未用新实现重新录制期望值，最终回归通过。
- **任务指定检查**：前台跑完 `cargo nextest run -p canter-protocol -p canter-core -p canter-server -p canter-tui -p canter --no-fail-fast --status-level fail --final-status-level fail`，最终整轮 1590 项执行、1589 通过、1 失败、27 跳过；唯一失败是未修改的 `lsp_tests::sigkill_of_server_removes_lsp_process_group` 读取空 PID 文件，单独原样重跑通过（上一整轮也通过）。本任务新增测试及修复后的 TUI 回归均通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。全部使用任务独立编译目录，没有跑额外 workspace 全套或真实模型接口。
- **踩过的坑**：字节基线含随机 scratch、项目哈希和 macOS `/private` 路径别名，补齐纯环境归一化后稳定比较。首次包回归发现协议生成物遗漏，使用现有导出程序更新；新增命令暴露了排序、旧清单数量和整页帮助高度问题，修正后 69 项通过。两次包回归分别撞到既有 suggestions 的 git PID 和 LSP instance PID 文件“已创建但未写完”的竞态，均原样单独重跑通过，没有改这两处测试。自查发现显式列出目标模型不提供的工具会产生空工具子会话，先补失败断言，再改为派出前报错。
- **没做的事和参考**：按范围不做项目角色、角色编辑界面、插件角色或嵌套派发；未改 README，未改 DeepSeek 等现有提供方测试，未触碰用户真实 `~/.canter`，未安装、合并或推送。只参考 Claude Code 文档 <https://code.claude.com/docs/en/sub-agents> 和 `/tmp/codex-p2-205` 的 `rust-v0.163.0` 源码 `codex-rs/core/src/agent/role.rs`、`codex-rs/agent-roles/src/agent_role_config.rs`，没有搬代码。模型和登录测试均使用隔离目录与本机假服务端，真实 API 调用 **0 次**。

## 主控审查

- 谁做：canter/dev-agent-roles-1，Codex `gpt-6-astra` / high；用时约 40 分钟（corral 开起到 DONE），中途停下问 1 次（角色文件改了或删了，已派出的子会话怎么办），主控定 A：派出时存快照，之后不重读文件。
- 范围：动了 protocol、core、server、tui 和 DESIGN，`/agents` 没动，没做项目角色、编辑界面、插件角色；「不要做」没碰。界面新增文字都是英文。
- 看了派子 agent 的路径：自定义角色先校验模型、档位、工具，再存快照；只读的判断是工具全在 explorer 那套之内；子会话执行时按快照的工具清单拦，快照丢了的自定义子会话一个工具都不给。
- 核对：新增角色测试 core/protocol 28 条、server/tui 3 条重跑全过；全套留到合并前。没写角色时请求逐字节不变的基线是在 main `2a6d466` 上录的，只归一化了临时路径、项目哈希、日期和平台。

### 交叉审查返工（2026-11-06）

按主控要求只处理审查意见第 1、2 条；主仓库 `docs/任务/P2-223-交叉审查.md` 只读，第 3～5 条未改。

- 第 1 条：`roles::load` 对显式 `model` 增加 `options.model_limits` 存在性检查；提供方已配而模型未配时，列为不可用，返回 `Model is not configured: <model>`。派出前使用当前 runtime 的同一份 options 再检查，失败立即返回，不创建子会话、不发请求、不回落模型。DESIGN 同步说明未配置模型也不可用。快照重开测试给 `saved/original`、`saved/changed` 补齐模型 limits；坏角色测试新增 `configured/typo`，同时断言不可用、派不出、请求数不增加。
- 第 2 条：在现有计划模式测试后半段关闭父会话计划模式并设 `allowAll`，再派 `tools: read, shell` 的只读角色，让假模型调用 `touch forbidden-readonly-shell`。确认子会话继承 `allowAll` 且没有开启计划模式、shell 在工具定义内，但写调用仍被拒绝，文件未生成。没有增加测试矩阵或修改权限执行逻辑。
- 验证：先运行坏角色测试，确认旧实现返回 `error: None` 而非预期模型未配置错误（RED）；修复后通过。最终 `cargo nextest run -p canter-core --test roles` 8/8 通过；server 定向运行 `roles_endpoint_lists_builtins_and_custom_roles` 和 `custom_role_routes_to_responses_and_missing_login_fails_without_fallback`，2/2 通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。均使用 `.target/agent-roles`，命令在前台跑完，未重跑五包全量，真实 API 调用 0 次。
- 返工中的调整：shell 拒绝沿用现有只读路径的 `Plan mode: shell is not allowed`，不是普通批准拒绝的 `Tool call declined.`；测试按实际拒绝原因断言，并保留文件未生成、非计划模式和 allowAll 的检查。没有新的待主控决定事项。
- 交叉审查（Claude Code opus[1m] / high）：必须改 1 条（角色写的模型只查了家、没查模型本身配没配，派出后窗口、档位、价格走全局退路），建议改 1 条（只读角色在 allowAll 下拒写缺测试），一起交回；可以不改 3 条记进 HANDOFF。返工约 15 分钟（`4da11cb`），复核可以合并，无新问题。
- 结果：返工 1 次
