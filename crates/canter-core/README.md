# canter-core

会话、一轮的模型/工具循环、批准、上下文和压缩。不包含 HTTP、终端 UI 或模型凭证。
服务端保留每个已打开会话的一个 `Session`，多个客户端共享它的克隆。

## P1-5 接入

`Session::create(Arc<Store>, id, SessionSettings, RuntimeContext, Arc<dyn Model>, CoreOptions)`
返回 `(Session, SessionCreateResponse)`；`Session::open` 不传 settings，返回
`(Session, SessionReadResponse)`。调用方负责提供会话 id 和专用的存盘根目录。
`Store::new` 本身是同步的，服务端应在阻塞线程中创建它；核心内部的所有 Store/
SessionWriter 操作、执行器构造和项目指令读取都在 `spawn_blocking` 中进行。

`RuntimeContext { environment, protected_paths }` 只放内存，不实现 Debug/序列化。
创建和恢复都必须重新传入完整环境，以及 canter 状态目录、配置文件等受保护路径。
核心不读取或合并服务端进程的环境。工具和 PermissionGate 每次打开重新创建，
会话授权不存盘。恢复时的未知权限模式仍交给闸门按每次问处理。

| 方法 | 行为与成功回复的 eventSeq |
| --- | --- |
| `create` / `open` | 初始化/恢复至 idle；返回快照对应的最后序号 |
| `input(SessionInputParams)` | 空闲开轮：turn.started；忙时 queue/steer：input.queued |
| `interrupt(SessionInterruptParams)` | turnId 必须匹配当前轮；等模型取消、工具回收及历史收尾后，返回 turn.completed 的序号 |
| `resolve(request_id, RequestResolveParams)` | request.resolved；后到的回答返回原序号及 alreadyResolved=true |
| `compact()` | 空闲时开独立的压缩轮，turn.started 落盘后即返回其序号和 turnId，结果看事件；忙时返回 conflict |
| `update(SessionUpdateParams)` | session.updated；名字、模型、权限模式可改，下一次请求/权限判断使用新值 |
| `close()` | 停止当前轮、清队列、释放存盘锁，返回 session.status=closed 的序号；可再次 open 恢复聊天 |
| `snapshot()` | SessionReadResponse，eventSeq 与 lastEventSeq 一致 |
| `skills()` | SkillListResponse，返回会话打开时固定的技能列表，不重新读盘 |
| `subscribe(after)` | `Subscription::recv()` 按序返回事件，每条自带 seq；after 超过当前最后序号报 invalid_request |

`CoreError` 提供协议的 `code` 和安全的 `message`，供服务端映射 HTTP 错误；重复会话 id 返回 conflict。
接受的变更在独立任务中完成，调用方丢弃等待 future 不会截断写盘/内存状态转换。
`close` 应在运行时停止前 await；丢弃最后一个 Session 句柄也会请求取消，但不能代替
运行时退出前等待清理。订阅者不会持有写入锁，close 后读完最后事件即返回 None。

## 循环与事件

先完整收完模型响应，再串行处理工具。工具只在 `Finished(ToolCalls)` 后执行，
严格按 prepare → started_item → gate.evaluate/resolve → execute 接入。
提问工具 `ask_user` 在 prepare 后由循环开 `userInput` 待处理请求、等待回答，跳过权限闸门
和执行器；包括 allowAll 在内的所有模式都等人回答。工具结果为 `{question, answer}`，
answer 区分选项标签与自写文字；显示问答写入 `userInput` 项目，恢复快照仍可查看。
等待、取消、关闭、重开和多客户端先到先得共用批准请求机制；不设置超时，不记录会话授权。
Length/Other 发出可见 warning，并为没有执行的调用补结果。工具失败/超时/拒绝的结果
交回模型；取消、拒绝并停下则终止当前轮。模型错误以 error 和失败的 turn.completed 收尾。
未知工具把结果交回模型，并发 warning 写明工具名。等批准时被打断，先发
request.resolved(cancelled)，再记录“被打断，没有执行”的工具结果和 failed 项目；
已收到拒绝或“拒绝并停下”的回答时，工具结果仍是 declined，后者同时停止本轮；
收不到回答的取消才记为被打断。打断（包括压缩）以 interrupted 收尾，不设置轮错误或发 error。
`declineWithFeedback` 同样不执行工具、保留 declined 卡片、继续本轮；模型收到
`Tool call declined. The user says: <原样说明>`，不翻译、不截断说明。
缺失或空白说明、其它 decision 携带说明、未提供的选项均报 `invalid_decision`，仍等待原请求。
说明随 `request.resolved` 存盘，并归并到快照的 `approvalFeedback`，重开后仍可显示。
完整思考内容写入模型历史，显示事件不是模型历史的替代品。

同会话的命令、批准回答和落盘更新串行；耗时的模型/工具等待不持会话锁。
一条批准请求只写一次处理事件，取消/恢复时也记录原处理序号供晚到回答查询。
模型内部重试不暴露重试事件，核心保持 working；没有伪造“正在重试”提示，也未修改模型接口。

普通对话忙时插话先排入公开队列：回复 eventSeq 指 input.queued，turnId 指当前轮。
下一次模型请求前写入上下文和用户项目，再发 input.removed；同时消费多条插话时
reason=merged，单条为 steered。模型失败时仍把已接受插话写入当前轮的上下文，
便于后续继续。普通 queue 输入逐条另开一轮，由 turn.started.inputId 移出。
打断/关闭时清除两种队列，原因分别为 interrupted/closed。
手动压缩轮中的 steer 按 queue 处理，回复不带 turnId，压缩结束后另开一轮；
后续对话的失败或批准等待不改变压缩轮的结果。

事件先 append_batch 并完成磁盘同步，再更新快照和广播；文字/命令输出增量
每约 20 ms 合批，模型流的待写事件到 128 条也刷盘。广播缓冲为 256 条事件。
订阅先接上广播，再从 Store::events_after 补历史，按已完成同步的序号上限读取；
实时重复事件按游标去重，广播落后时重新从盘补，因此不会将写盘中途的后缀提前发送。

## 模型历史与恢复

每次正常请求的消息由当前 Context 和追加 Message 原样组成。新增用户消息、完整助手
内容、原样工具参数、工具实际回传文字都用 canter-model 的消息类型写入模型历史后再使用。
相邻的同类文本/思考增量合成内容片段，文字不改写。

私有 JSONL 条目带 `type`，目前有：

- `context { messages, segments? }`：`segments` 保存提示词各段的 `kind`、可选 `path` 和原文 `text`；`messages` 保存转换器拼出的一条 system。顺序为固定段 `core`、模型家族补充 `model_family`（目前为空）、全局说明 `global_instructions`、技能 `skills`、项目说明 `project_instructions`（每个文件一段）、环境 `environment`。前三种跨会话稳定，后三种本会话固定；稳定性由种类的常量表决定，不另存盘。创建或打开会话时先固定内置及 MCP 工具集，再读文件、生成各段；段和 messages 都与最后一条 context 相同就沿用存盘字节，有一样不同才追加。只有 messages 的旧记录第一次重开会追加带 segments 的新记录，旧行不改；旧版仍可忽略 segments 读取 messages。
- `message { message, user_input }`：一条模型消息；user_input 区分用户原话和核心的中断提示，旧条目缺字段时默认 false。
- `compaction_request { messages }`：每次摘要调用收到的完整消息，仅作审计，不混入正常对话。
- `compaction { summary, messages, user_messages }`：压缩后的完整对话替换及保留的用户原话，恢复从最后一次压缩继续。原记录不删除。
  有图片时另加可选 `user_inputs` 保存保留的完整用户消息；旧的纯文字记录仍原样读写。
  图片用户消息用 `user_parts` 保存按顺序排列的文字和会话副本文件名，不存绝对路径或内联 base64。
  常规与压缩请求都临时读取副本，支持图片的模型收到 data URL；不支持的模型收到文字占位。
  从最新图片向前累计 base64 大小，40 MiB 之外的旧图只在请求中换成 omitted 占位；
  副本按当前会话目录解析，不可读时换成 unavailable 占位并发 warning，历史引用不改。
  图片每张估 1024 token，并计入压缩保留用户消息的现有预算。

全局说明从会话环境的 `CANTER_HOME` 下的 `AGENTS.md` 读取，没有 `CANTER_HOME` 时读 `HOME` 下的 `.canter/AGENTS.md`；两个都没有或不是绝对路径时不读全局说明，不读取服务端进程的环境。项目说明从最近 git 根读到 cwd，没有 git 根则只读 cwd。
全局和项目的每份 AGENTS.md 至多 64 KiB；文件不存在或只有空白时不加段落、不警告，其他读取失败、非 UTF-8 或超限时整份跳过，warning 写明文件路径和原因，继续正常请求。
警告也只在创建或打开会话时发送；中途问题变化不重新提示，下次打开时按当时的读取结果报告。

`prompt` 模块的 Chat Completions 转换器是纯函数：固定段和家族补充原样放置，其余各段包在
`<global_instructions path="…">`、`<skills>`、`<project_instructions path="…">`、`<environment>` 中，
段之间空一行。文件内容不转义、不裁空白，path 属性转义 `&`、`"`、`<`、`>`；空段不出现。

固定段原文在 `src/prompt/core.txt`（P2-57 方案 §3.1）：行首 `- [条件]` 的要点只在条件成立时出现，
条件按打开会话时的工具名求值，支持 `&`、`|` 和括号，`mcp` 指至少有一个 `mcp__…` 工具；
`{approval_list}`、`{data_list}` 按同一工具集依次拼接，清单为空时整句不出现。
当前六个内置工具和没接 MCP 时渲染出的全文放在 `tests/data/core_deepseek.txt`，测试逐字核对。

环境段固定四行：工作目录、git 仓库根（没有则 none）、系统和架构、本地打开日期、英文星期和 UTC 偏移。
日期后提示当前日期可能更晚；不加入权限模式、git 分支/状态或模型名。会话中途改文件或跨本地零点都不重读，
普通请求、手动和自动压缩沿用打开时的前缀，下次打开才更新。

`/context` 每段一行：固定段为 system，家族补充为 modelFamily（非空才有），其余为
globalInstructions、skills、projectInstructions、environment。路径保留为原路径。
角色和 JSON 开销计入 system，标签及段间空行随各段按累计估算差分计入，合计等于整条 system 的估算。

技能从会话环境 `HOME` 下的 `.agents/skills/<目录>/SKILL.md` 和同一项目根目录下的
`.agents/skills/<目录>/SKILL.md` 发现；未提供 HOME 时只查项目，不读取服务端进程的 HOME。
跟随技能目录的符号链接，按 `name` 去重（项目优先）并排序。只读取开头两个 `---`
之间的头部，使用本机样例的单行普通文本或双引号文本解析 `name`、`description`，
忽略其他元数据。缺少头部、名字或简介，以及读取或 UTF-8 错误，都跳过并沿用 warning；
技能根目录不存在不警告。`disable-model-invocation: true` 的技能仍列在 `skills()` 中，
但不进入系统提示词。没有可供模型使用的技能时不加技能段落。全文只在模型需要时由现有
read 工具读取，权限与受保护路径规则不变。完整技能列表只驻内存，提供给模型的简介和路径随 skills 段存盘。

未知条目/消息/助手内容会跳过，绝不把 Unknown 再写回。恢复模型上下文只读模型历史；
事件用于恢复显示、请求处理序号和轮状态。重新打开时取消遗留批准和排队输入，未完成轮
标成 interrupted，未完成工具项目标成 failed，并补工具占位结果和“被打断”记录。
事件与模型历史是两个文件，没有跨文件事务：先记模型输入/结果，再记相关显示事件；
助手流式增量照常发送，收完流后先写完整助手消息，再发最后的 item.completed。
恢复不会根据显示事件猜测工具结果或重新执行工具。

## 压缩与数字

`CoreOptions` 可配置 `context_window`、`max_output_tokens`。思考档位由会话设置的
`reasoningEffort` 决定，每次请求重新读取，默认 `high`；`none` 关思考，其余档位开思考并传
对应的 `reasoning_effort`。摘要请求固定关思考、不传档位。默认窗口
1,048,576 token（按 DeepSeek 当前文档的 1M），所有档位的正常输出均显式限制为 65,536；
窗口必须大于输出上限加 16,384。模型包保持原样，核心显式发送思考和输出上限选项。
依据：[模型参数](https://api-docs.deepseek.com/quick_start/pricing)、
[max_tokens 定义与输入输出总长度限制](https://api-docs.deepseek.com/api/create-chat-completion/)
（2026-10-24 只读核对，没有调用模型）。

自动触发：输入占用 > 窗口 − 输出上限 − 16,384。无 tokenizer 时按序列化消息与工具定义
每 3 UTF-8 字节约一个 token，加每条消息 8 token 估算；也取提供方上次实际输入用量
加新增上下文估算，两者的较大值。它是估算，不保证等于提供方计数。
收到 ContextLength 时也会压缩重试；一次请求最多恢复一次，仍超长就明确失败，避免无限压缩。
自动压缩失败时保留原上下文；若输入占用加输出上限仍在窗口内，发 warning 后继续原请求，
超出窗口才报错并说明压缩失败及窗口不足。输入占用沿用上述估算与实际用量的较大值。
自动压缩失败后，本轮的后续工具调用和插话不再触发自动压缩，仍检查窗口硬上限；
下一轮或手动压缩可以重新尝试。

摘要独立调用模型，禁用工具和思考，输出上限 min(正常输出上限, 8,192)。对话太长时
按可容纳的 UTF-8 文本块顺序摘要，把前一块摘要带到下一块，不静默扔掉旧内容；完整思考
保留在原历史里，摘要文本不包含思考。摘要失败或取消时保留原上下文。
摘要请求被提供方判为 ContextLength 时，将当前块减半（按 UTF-8 边界）重试；
只有成功才前移待摘要文本，下一块带上先前摘要。缩到无法再分仍失败时停止，保留原上下文。
Length 摘要仍视为不完整，走上述失败回退，不用截断的摘要替换历史。
压缩后保留最近约 20,000 token 的用户原话，再加摘要；小窗口会进一步缩减保留预算，
边界消息只保留可容纳的原文尾部，按 UTF-8 边界截取。

压缩和 `/handoff` 的说明原文采用 P2-57 方案 §6，共用 Goal、User's words、
Constraints and preferences、Decisions、Files、Commands and results、Where it stopped、
Open issues、Next steps 九个小节及规则。压缩转写包在 `<transcript>` 中，分段时前一块
摘要包在 `<earlier_summary>` 中（没有则整块省略），末尾提醒只输出摘要；标签和尾句
均计入每段预算。压缩后的历史仍为保留的最近用户消息加 `[Context summary]\n<摘要>`。
`/handoff` 先要求 `## Task`，另强调工作区状态和动手前的检查，用户方向放在说明末尾，
未给方向时用 `Continue the current task.`；沿用前缀、历史和工具定义，但不执行工具。

系统提示词独立撰写；循环/插话参考 pi v1.4.0 的 agent-loop.ts、agent.ts，压缩触发参考
packages/coding-agent/docs/compaction.md；摘要结构和保留用户消息的方式参考
Codex rust-v0.163.0 的 core/src/compact.rs 与 prompts/templates/compact/prompt.md。
没有复制代码或提示词原文，不新增第三方许可证声明。

测试仅使用脚本假模型，不起 HTTP、不调真实接口；工具在临时目录运行。
覆盖任务指定的四项验收及三个打断位置、两个客户端竞争回答。

MCP 工具由服务端经 `RuntimeContext.mcp_tools` 在会话创建/重新打开时提供快照。
`State` 保存内置工具与该快照组成的模型工具列表；普通请求、上下文估算和压缩沿用它，
会话中途不从当前服务目录重建列表。连接句柄仍能发现断线并返回工具错误。
MCP 调用沿用 prepare → permission → execute 和取消令牌；审查轮直接拒绝且不打开批准请求。

## 撤销与回退

`rewind_list()` 按新到旧列出本会话可退的用户轮和所选轮及以后涉及的文件数；
`rewind_preview(SessionRewindParams)` 只读取文件、报告外部修改和不可恢复原因；
`rewind(SessionRewindParams)` 选择只退对话、只退文件或一起退。运行中的轮（含旁问）、
待处理队列和关闭中的会话拒绝执行；先打断并等到 idle。成功压缩会清掉此前的回退边界，
包括在该轮中途自动压缩的那一轮；压缩后的新轮仍可退。

每轮输入前追加 `turn_boundary`。内置 write/edit 获得执行许可后，沿用已准备的原字节和
规范化路径，在实际写文件之前保存 `rewind-*` 副本并追加 `file_checkpoint`（带轮 id、路径、
原内容与预期写入内容的引用）；工具失败追加 `file_aborted`，不会把失败的改动计入回退。
副本与会话文件一样为 0600，目录沿用 0700。原内容不超过 **1 MiB** 就保存，
超过时只记原因，无法恢复该原内容。写入内容超过 **1 MiB** 时只记长度，不影响原内容备份；
这时无法确认有没有外部修改，预览标为 `externalChangeUnknown`，须明确确认该路径才执行。
副本缺失等不可恢复原因在预览与结果记录中说明。文件恢复不依赖 git；原来不存在的文件删除，
原来存在的恢复字节，路径变化、权限或 I/O 错误逐文件报告。

预览比较当前文件与最后一次 canter write/edit 的预期内容；不同即标为外部修改。
预览带 `lastEventSeq`，执行时须原样传回，同时用 `confirmedExternalChanges` 路径列表
确认预览中列出的外部修改或无法比较的文件。执行时再比较一次，会话序号已变或出现未确认
的路径都返回 conflict，不改变任何文件或对话，客户端须重新预览、确认。shell/MCP 不创建检查点；
它们自行创建的文件不退，它们在已跟踪文件上的后续修改会被识别为外部修改。

回退只追加 `rewind` 模型历史和 `session.rewound` 事件，不改写已有 JSONL 行。
对话仅截断指定边界后的消息，保留前面的模型消息原值；用量仍累计真实已发生的请求。
结果作为一个无用户输入的记录轮广播并存盘（恢复/删除/失败的路径及原因），不作为模型消息，
因此不会插入或改写保留的请求前缀。跨重开时系统提示词沿用现有恢复规则，使用这次打开时的
系统提示词。打开会话时在恢复模型消息的同一遍重放中计算边界和回退，使用相同的未知消息过滤规则；
若模型历史已提交而事件未提交，
在 resume 时补发该事件。多文件恢复逐个执行，不是文件系统事务：进程在恢复中途退出可能留下
部分已恢复的文件，下次预览会据实际内容报告差异；已有私有备份仍保留。

没有重做、跨会话回退或 git 快照。升级前没有记录边界和文件副本的轮不会出现在列表中。
旧版会忽略未知的回退历史条目与事件，重新看到并使用被退掉的对话；回退过的会话应继续用
支持 rewind 的版本，不能靠降级恢复对话一致性或恢复已删除的文件。
