# 任务：第二期-29.2 - 自定义子agent角色

2026-11-06，canter/main 交给 canter/dev-agent-roles（Codex，`gpt-6-astra` / high）。
路由：中 / 交叉审查要 / 影响面：碰要害（子 agent 的权限和模型路由）。交叉审查按 11-05 起的对比条件交 Claude Code `opus[1m]` / high。
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
2. **派子 agent**：`spawn_agent` 的 `type` 能填自定义角色名；工具说明里列出内置和能用的自定义角色，各带说明，主 agent 按说明挑，用户点名时照用户说的派。现有的「general 只在用户或 AGENTS.md 明确要求委派时开」照旧适用于会改文件的角色。计划模式和审查轮只许开工具全在 explorer 那套之内的角色（含 explorer）。
3. **按角色换模型**：子会话用角色的模型和档位，窗口、压缩线、看图、搜网页、`apply_patch`、计费跟着子会话的模型走，复用现有按会话找那一家的路子。子 agent 的花费照旧计入主会话目标的上限。
4. **权限只能收紧**：沙箱、批准档位、放行规则、文件读写规则、文件夹信任、目标预算都照旧从主会话来，角色改不了。
5. **角色不能用时**（判断不了一律按不能用）：文件读不了、开头写错、名字重复或撞内置、工具名不认识、模型那一家没配：这个角色不能用，其余照常，服务端照常起。派的时候才发现用不了（GPT 没登录、模型不支持这一档）：这次派失败，给主 agent 一句英文原因，不退回别的模型、不放宽权限。
6. **改了文件不用重启**：新增、修改、删除角色文件不用重启服务端。什么时候生效你定，照 P2-154 的轮边界思路，并且不能让同一会话每次请求的工具定义变来变去（第 17 条缓存）；写进 DESIGN。
7. **协议**：角色从固定两种改成名字。旧会话里的 `explorer`、`general` 照样认得，旧终端连新服务端不出错。钩子的 `agent_type`、统计、导出、子 agent 列表和卡片显示角色名。和 Codex 不同的地方写进 DESIGN 第 8 条。
8. **`/roles` 命令**：输入框上方弹框，样子照 `/skills`、`/mcp`。命令说明 `List built-in and custom sub-agent roles`；第一行 `Roles` 加数量摘要；每个角色一行名字，后面写模型（没写就是 `inherits model`）和 `read-only` 或 `can edit`；说明另起一行灰色；能用的名字上色，不能用的红色、下一行写英文原因；分 `Built-in` 和 `Custom`（`~/.canter/agents`）两节；没有自定义角色时提示 `Add roles as ~/.canter/agents/<name>.md`。`/agents` 不动。
9. DESIGN 第 8 条子 agent 那几段、第 15 条（状态目录）写清：文件放哪和写法、能写哪几项、什么时候生效、不能用时怎么办、和 Claude Code、Codex 不同的地方。

## 怎么算做完

- 照用户 11-06 选的：只放全局 `~/.canter/agents/`；角色能指定模型和思考档位；照 Claude Code 的 Markdown 写法；只列出，新加 `/roles` 命令，`/agents` 照旧。
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
