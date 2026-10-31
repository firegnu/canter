# 任务：第二期-7 - 压缩请求吃上缓存

2026-10-31，canter/main 交给 canter/dev-compact-cache（Codex，gpt-6-astra / xhigh）。
路由：常规 / 交叉审查要 / 影响面：碰要害（路由：常规（置信 0.73）、拿不准、拿不准；推翻：压缩决定长任务里模型还记得什么，属于核心规则，P2-142 刚改过同一处，按碰要害做、要交叉审查）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：第二期第 144 件。现在压缩时把整段对话拆开重拼成一大段文字发给模型，和之前请求的开头对不上，DeepSeek 的前缀缓存用不上，每次压缩全价。改成照 Codex 和 Claude Code：原来的请求原封不动，末尾加一条「请总结」。不改思考档、不改摘要的 9 个小节。
执行：派出

## 用户原话

- 10-29 看板标题：「也重点关注一下token消耗」。
- 10-31 主控拿三次试跑的账给用户看：花费里输出（含思考）占一半多，压缩请求本身占 TS 两次试跑的 26%～27%；建议本件只做「压缩请求吃缓存」。用户：「思考挡不要动，也不该动。」
- 10-31 主控解释做法（原来的请求原封不动，末尾加一句请按 9 个小节总结、这次不要调用工具；发不了时退回现在的拼稿办法），并说明 Codex `rust-v0.163.0` 的 `codex-rs/core/src/compact.rs` 就是在原历史末尾加总结提示，Claude Code 程序里有 `Compact cache sharing`、`Tool use is not allowed during compaction`。用户：「好的。」

## 三次试跑的账（10-31，主控记，deepseek-flash，都在优惠时段）

| | Rust（压缩线 15 万，压 1 次） | TS 第一次（6 万，压 15 次） | TS 第二次（6 万，压 15 次，有 P2-142） |
|---|---|---|---|
| 总花费 | 0.146 美元 | 0.346 美元 | 0.329 美元 |
| 压缩请求花费 | 0.015（10%） | 0.089（26%） | 0.089（27%） |
| 压缩请求没命中缓存的输入 | 9.1 万 | 43.9 万 | 45.9 万 |

压缩线 40 万时，按现在的做法每次压缩约 40 万 token 全价（高峰约 0.12 美元一次）。

## 先读

- `crates/canter-core/src/compaction.rs`：`summarize` 现在怎么拼稿（`PROMPT`、`SECTIONS`、`RULES`、`FOOTER`、分段折叠 `<earlier_summary>`），各种私有提示（计划模式、技能、诊断、后台事件、P2-142 带回的文件）在拼稿里怎么标来源、不当用户原话。
- `crates/canter-core/src/btw.rs` 的 `handoff`：P2-10 交接已经是「主线前缀、历史、工具定义原样，末尾追加说明」的做法，可以照它。
- 普通请求怎么拼：`runner.rs` 里发模型请求那段、`context.rs` 的 `request`（含 P2-5 图片预算）。
- `docs/DESIGN.md`：第 17 条（照顾缓存命中）、压缩和 `/handoff` 共用 9 个小节那段（P2-72）、P2-113 压缩线、遥测记录 `telemetry.compaction` 和请求遥测的 `purpose`。
- 参考 Codex `rust-v0.163.0` 的 `codex-rs/core/src/compact.rs`（`run_compact_task_inner_impl`）。只参考、不搬代码；只读克隆在 `/tmp/codex-ro`。

## 在哪里干活

- worktree：`../canter-worktrees/p2-144-compact-cache`，分支 `p2-144-compact-cache`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-144-compact-cache`。

## 要做的

1. **压缩请求的开头和普通请求一样**：模型、系统提示词、工具定义、历史消息都和这次压缩前最后一次普通请求发的一样（能保持一致的都保持），末尾追加一条用户消息：按现有的 9 个小节和规则总结，这一次不要调用工具，只输出摘要。目的是让前缀命中 DeepSeek 的前缀缓存。
   - 思考设置照现在压缩用的（关）。如果你发现这样会让开头对不上、吃不上缓存，停下来问主控，不要自己改。
   - 摘要的 9 个小节、规则的意思不变；提示里提到 `<transcript>` 的措辞按新做法改成指上面的对话。
   - 历史里那些用户角色的内部提示（计划模式说明、技能全文、诊断、后台事件、P2-142 带回的文件、目标说明等）现在在拼稿里标了来源。新做法里它们是原样的消息，要在末尾那条说明里讲清哪些不是用户说的话，「User's words」只摘用户真正说的。
2. **发不了时退回现在的拼稿办法**：这几种情况走原来的路，结果和现在一样：
   - 整个请求加上输出预留超出窗口（比如手动 `/compact` 时历史已经很长）；
   - 服务商报上下文太长；
   - 模型回了工具调用，或者没有摘要文字。
   退回不算失败，用户只看到正常的压缩结果。
3. **压缩完以后的样子不变**：摘要怎么存、保留哪些用户原话、P2-142 带回文件、计划和技能补回、遥测记录，都和现在一样。
4. **遥测能看出走的哪条路**：压缩请求照旧记 `purpose=compaction`，缓存命中照旧记在用量里；另外记一下这次压缩走的是新做法还是退回拼稿（字段名你定，写进 DESIGN），`canter stats` 能看到就行，不用改界面。

终端上显示的文字都用英文。

## 怎么算做完

- 用户 10-31 同意的做法：压缩请求照 Codex 和 Claude Code，原来的请求原封不动、末尾加一句请总结，这一次不调工具；发不了时退回现在的拼稿办法；思考档不动。
- 验证只做这些：每条行为一条针对性测试（先写测试、确认因为功能缺失而失败），用本机假模型服务端看收到的请求：
  - 压缩请求的系统提示词、工具定义、历史消息和前一次普通请求逐项一致，末尾多一条总结说明；
  - 三种退回情况各一条：超出窗口、服务商报上下文太长、模型回工具调用或空文字，都退回拼稿并得到正常摘要；
  - 内部提示不当用户原话；
  - 压缩后的历史、保留的用户原话、带回的文件和现在一样；
  - 遥测记下走的哪条路。

  再跑直接相关的已有测试（压缩、handoff、遥测、P2-142 带回文件），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。不跑全套，不调真实模型。觉得不够，在回复里说，不要自己加。
- 合并装好后，用户试跑看 `canter stats` 里压缩请求的缓存命中和花费（主控安排，不是开发做）。

## 不要做

- 不改思考档，不改普通请求，不改压缩线和触发时机，不改摘要的 9 个小节。
- 不删现在的拼稿办法，它留着当退回。
- 测试不调真实模型接口；不读、不改用户真实的 `~/.canter` 和会话，测试用隔离的 HOME 和临时状态目录。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-144-compact-cache` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-10-31，canter/dev-compact-cache。

- 做了什么：压缩复用 `context::request` 的模型、系统前缀、工具定义和完整历史，只在尾部加九节总结说明；思考仍关、输出上限仍为 `min(max_output_tokens, 8192)`。末尾说明区分计划模式、技能（含嵌在真实输入里的技能全文）、诊断、后台事件、带回文件、目标、目录、工具图片和旧摘要/清单的来源。完整请求加输出预留超窗、服务商报上下文超长、返回工具调用或没有完整摘要时，进入原拼稿分段折叠；工具不执行，废弃的摘要不混入回退请求。压缩后的存储、用户原话、计划/技能/目标/目录补回、文件恢复沿用原代码。
- 遥测：`telemetry.compaction.method` 和 stats 的压缩记录增加可选字符串 `prefix` / `transcript`，记录最后采用的方式；旧记录缺省未知。`canter stats` 文本报告显示 method，JSON 同时提供字段；所有请求仍为 `purpose=compaction`，缓存用量照旧。已更新 DESIGN 和生成的协议产物，不改交互界面。
- 验证：新增 `compaction_cache.rs` 六条本机假服务端测试，逐条 RED → GREEN。RED 分别是普通请求的工具定义丢失、超窗历史没有分段、上下文拒绝后仍切半而非从完整拼稿开始、工具调用使压缩失败、缺少内部来源说明、遥测缺少 method。压缩后历史和用户原话由新测试及既有回归检查，P2-142 文件恢复、重开、计划模式和技能补回继续使用原回归；这些保行为部分没有人为制造失败。旧测试中「工具定义为空」「历史带 User: 标签」的断言改为原消息加来源说明，失败场景明确让新增的回退请求也失败；未删除测试、放宽失败条件或缩短超时。
- 最终通过的检查（均在本任务编译目录、前台执行完毕）：
  - `cargo nextest run -p canter-core --no-fail-fast -E 'test(compact) or test(handoff) or test(telemetry) or test(summary) or test(compression) or binary(compaction_cache) or binary(restore_files) or binary(stats)'`：60 项通过。
  - `cargo nextest run -p canter --test stats`：3 项通过，含真实 CLI 的 method 文本和 JSON 输出。
  - `cargo nextest run -p canter-protocol -E 'test(exports_schema_and_typescript_and_matches_checked_in_artifacts) or test(telemetry_enums_accept_unknown_values_and_missing_new_fields)'`：2 项通过。
  - `cargo nextest run -p canter-core --test telemetry_recording`：1 项通过，原有手动性能比较仍忽略。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过；协议已用 `cargo run -p canter-protocol --bin export_protocol` 重新生成。
- 拿主意的地方：按任务授权选择 method 字段名；新路径只以完整请求估算加原输出上限判断是否超窗，回退的 16384 余量、分段缩小和重试机制保留。参考了只读 `/tmp/codex-ro`（确认标签 `rust-v0.163.0`）的 `codex-rs/core/src/compact.rs::run_compact_task_inner_impl`，没有搬代码。核对本地 wire 编码不会因关闭思考删改历史 reasoning_content；真实缓存命中仍以主控安排的试跑为准。没有需要主控另定的事项。
- 没做的事：真实模型调用 0 次；未读改用户真实状态，测试使用临时 HOME/状态目录和本机临时端口。未跑全套、未改思考档/普通请求/压缩线/九节规则、未合并/推送/安装；交叉审查、合并和用户试跑交主控。
- 踩过的坑：长输出通过假服务端逐 7 字节回放时，在到达压缩之前就超时；改成从隔离存盘载入长历史后，再由假服务端观察压缩请求。窗口夹具最初没有同时调低普通输出上限，先被配置校验拒绝，修正后才取得功能缺失导致的有效 RED。已有测试用「无工具定义」识别压缩及断言转写格式，与新路径冲突；已改成校验新请求形状，并保留压缩后的恢复和失败行为断言。

## 返工

2026-10-31，按主控在 `55e5c06` 上发现的 8 个失败返工。先原样运行这 8 条，确认 8/8 都以主控报告的症状失败。逐条诊断后，均属于测试夹具或断言依赖旧请求形状；本次只改测试及本记录，没有修改生产代码。原验证按包和测试名筛选，漏了跨包调用压缩的测试及名字不含 compact 的 LSP 测试；以后从压缩调用点检查直接相关测试所在包，不能只靠名称筛选。

1. `canter-server …plan::repeated_compaction_strips_only_the_previous_summary_plan`：旧断言把最后一条消息当作整份转写稿，新路径最后一条只有总结说明。前缀路径按要求保留上一份摘要及其清单；「只去掉上一份摘要的清单」仍是转写回退的规则。现在先校验前缀与普通请求的历史、工具一致，旧摘要清单和用户写的同名标记均原样保留、末尾说明清单是工具状态；再让假服务端返回一次上下文超长，校验回退请求。原来针对转写稿的全部断言保留：摘要正文、用户原话仍在，只有用户原话里的一个 `[Current plan]`，旧清单的 Implement 不出现。
2. `canter-server …context_report_is_read_only_and_describes_the_next_request`：Replay 假模型用 `tools.is_empty()` 识别压缩，误回普通回答 `done`，导致摘要 token 估算为 24，而预期 `saved summary` 为 27。只将夹具识别改成末尾用户消息的总结提示；保留原来的 27、分项 token、只读和下一请求逐项相等断言。
3. `canter-server …user_shell_rejects_work_approval_questions_and_compaction`：夹具用无 tools 识别压缩，未进入 pause 分支，立即回完摘要。临时诊断观测到：之前快照为 Working，但 shell 请求后，原 compact 轮已 Completed，新增 userShell 轮 InProgress；所以 200 是空闲后正常接受命令，并非压缩互斥失效。改用总结提示识别并真正暂停压缩，409/Conflict 和文件未执行断言原样保留；另检查拒绝后仍为 Working、轮数不变、同一压缩轮 InProgress 且摘要为空。临时诊断输出已删。
4. `canter-server …wake::completion_during_manual_compaction_wakes_after_turn_finishes`：夹具与测试等待条件都用「无 tools」识别压缩，导致摘要未暂停、等待条件永远不成立。两处共用按末尾总结提示识别的测试函数；保留后台结束时不提前开轮、完成压缩后只唤醒一次、没有用户输入/用户卡片、Exit code 7 和通知不重复的原断言。
5. `canter-server …wake::matching_event_during_compaction_survives_and_is_not_user_words`：先遇到与第 4 条相同的等待问题；后续断言也依赖 `Background event:` 转写标签。修正等待，改为检查原始后台通知恰好一条、正文包含匹配内容，末尾来源说明明确后台事件不是用户原话。新增对实际压缩存盘的精确断言：`user_messages` 只能是 `monitor-errors` 和 `after compaction`；保留暂停期间事件排队、压缩后唤醒、通知去重及重开重放的原检查。
6. `canter tests/tui.rs::thinking_none_low_and_compaction_preserve_their_wire_options`：唯一冲突是旧的「压缩无 tools」断言。改为工具定义等于前次普通请求，并验证完整消息前缀、最后一次回答和新增总结指令；补压缩输出上限 8192 和禁止调用工具的指令检查。none/low、原始思考内容、普通输出上限、压缩关闭思考及不带 reasoning_effort 的原断言都保留。
7. `canter tests/server …plan::plan_reminders_count_calls_across_turns_repeat_and_stay_out_of_ui_and_user_words`：旧断言寻找转写文本 `Tool result read-10: …`。改查真正的 tool 消息，read-10/read-20 都要求 role=tool、正文精确等于原输出加提醒；所有 user 消息都不能含提醒。跨轮计数、UI/导出不显示提醒、存盘 user_messages 精确等于两条真实输入的原断言保留。
8. `canter-core tests/lsp.rs::retracting_a_late_notice_keeps_later_user_words_labeled_as_user`：旧断言在前缀请求里找 `User:` / `Tool diagnostics:` 转写标签。现在先检查前缀逐项不变、真实原话仍为 user 消息、已撤回的诊断通知没有回流、末尾提示区分内部来源；假服务端随后报上下文超长，继续用原有两条转写断言检查回退稿里的 `User: RETRACT user words`，且绝不是 `Tool diagnostics: RETRACT user words`。另检查压缩存盘保留该原话且未把诊断存为用户原话，未丢掉对诊断索引清理的原覆盖。

验证：只运行上述 8 条（分组定位后联合复跑），最终 `cargo nextest run -p canter-core -p canter-server -p canter --no-fail-fast -E 'test(repeated_compaction_strips_only_the_previous_summary_plan) or test(context_report_is_read_only_and_describes_the_next_request) or test(user_shell_rejects_work_approval_questions_and_compaction) or test(completion_during_manual_compaction_wakes_after_turn_finishes) or test(matching_event_during_compaction_survives_and_is_not_user_words) or test(thinking_none_low_and_compaction_preserve_their_wire_options) or test(plan_reminders_count_calls_across_turns_repeat_and_stay_out_of_ui_and_user_words) or test(retracting_a_late_notice_keeps_later_user_words_labeled_as_user)'` 为 8 passed、432 skipped；没有跑全套或扩大测试范围。`CARGO_TARGET_DIR` 一直为本任务目录，`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。所有命令均在前台跑完；未删测试、放宽原行为断言或改超时，没有遗留诊断代码。真实模型调用仍为 0；无新增主控决策事项，不合并、不推送、不安装。

## 返工 2

2026-10-31，按 `P2-144-交叉审查.md` 的审查意见及用户选项返工。必须改第 1 条已在 `09e5fb1` 完成，本次复跑原 8 条；必须改第 2 条及用户选改的建议第 1、2、3 条落实如下，建议第 4 条按要求不改。

1. 必须改第 2 条：`PREFIX_SOURCES` 的文件恢复来源补上 `[Referenced file after compaction: ...]`，与自动重读一样明确为内部上下文。来源测试加入真实形状的只给名字消息，检查它原样进入前缀、末尾说明覆盖该标记，保留压缩存盘中用户原话及最终消息的精确断言。
2. 建议第 1 条：仅前缀请求遇到 `InvalidRequest` 时 `continue 'chunks`，退回完整拼稿；前缀已取走，拼稿再次遇到同类错误仍沿用原错误处理，不会循环回退。新增 `invalid_prefix_requests_fall_back_to_transcript`，分别用非上下文超长的 400 和 422 验证：前缀带工具、回退无工具且包含完整用户/助手原文、摘要正常存盘、没有 Warning/Error，最终遥测为成功的 `transcript`，两次尝试都记录为压缩请求。
3. 建议第 2 条：来源说明补上轮中断、服务端停止两类通知，以及 `<user_shell_command>` 中命令是用户输入、结果是命令输出的区别。来源测试加入这三类原始消息，检查前缀保留和说明中的来源区分；保留原来的存盘原话隔离断言。
4. 建议第 3 条：说明明确 `[Current plan]`、技能全文、目标说明和额外目录会自动补回，不要抄进摘要；来源测试检查这两项要求。保留原历史以维持前缀一致，不修改九节格式、公共规则或补回机制。以上行为先写入 DESIGN，再修改实现。

- RED → GREEN：扩充来源测试后先因缺少 `[Referenced file after compaction:` 的来源说明而失败，补齐说明后通过。新增 InvalidRequest 测试先因普通 400 让压缩失败（期望 Completed）而失败，加入前缀专用回退后，400、422 两个分支都通过。
- 验证范围：`cargo nextest run -p canter-core -p canter-server -p canter --no-fail-fast -E 'binary(compaction_cache) or test(repeated_compaction_strips_only_the_previous_summary_plan) or test(context_report_is_read_only_and_describes_the_next_request) or test(user_shell_rejects_work_approval_questions_and_compaction) or test(completion_during_manual_compaction_wakes_after_turn_finishes) or test(matching_event_during_compaction_survives_and_is_not_user_words) or test(thinking_none_low_and_compaction_preserve_their_wire_options) or test(plan_reminders_count_calls_across_turns_repeat_and_stay_out_of_ui_and_user_words) or test(retracting_a_late_notice_keeps_later_user_words_labeled_as_user)'`：15 passed、426 skipped（compaction_cache 7 条及上次 8 条一起通过）。只改了 compaction_cache 中的测试，没有扩跑其它测试。
- 最终检查：`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 全部通过。所有 cargo 命令均使用本任务的 `CARGO_TARGET_DIR`，全部命令在前台执行完毕。
- 拿主意的地方：按用户明确选择落实建议第 1、2、3 条；建议第 4 条不做。没有新增需要主控决定的事项。实际服务商缓存命中仍交主控安排试跑，思考档保持原样。
- 没做的事：没有改普通请求、压缩触发线、九节格式、存储及恢复机制；未删测试、放宽断言。未跑全套、未调真实模型、未读改用户真实状态、未合并/推送/安装。
- 踩过的坑：没有。

## 返工 3

2026-10-31，按交叉审查「复核」的必须改项返工。

- 做了什么：修正 `restore_files.rs::failed_and_interrupted_compactions_do_not_restore_files` 的非中断分支脚本，让前缀请求和拼稿回退各收到一条普通 400，之后仍留一条 `answer()` 给后续 `send()`。新增请求检查：初次 send 的两次请求之后，压缩恰好发两次请求，前缀带 tools、第二次拼稿不带 tools。原有 Failed / Interrupted、没有 restoredFiles、后续 send 成功、旧文件内容保留且新内容及恢复提示不出现的断言全部保留，中断分支的响应序列不变。只改测试和本记录，没有生产代码或设计变更。
- RED → GREEN：先运行 `cargo nextest run -p canter-core --test restore_files -E 'test(=failed_and_interrupted_compactions_do_not_restore_files)'`，复现原 628 行期望 Failed、实际 Completed。原因与审查一致：前缀 400 触发回退，原本留给后续 send 的 answer 被消耗为摘要。修正脚本后同一测试通过，两次 400 后确实失败，后续 send 仍成功。
- 同类检索：在整个 `crates/canter-core` 用 `rg` 检索 `InvalidRequest`、`invalid_request`、`BAD_REQUEST`、`UNPROCESSABLE_ENTITY`、400、422，并交叉核对调到 compact / compaction / summary 的测试及其 `ModelError` / `ErrorKind` / HTTP 状态脚本。HTTP 400/422 的模型脚本只在 `restore_files.rs`、`lsp.rs`、`compaction_cache.rs`：本次修正的 restore_files 是唯一用普通 400 期望压缩失败的测试；lsp 的 400 是 `context_length_exceeded`，已有正常回退摘要；compaction_cache 的普通 400/422 本来就在测回退成功，其它 400 也明确是上下文超长，均无需修改。`review.rs` 的压缩失败脚本使用 Server、ContextLength 或不完整摘要，`retry.rs` 的压缩脚本使用不完整流重试；都没有本次普通 400 的问题。`session.rs`、`skill_invocation.rs` 中的 `ErrorCode::InvalidRequest` 是本地参数/技能校验，不是服务商压缩失败脚本。未发现其它同类漏配，不修改或扩跑这些文件。
- 验证：`cargo nextest run -p canter-core --test restore_files --test compaction_cache --no-fail-fast`，19 passed、0 skipped（restore_files 全部 12 条，compaction_cache 全部 7 条）。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 全部通过。所有 cargo 命令使用本任务的 `CARGO_TARGET_DIR`，所有命令均在前台跑完。
- 拿主意的地方及没做的事：按用户指定修法执行，无需主控新增决定。未删测试、放宽断言或改超时；未跑全套、未调用真实模型、未读改用户真实状态、未合并/推送/安装。
- 踩过的坑：返工 2 新增错误回退时，漏核对 P2-142 用同类错误构造失败的脚本；本次按错误分类检索并逐项确认，不再仅靠测试名筛选。

## 主控审查

- 看了 `compaction.rs`、DESIGN 和改过的旧测试：压缩请求取自同一个 `context::request`，末尾加总结说明；退回拼稿的条件和任务文件一致，「不要做」一件没做。
- 全套第一次（`55e5c06`）8 个失败，都是别的包里按旧请求形状认压缩的测试，交回返工（只改测试）；`09e5fb1` 上 1 个和压缩无关的终端状态栏测试在负载 31 时失败，最终（`571ddbf`）全套 nextest 1290 通过、3 跳过，文档示例 1 通过。
- 交叉审查（Claude Code）：改完再合并，必须改 2（8 个旧测试、只给名字的带回文件没写进来源说明）、建议改 4、可以不改 1。用户选改建议改 1～3（前缀请求被拒收也退回、补全内部提示清单、会补回的内容不要抄进摘要），第 4 条（退回拼稿时的标签测试）不改，记 HANDOFF。第一轮复核又发现建议改 1 让 P2-142 一条失败测试失效（只改测试），第二轮复核可以合并。
- 合并后要试跑看：压缩关思考、普通请求开思考，DeepSeek 服务端关思考时怎么处理历史里的思考内容，文档没写；看 `canter stats` 里 `method prefix` 那次压缩命中缓存的输入是否接近前一次普通请求。
- 结果：返工 3 次

## 装上后的长程试跑（11-01，P2-142 + P2-144 + P2-171）

用户 10-31 晚：「我还想做一个长程任务的测试，就是之前做的那个。……请你总体评估一下。理解我意思吧，不但是压缩比还有其他的。」条件和第 2、3 次一样：canter-test-ts（分支 `trial-cache`），TODO.md 12 件，压缩线 60000，沙箱关，deepseek-flash（思考 high），Tab 插三句（第 2、5 件提交后，确认提示原文在第 3 件将提交时）。三次都按半价时段计价，花费可直接比。

| | 第 2 次（无带回文件） | 第 3 次（P2-142） | 这次（再加 P2-144、P2-171） |
|---|---|---|---|
| 用时 | 41 分 | 38 分 | 29 分 |
| 请求 | 319 | 252 | 255 |
| 压缩 | 15 次 | 15 次 | 12 次，全走 prefix |
| 压缩请求命中缓存 | 约 17% | 15–20% | 96–98% |
| 没命中缓存的输入 | 81.3 万 | 87.1 万 | 35.0 万 |
| 输出（其中思考） | 32.9 万（19.2 万） | 29.0 万（16.3 万） | 23.7 万（11.8 万） |
| 总花费 | $0.346 | $0.329 | $0.220 |
| 压缩花费（占比） | $0.089（26%） | $0.089（27%） | $0.022（10%） |
| read 调用 | 158 | 131 | 111 |
| 压缩后重读文件（累计） | 125 | 108 | 79（每次约 6.6） |
| 工具失败 | 18 | 14 | 18 |

- 完成：12 件全做、全勾、各一个 `ritual: ` 提交；`npm test` 145 过，`tsc --noEmit` 过；没停下来问，没有漏做、重做、卡住，请求 0 失败 0 重试。
- 约束：`Task: N/12` 第 3～12 件全带；确认提示隔 8 次压缩在第 12 件一字不差（`Remove "${habit.name}"? [y/N] `）；README 示例第 5 件后插，之后加了命令或选项的 4 件都写了。
- 压缩：12 次压缩思考输出合计 543 token，可忽略；压缩后 19–30k，和第 3 次相近。
- 省钱：压缩那部分 $0.089 → $0.022 是确定的；对话部分 $0.240 → $0.198 只跑一次，含模型写法的波动，不全算在改动上。
- 其他：一次 `rm -rf "$tmp"` 被 allowAll 下的关键路径检查拦下（变量解析不了就按关键路径算），偏严但安全，留下一个临时目录。抽看第 7、9 件实现和测试干净，第 10 件新命令也进了加锁。
- 总评：压缩后接得上、插的要求记得住、压缩花费降到原来的四分之一；剩下可改进的是压缩后重读文件，在降，不急。
