# canter-protocol

只有公开协议类型、事件归并和类型导出；没有网络服务或 agent 循环。
参考 Codex app-server v2 `rust-v0.163.0` 的稳定结构及字段名，独立编写，未复制源码。

## HTTP 对照

`routes` 提供路径常量。`SessionPath` 和 `RequestPath` 对应路径参数；
表中的 Params 是 JSON 请求体（GET 是查询参数，空 Params 表示无参数）。

| 方法和路径常量 | Params | Response |
| --- | --- | --- |
| GET INFO | InfoParams | InfoResponse |
| GET MODELS | ModelListParams | ModelListResponse |
| GET USAGE | 无参数 | UsageResponse |
| GET SESSION_USAGE | 无参数，SessionPath | UsageResponse |
| POST SESSIONS | SessionCreateParams | SessionCreateResponse |
| GET SESSIONS | SessionListParams | SessionListResponse |
| GET SESSION | SessionReadParams | SessionReadResponse |
| GET SKILLS | SkillListParams | SkillListResponse |
| POST RESUME | SessionResumeParams | SessionResumeResponse |
| POST INPUT | SessionInputParams | SessionInputResponse |
| POST BTW | SessionBtwParams | BtwEvent（私有 SSE data 帧） |
| POST INTERRUPT | SessionInterruptParams | SessionInterruptResponse |
| POST REQUEST | RequestResolveParams | RequestResolveResponse |
| PATCH SESSION | SessionUpdateParams | SessionUpdateResponse |
| POST COMPACT | SessionCompactParams | SessionCompactResponse |
| DELETE SESSION | SessionCloseParams | SessionCloseResponse |
| GET EVENTS | EventsParams | EventsResponse（每个 SSE data 帧） |

协议版本为 `PROTOCOL_VERSION`。每个会话的事件序号从 1 开始，空快照的
`lastEventSeq` 为 0；时间为 Unix 毫秒。单会话命令回复的 `eventSeq` 指向
该命令对应的事件，取快照时与快照的 `lastEventSeq` 一致。`info` 和列表
没有全局序号，各个 `SessionSummary` 自带 `lastEventSeq`；SSE 每帧自带 `seq`。
事件序号和时间在 JSON / TypeScript 中都是 number；网页调用方使用 JS 安全整数范围。

空闲时 input 开新轮，忙时默认 `queue`，也可 `steer`。
`UserInput` 支持 `{ "type": "text", "text": "这是什么" }` 和
`{ "type": "localImage", "path": "/tmp/screenshot.png", "name": "screenshot.png" }`。
图片沿用 Codex v2 的 localImage/path，`name` 是 canter 可选的显示文件名；不提供 detail 或网址输入。
服务端读取整条输入、校验图片格式和 8 MiB 上限后才接收；无效图片返回 invalid_request，
不追加消息或事件。事件里的图片 path 是会话目录内的持久副本文件名，name 保留原文件名。
此文件名以当前会话目录为基准解析，不能当作客户端工作目录里的本机路径重新提交。
存盘副本不可读时以 `[image unavailable: 名字]` 替代并发送 warning；请求不会因此失败。
纯文字输入的 JSON 不变；旧客户端用 Unknown 跳过新的图片种类。
排队成功先落盘并发送 `input.queued`，包含 `id`、`input`、`clientName`；
输入回复的 `eventSeq` 指向该事件，`turnId` 为空。快照的 `queuedInputs` 保留待执行输入。
开始对应轮时，`turn.started.inputId` 带排队输入 id，归并时将其移出队列。
非排队输入开始轮时 `inputId` 可省略或为空。`SessionInterruptParams.turnId` 是目标轮的前置条件。
`input.removed { inputIds, reason, turnId? }` 按 id 移出一个或多个排队输入；
reason 为 `steered`（插话消费）、`merged`（合并消费）、`interrupted`（打断丢弃）、
`closed`（关闭丢弃）。重复移出无影响，未知 reason 也按 id 移出。
`turnId` 在消费时指目标轮，丢弃时可省略；TypeScript 同样为可选属性。
忙时插话也先落盘 `input.queued`，回复的 `eventSeq` 指该事件、`turnId` 指当前目标轮；
只有实际进入上下文时才发送 `input.removed`，接收成功不等于模型已经看见。
手动压缩成功接收后立即回复：`eventSeq` 指独立压缩轮的 `turn.started`，
`turnId` 是该轮 id；旧回复可以没有此字段，省略时不序列化，TypeScript 为可选属性。
压缩结果看该轮的 `contextCompaction` 项目和 `turn.completed`，回复不等摘要结束。
压缩轮进行中的 `steer` 按 `queue` 处理，输入回复 `turnId` 为空，之后另开一轮。
批准和提问先到先得：后到的回答由服务端返回 `alreadyResolved: true` 和原处理事件序号，
不再产生一个处理事件。取消、超时的 `clientName` 可为空。
权限模式为 `alwaysAsk`、`askForExecution`、`allowAll`；批准回答为
`accept`、`acceptForSession`、`decline`、`cancel`。

解决请求的 `decision` 和 `answer` 都可省略，但必须恰好提供一个，且匹配请求类型。
批准仍发送 `{ decision, clientName }`；提问发送 `{ answer, clientName }`，其中 answer 为
`{ "type": "option", "label": "选项标签" }` 或 `{ "type": "text", "text": "自己写的一句话" }`。
选项标签必须在请求中，文字不能只有空白或包含换行。类型错、空回答、未知选项返回
`invalid_request`，请求、会话状态和事件序号不变；非法批准 decision 仍为 `invalid_decision`。
已处理请求沿用重复处理规则，后到的回答不覆盖原回答。
`request.resolved.resolution` 的提问结果是 `{ "type": "userInput", "answer": ... }`；
取消仍用 `cancelled`。等待问题沿用 `waiting_approval`，客户端由请求种类区分提示。
新的可选字段不影响旧批准请求；旧客户端遇到新种类按 Unknown 跳过。

`session.updated` 的 `settings` 是完整的公开设置（工作目录、名字、模型、思考档位、权限模式）。
创建会话、PATCH 成功后各写一条。PATCH 中省略/null 表示不修改字段。
`cwd` 同时出现在公开设置和会话摘要中，供显示、选择恢复目录；PATCH 不修改它。
环境变量只留在服务端内存中，供执行器和打开会话时的技能发现使用，不存盘，不进入事件、公开设置或快照。
创建请求不实现 Debug，调用方也不得记录其请求体。
`reasoningEffort` 选用 Codex v2 创建/恢复/派生会话回复中的名字；Codex 的设置对象和
改设置参数用 `effort`。canter 统一使用明确表示推理强度的 `reasoningEffort`，
取值为 DeepSeek 的 `none`、`low`、`high`、`max`。
创建时省略/null 默认 `high`；旧设置中缺少该字段按 `high` 读取。PATCH 省略/null 不改，
修改后由下一次模型请求使用；摘要请求仍固定关思考。新字段在 TypeScript 中可选。
例外：旧存盘的未知档位在运行和终端显示时按 `high`，PATCH 没带档位时也写成 `high`，
不把解码兜底值 `unknown` 写回；此规则不改变权限模式的 Unknown 处理。
创建请求的 `model` 为空字符串时使用服务端配置里的默认模型；非空时使用请求指定的模型。
创建和 PATCH 的模型必须在可用列表内，否则返回 `invalid_request`，设置和事件序号不变；
恢复旧会话不检查其已存模型是否仍在列表中。

`GET /v1/models` 一次返回配置的全部可用模型，回复结构参考 Codex v2 `model/list`：
`{ "data": [{ "id": "deepseek-flash", "model": "deepseek-flash", "displayName": "deepseek-flash", "isDefault": true }], "nextCursor": null }`。
条目的 id、model、displayName 都是配置中的模型名；isDefault 标记默认模型。
可选的 `inputModalities` 字段说明是否收图：`["text", "image"]` 或 `["text"]`。
旧回复不带该字段时，终端默认只有 deepseek-flash 收图。
本机配置列表无需分页，只提供模型名和默认标记；省掉的 Codex 模型字段和查询参数
及理由逐项记在 DESIGN 第 8 条。

`GET /v1/sessions/{id}/skills` 返回打开会话时固定的技能，按名字排序，一次全部返回：
`{ "data": [{ "name": "tdd", "description": "Test-first development", "path": "/work/.agents/skills/tdd/SKILL.md", "source": "project", "modelInvocation": true }] }`。
`source` 为 `global` / `project`，`path` 是 SKILL.md 的绝对路径；项目同名技能覆盖全局。
`modelInvocation: false` 表示技能禁止模型自行使用，但仍供 `/skills` 显示。
GET 不重新扫描，不发事件；修改技能在下次打开会话时才生效。未打开时返回
`session_not_resumed`，不存在时返回 `session_not_found`。该列表不存盘，读取错误只在
打开时沿用 warning 事件报告。与 Codex `skills/list` 的差异和理由见 DESIGN 第 8 条。

`GET /v1/usage` 按本机时区日期、模型汇总所有会话；`GET /v1/sessions/{id}/usage`
按模型汇总一个会话，不要求 resume。两者返回 `UsageResponse { data, total, lastTurnCost? }`。
每行有 `date?`、`model?`、`cachedInputTokens`、`uncachedInputTokens`、`outputTokens`
和 `cost: { usd, unknown }`，其中 usd 是已知金额合计，unknown 表示含至少一笔未知金额。
单会话行没有 date，合计没有 date/model；lastTurnCost 只在单会话存在已结束轮时提供。

`TokenUsage` 新增可选 `request: { model, startedAt, costUsd }` 和 `cost`。
request 是一次请求的已记金额，startedAt 是请求发出时刻（Unix 毫秒）；costUsd 可以是 null，
表示未知，不能视为零或以后补价。整个 request 缺失才是旧事件：服务端按当前单价、
事件 timestamp 和此前最近一次 session.updated 的模型补算，不写回文件。
cost 对应该 TokenUsage.total 的金额：session.usage 中是会话累计，Turn.usage 中是该轮累计。
客户端显示服务端给出的金额，不取价、不自行估算；旧客户端可以忽略这两个字段。

`POST /v1/sessions/{id}/resume` 的请求体为 `{ "env": { ... } }`，也不实现 Debug、
不得记录请求体。服务端重启后，会话默认未打开；接着聊用新环境打开，并返回快照及其
`eventSeq`。已经打开时只返回当前快照，保留原环境。未打开时快照和事件直接从盘读取，
input、interrupt、批准回答、compact、PATCH、DELETE 返回 `session_not_resumed`，
客户端须先 resume；不存在的会话仍返回 `session_not_found`。

### 上下文查询

`GET /v1/sessions/{id}/context` 返回已打开会话当前装入的上下文估算；未打开时返回
`session_not_resumed`。查询不改变事件、存盘或模型请求。返回字段：

- `prefix`：`{kind, path, tokens}` 数组，kind 为 `system`、`globalInstructions`、
  `projectInstructions`（每个文件一行）、`skills`；非文件段的 path 为 null。
- `skillCount`：模型可调用的技能数量；`toolCount`、`toolTokens`：工具定义数量及估算。
- `compactionTokens`：压缩摘要估算，未压缩时为 null。
- `messages`：`count`、`tokens`、`user`、`assistant`、`toolResults`、`images`；摘要单独统计。
- `estimatedTokens`：上述各项合计；`freeTokens`：窗口减估算值，最小为 0；
  `modelContextWindow`：窗口大小；`contextTokens`：与状态栏相同的最近普通请求实测占用，
  尚无用量记录时为 null。

估算沿用压缩判断的字节启发式和图片固定估算；系统消息的角色、JSON 开销计入 system，
其余前缀段按追加前后的估算差分计入，保持总和一致。排队输入、尚未产生的工具结果和
输出预留不计入；实际下一次请求还可能因新输入、工具执行或压缩而变化。

### 顺口问

`POST /v1/sessions/{id}/btw { "question": "问题原文" }` 不开轮、不排队，
响应是只发给本 HTTP 调用方的 `text/event-stream`。主线可以同时运行；请求复用当前
模型、思考档位、前缀、工具定义和已完整的历史，末尾追加问题原文。最后一组工具调用
尚有结果缺失时，连同这组已有的结果一并排除。模型返回工具调用也只结束这次回答，不执行工具。

帧为 `data: {"type":"delta","delta":"回答片段"}`，末尾为 `completed`、
`cancelled` 或 `error { message }`。这些帧没有 seq/id，不落盘、不重放，断开响应流
就取消本次 btw。问答不进入会话事件、模型历史或快照；错误也不改变主线状态。
空白问题返回 `invalid_request`，同一会话已有 btw 时返回 `conflict`；未打开时返回
`session_not_resumed`。关闭会话取消 btw 并等已收到的用量入账。

用量单独记为 `session.btwUsage { tokenUsage }`：total/cost 为会话累计，last/request
为本次请求，contextTokens 保留主线实测值。服务端汇总时不把它归入任何主线轮，
不改变 lastTurnCost。已有 `session.usage` 语义不变；旧客户端可跳过新事件。

### 交接

`POST /v1/sessions/{id}/handoff { "direction": "新会话要做的事" }` 沿用上述私有
`BtwEvent` SSE 和断流取消；direction 可省略或为空白，此时接着做当前任务。
只在已有对话、主线空闲、没有 btw 时接受；空会话返回 `invalid_request`，
主线在跑或等待批准/提问、已有私有请求时返回 `conflict`。生成期间主线输入、压缩和
设置更新也返回 `conflict`。主线请求的前缀、历史和工具定义原样保留，末尾追加英文
交接提示，要求用用户的语言只输出说明；不执行工具、不写问答历史或事件。
用量沿用 `session.btwUsage`，记入原会话，不改变 ctx。

终端收完非空说明后通过已有建会话接口创建同目录、模型、思考档位和权限模式的新会话，
input 不填，说明只放入本地输入框供用户修改后发送。取消、错误或空回答不切会话。
原会话保持打开，不保存会话父子关系。

### 审查轮

`GET /v1/info` 新增可选 `features` 字符串数组，本版返回 `["review"]`。
终端每次发起 `/review` 先读取所连服务端的能力；缺省/null 或不含 review 时不发送，
提示运行 `canter serve --replace` 后重试。旧客户端可以忽略该字段。

`POST /v1/sessions/{id}/input` 可带 `review: "Review: uncommitted changes"` 或 PR 标题。
存在这个字符串即开启只读审查轮，省略/null 仍是普通输入。`QueuedInput` 和 `Turn`
带同一个可选字段，随队列、轮事件和快照保留；Schema 和 TypeScript 也标为可选，
旧客户端和旧存盘不必提供。审查输入即使指定 steer，也排队成为独立一轮。
`input` 和 userMessage 仍存完整审查要求、对象和 diff，模型回答沿用 agentMessage；
终端仅把这一轮的开头提示显示为 review 标题，后续插话正常显示。
write/edit 与白名单外 shell 直接返回 `review is read-only`，不产生批准请求，
会话权限模式与已有授权都不放行；轮结束后继续按原权限运行。
这是 canter 自定扩展，与 Codex 的差异见 DESIGN 第 8 条。

### 错误与 SSE

HTTP 错误统一返回 `ErrorResponse { code, message }`，不带成功回复的 eventSeq：

| code | HTTP 状态 | 用途 |
| --- | --- | --- |
| invalid_request | 400 | 请求格式、未知输入/权限模式或续传游标无效 |
| session_not_found / request_not_found | 404 | 会话或请求不存在 |
| turn_mismatch / session_closed / conflict | 409 | 目标轮不符、会话已关闭或状态冲突 |
| session_not_resumed | 409 | 会话尚未打开，请先用 resume 重新提供环境 |
| invalid_decision | 400 | 回答不在 availableDecisions 中（包括 Unknown） |
| internal | 500 | 服务端内部错误，message 不包含私有数据 |

`Unknown` 是旧客户端的解码兜底，不是服务端可执行的新权限或批准选项。
会话事件响应类型为 `text/event-stream`，每帧格式由 `sse_frame` 生成：

```text
id: 42
event: message
data: {"seq":42,"sessionId":"s1","timestamp":9,"type":"warning","message":"notice"}

```

`id:` 是该会话事件的十进制序号，`event:` 固定为 `message`，实际种类在 JSON 的 `type`。
data 为单行 JSON，内容里的换行保持 JSON 转义；帧末两个换行。
续传严格取序号大于游标的事件。`Last-Event-ID` 存在时优先于查询参数 `after`，
否则用 `after`（默认 0）；空、负数、非十进制或溢出的 header 返回 invalid_request。
`resume_after` 实现这一共享规则。心跳可以是 SSE 注释，不占事件序号。

## 归并

流式客户端和存盘重放使用 `apply_event_mut(&mut snapshot, &event)` 原地归并，
所有会报错的条件在修改前校验，失败时快照不变；输出增量不会复制整份历史。
`apply_event(&snapshot, &event)` 保留为克隆加原地归并的便利接口。
`validate_event` 供存盘在写盘前校验；单条 append 在落盘成功后再原地归并。
必须按会话连续顺序使用；错会话、序号缺口和无效的已知项目增量返回错误。
已消费序号的重复事件返回原快照。未知字段忽略；各枚举新增值解码为 Unknown，
不会因为已知事件中出现新的状态、输入或回答而中断整条流。未知事件、项目和增量跳过，
但仍推进事件游标，以便继续处理后续已知事件。未知项目不加入各轮的项目列表。

`session.status` 是会话状态的唯一来源，不从项目状态推测。
`turn.completed` 只更新目标轮的 status/error/usage，忽略其 items/input/clientName，
因此生产者可以发送空 items，不必重复整轮输出。项目以 `item.*` 为准，
`item.completed` 携带完整最终项目并替换同 id 的项目。
待处理请求公共字段 id/turnId/createdAt 放在 PendingRequest 外层；
kind 内层为带 type 的 RequestKind，包含 approval、userInput 及未知种类的兜底。
批准内容包含 itemId/tool/arguments/availableDecisions，未知请求也能按外层 id 移除。
提问内容包含 itemId/question/options，每个选项是 label/description；一次一题、2–4 个选项。
对应的 `userInput` 项目保存 id/question/status 和可选 answer，答完后快照也能显示问答；
取消、关闭和重开会话时沿用批准的请求取消及未完成工具收尾规则。
待处理请求由 `request.opened` 加入、`request.resolved` 移除；对已移除请求重复处理
不会改变批准状态。若输入历史本身包含一条新的重复处理事件，只更新其游标和时间。
`warning` 只供实时展示；最近的 `error` 和 `session.usage` 保存在快照中。
结束会话使用 `session.status: closed`，不删除存盘。事件和 `SessionSnapshot`
带可选的 `closeReason`：`clientRequest` 表示客户端 DELETE，`serverShutdown`
表示服务端停机（含升级）。归并保留当前关闭的原因；后续非 closed 状态清空原因。
字段缺失时不序列化，旧事件和快照没有该字段仍可读取；TypeScript 同样为可选属性。
终端连接新服务端时先读快照：停机关闭、无原因的旧会话，以及崩溃留下的非关闭状态
可自动 resume；客户端主动关闭或未知关闭原因保持关闭。用户主动 resume 不受此限制。

## 重新生成

在 workspace 根目录运行：

```sh
CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/main cargo run -p canter-protocol --bin export_protocol
```

也可以在 `--` 后传一个输出目录。`generated/protocol.schema.json` 是 schema 类型目录，
具体类型使用 `#/$defs/Event`、`#/$defs/SessionSnapshot` 等引用。
`generated/typescript/` 是 TypeScript 类型及协议版本。schema 描述当前已知的输出种类；
跨版本客户端仍必须在解码时跳过未知种类，不能把旧 schema 当作未来事件的严格白名单。
导出测试在临时目录生成、检查关键字段并与入库产物逐字节对比。

## MCP（P2-12）

`GET /v1/mcp` 返回 `McpListResponse { data }`：每项有 `name`、`connected`、`error`（无错误为
null）、`tools: [{ name, tool }]` 和 `warnings`。`name` 是给模型的完整工具名，`tool` 是原 MCP
工具名。此接口查询当前服务目录，已打开会话的工具快照不随它变化；不返回命令、地址、环境、
请求头。终端 `/mcp` 使用该接口。

`Item::McpToolCall` 序列化为 `mcpToolCall`，含 `id`、`server`、`tool`、`arguments`、`result`
（有界文字）和既有 `ItemStatus`。它通过既有 item.started / item.completed 发送，没有 MCP
专属增量事件。批准仍是 `ApprovalRequest`，tool 用完整 MCP 工具名，arguments 是调用参数；
客户端可从对应项目显示原始服务名/工具名。旧客户端按 Unknown 忽略新项目，其它字段含义不变。

## 网页抓取（P2-13）

新增 `Item::WebFetch`，序列化为 `webFetch`，字段为 `id`、完整 `url`、有界文字 `result`、
既有 `ItemStatus`。沿用 item.started / item.completed，无专属增量事件。失败原因放在 result，
取消和超时的项目状态为 failed，一轮是否 interrupted 沿用现有取消机制。
批准仍是 `ApprovalRequest { tool: "web_fetch", arguments: { url }, ... }`，完整网址不截断；
acceptForSession 的授权范围为该网址的规范化主机名及有效端口，客户端需明确显示该范围。
与 Codex 的 webSearch 不同，webFetch 只取指定网址，不带搜索动作或查询/来源列表。
旧客户端按 Unknown 忽略新项目，已有协议字段和历史文件格式的含义保持不变。
