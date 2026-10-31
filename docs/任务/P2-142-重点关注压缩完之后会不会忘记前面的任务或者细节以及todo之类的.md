# 任务：第二期-6 - 压缩后自动带回最近读过的文件

2026-10-31，canter/main 交给 canter/dev-restore（Codex，gpt-6-astra / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：第二期第 142 件。10-31 两次试跑表明压缩后不忘事，但摘要只记文件名，模型压缩后要自己重读文件。本件照 Claude Code：压缩完自动把最近读过或改过的文件带回来。不改压缩摘要本身。
执行：派出

## 用户原话

- 10-29 看板标题：「重点关注压缩完之后会不会忘记前面的任务或者细节以及todo之类的」，附了 Claude Code 压缩后重新读取的显示（见文末）。
- 10-29：「整体退回也排到第三期，压缩好几次以后会不会走样这个要排到第二期。」
- 10-31 看完两次试跑：「142这个我记得claude code压缩完也是会读取压缩前的文件」；主控说明 Claude Code 带回最近读过的文件（约 5 个、总共约 5 万 token、单个约 5 千 token，超大的只给文件名）后，用户：「写吧，照 Claude Code 的来」。

## 试跑（10-31，主控记）

- canter-test-rust，压缩线 150000：12 件全做完，压缩 1 次，没丢东西；`read` 调用 30 次。
- canter-test-ts，压缩线 60000：12 件全做完，压缩 15 次。中途插的三句话（提交末行 `Task: N/12`、第 12 件的确认提示原文、README 加示例）都守住了，确认提示原文隔了 12 次压缩照样一字不差。每次压缩后重读 3～17 个文件，`read` 调用 158 次；没命中缓存的输入 81 万，是上一次的 4 倍多。
- 结论：摘要守得住任务、细节、待办和用户中途的要求；缺的是文件内容，压缩后模型只能重读。

## 先读

- `docs/DESIGN.md`：第 13 条（事件和模型历史两份）、第 17 条（照顾缓存命中，压缩是例外）、计划和待办（P2-34）里压缩附清单那段、用户点名用技能（P2-129）里压缩后补回技能那段、文件规则（P2-167）、遥测查看（P2-170）的三个信号。
- `crates/canter-core/src/compaction.rs` 的 `compact`：压缩成功后依次 `restate_plan_mode`、`restate_skills`、`restate_goal`、`restate_directories`。带回文件照这个路子加。
- `crates/canter-core/src/skill_invocation.rs` 的 `restate_skills`：补回的内容怎么记进模型历史、存盘重开怎么重放。
- 文件规则怎么判断：`crates/canter-tools/src/rules.rs`、`permission.rs`。
- 参考 Claude Code 的行为：文末用户贴的显示。Claude Code 不开源，不用去找源码。

## 在哪里干活

- worktree：`../canter-worktrees/p2-142-restore-files`，分支 `p2-142-restore-files`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-142-restore-files`。

## 要做的

1. **记下用过哪些文件**：会话里模型成功 `read`、`write`、`edit` 过的文本文件，记路径和最后一次用到的先后。跨压缩累计，被回退、撤回掉的那几轮里的使用不算。服务端重启、重开会话后可以从空开始记。
2. **压缩成功后带回**（照 Claude Code）：
   - 按最后一次用到的先后，取最近的 **5 个**文件，**从硬盘重新读现在的内容**，不用压缩前那份旧的。
   - 单个文件超过 **5 千 token**（用现有的上下文估算口径），只给文件名，不带内容。
   - 带内容的合计不超过 **5 万 token**。
   - 不带：已经删掉或读不了的、图片和其他非文本、已经整份在系统提示词里的（全局和项目的 `AGENTS.md`）。
   - 文件规则当下再判一次：规则禁止读的，连名字也不带；规则要问的，只给名字，不带内容（压缩时不弹框）。
   - 带回的内容给模型时写清这是压缩后自动重读的哪个文件、第几行到第几行，格式和 `read` 工具给模型的结果一致，模型拿来就能接着 `edit`。
   - 只在压缩成功时做；失败、被打断的压缩不带。
3. **进模型历史，存盘重开一致**：照 `restate_skills` 的路子记进模型历史，重开会话、服务端重启后重放出同样的上下文。下一次再压缩时，上一次带回的内容不当成用户原话。
4. **界面显示**（照 Claude Code）：压缩卡片下面列出带回了什么，每个文件一行。带了内容的写 `Read <path> (<N> lines)`，只给名字的写 `Referenced file <path>`。路径相对会话目录，在目录外的写绝对路径。协议给压缩项目加字段，旧终端忽略；和 Codex v2 不同的地方写进 DESIGN。
5. **遥测**：P2-170 的「压缩后重读的文件」信号照旧只看模型自己的 `read`，自动带回的不算重读。这样改之前和改之后的试跑可以直接对比。

终端上显示的文字都用英文。

## 怎么算做完

- 用户原话：「照 Claude Code 的来」：压缩完把最近读过或改过的文件带回来，最多 5 个、合计 5 万 token、单个 5 千 token，超大的只给文件名；压缩卡片列出带回了哪些。
- 验证只做这些：每条行为一条针对性测试（先写测试、确认因为功能缺失而失败），加上这些边角用例：
  - 文件规则禁止的不带，要问的只给名字；
  - 文件改过以后带回的是新内容，删掉的不带；
  - 超过 5 个、超过单个和合计上限时怎么取舍；
  - 回退、撤回掉的使用不算；
  - 存盘重开后模型历史一致；
  - 连续压缩两次，第二次不把第一次带回的内容当用户原话。

  测试用本机假模型服务端和隔离的 HOME、临时状态目录。再跑直接相关的已有测试，加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改压缩摘要的提示词和 9 个小节，不改压缩线和触发时机。
- 不改模型自己用 `read` 的行为和权限判断；带回时不弹批准框。
- 不带回 shell 命令的输出、MCP 结果、网页内容。
- 测试不调真实模型接口；不读、不改用户真实的 `~/.canter` 和会话。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-142-restore-files` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 附：用户 10-29 贴的 Claude Code 压缩后显示

```
Read docs/任务/P2-136-LSP接第一批语言.md (60 lines)
Referenced file ../canter-worktrees/p2-135-lsp-tui/docs/DESIGN.md
Read ../../../../../private/tmp/claude-501/…/scratchpad/p2-135-checks2.txt (17 lines)
Referenced file ../canter-worktrees/p2-135-lsp-tui/crates/canter-core/tests/lsp.rs
Read ../../../../../private/tmp/claude-501/…/scratchpad/p2-135-checks.txt (12 lines)
Skills restored (corral-dispatch)
```

## 主控记下的（10-29）

- 10-29 主控为长程任务又过了一遍；用户：「整体退回也排到第三期，压缩好几次以后会不会走样这个要排到第二期。」并进本件：长任务会压缩不止一次，摘要的摘要可能越压越偏。本件要专门看连续压缩两三次以后，任务、细节、待办还在不在。
- 要看的东西参考 P2-81「压缩后不丢东西」那一节：用户中途定的规矩、试过失败的办法、改过哪些文件和为什么、后台命令的编号和在等什么。
- 10-31 两次试跑（见上）回答了这一条：连续压缩 15 次没走样，本件改做带回文件。

## 完成记录

- 做了什么：成功的 read/write/edit 按所属轮累计文件使用顺序，压缩成功后取最近最多 5 个可带回文件，从磁盘重读当前内容。按真实路径去重，目录内显示相对路径、目录外显示绝对路径；回退或撤回掉的轮不参与选择，重开后使用记录从空开始。删除、读不了、非文本、图片及已经整份放进系统提示词的全局和项目 AGENTS.md 排除。复用工具的路径检查、执行许可和当前文件规则，forbidden 完全排除，prompt 即使在 Allow all 下也只给名字，不发批准请求。普通模型 read 的行为及权限判断未改。
- 模型历史和界面：新增私有 restored_file_notice，保存原样内容及自动重读说明、路径、行范围；重开后原样重放，连续压缩将这些记录标作工具信息，不加入用户原话。读取先暂存，摘要或读取阶段被打断时不提交恢复内容。压缩项目新增可省略的 restoredFiles，终端在卡片下逐文件显示 Read / Referenced file，更新了协议生成物和 DESIGN 第 13 条的扩展说明。自动读取不产生 FileRead 卡片，遥测仍只统计模型自己发起的 read。
- 验证：新增 12 条核心集成测试和 1 条界面测试；功能缺失的 RED 已分别确认在恢复字段/新内容、write/edit 与最近顺序、单文件上限、超过 2,000 行但仍在 token 预算内的全文、prompt 引用、指令排除、路径显示、回退、连续压缩来源及卡片显示的断言处失败，再实现至 GREEN。负向的失败/打断、撤回窗口和遥测用现有行为做回归，没有人为制造 RED。全部用本机假模型 HTTP 服务、隔离 HOME 和临时会话状态，真实模型调用 0 次。
- 已通过的相关检查（均使用本 worktree 的独立 CARGO_TARGET_DIR，前台等待结束）：
  - `cargo nextest run -p canter-core --test restore_files --test skill_invocation --test retract --test stats --test session --test review -E 'binary(restore_files) | binary(skill_invocation) | binary(retract) | binary(stats) | test(compact) | test(rewind)'`：46 条通过。
  - `cargo nextest run -p canter-tools --test files --test file_rules --test sandbox_files`：40 条通过。
  - `cargo nextest run -p canter-protocol --test export --test compatibility --test reducer`：17 条通过。
  - `cargo test -p canter-tui --test compaction`：4 条通过。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过；协议生成物已重新生成并通过一致性测试。
- 主控决定（10-31）：保留 5 个、单个 5,000 token、合计 50,000 token 三个上限。合计上限目前不可达仍保留，不造超限测试、不为测试另开小预算，只测 5 个接近单文件上限的文件都能带回。token 按最终发给模型的内容估算，包含自动重读说明和行号。已按此实现和验证，没有其它待主控决定的事。
- 没做的事：未改摘要提示词、九个小节、压缩线或触发时机；未带回 shell、MCP、网页输出；未跑全套或真实模型试跑；未合并、推送、安装或重启用户服务。
- 踩过的坑：直接复用普通 read 会受 2,000 行分页限制，空行较多的小文件仍会被截断，改为复用执行许可和文件执行器、单独做有界文本捕获与完整 UTF-8 校验，保持普通 read 不变。假模型只回 `[DONE]` 会触发既有断流重试并消费下一份响应，失败测试改用明确的不可重试 HTTP 错误。遥测的文件信号使用规范化绝对路径，修正了最初按相对路径写的测试预期；nextest 中没有独立 rewind 测试二进制，改为在相关二进制里按测试名筛选。
