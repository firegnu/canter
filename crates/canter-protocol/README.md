# canter-protocol

只有公开协议类型、事件归并和类型导出；没有网络服务或 agent 循环。
参考 Codex app-server v2 `rust-v0.163.0` 的稳定结构及字段名，独立编写，未复制源码。

## HTTP 对照

`routes` 提供路径常量。`SessionPath` 和 `RequestPath` 对应路径参数；
表中的 Params 是 JSON 请求体（GET 是查询参数，空 Params 表示无参数）。

| 方法和路径常量 | Params | Response |
| --- | --- | --- |
| GET INFO | InfoParams | InfoResponse |
| POST SESSIONS | SessionCreateParams | SessionCreateResponse |
| GET SESSIONS | SessionListParams | SessionListResponse |
| GET SESSION | SessionReadParams | SessionReadResponse |
| POST INPUT | SessionInputParams | SessionInputResponse |
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
排队成功先落盘并发送 `input.queued`，包含 `id`、`input`、`clientName`；
输入回复的 `eventSeq` 指向该事件，`turnId` 为空。快照的 `queuedInputs` 保留待执行输入。
开始对应轮时，`turn.started.inputId` 带排队输入 id，归并时将其移出队列。
非排队输入开始轮时 `inputId` 可省略或为空。`SessionInterruptParams.turnId` 是目标轮的前置条件。
审批先到先得：后到的回答由服务端返回 `alreadyResolved: true` 和原处理事件序号，
不再产生一个处理事件。取消、超时的 `clientName` 可为空。
权限模式为 `alwaysAsk`、`askForExecution`、`allowAll`；批准回答为
`accept`、`acceptForSession`、`decline`、`cancel`。

`session.updated` 的 `settings` 是完整的公开设置（工作目录、名字、模型、权限模式）。
创建会话、PATCH 成功后各写一条。PATCH 中省略/null 表示不修改字段。
`cwd` 同时出现在公开设置和会话摘要中，供显示、选择恢复目录；PATCH 不修改它。
环境变量只在创建请求中传入，由私有文件存盘，不进入事件、公开设置或快照。
创建请求不实现 Debug，调用方也不得记录其请求体。

### 错误与 SSE

HTTP 错误统一返回 `ErrorResponse { code, message }`，不带成功回复的 eventSeq：

| code | HTTP 状态 | 用途 |
| --- | --- | --- |
| invalid_request | 400 | 请求格式、未知输入/权限模式或续传游标无效 |
| session_not_found / request_not_found | 404 | 会话或请求不存在 |
| turn_mismatch / session_closed / conflict | 409 | 目标轮不符、会话已关闭或状态冲突 |
| invalid_decision | 400 | 回答不在 availableDecisions 中（包括 Unknown） |
| internal | 500 | 服务端内部错误，message 不包含私有数据 |

`Unknown` 是旧客户端的解码兜底，不是服务端可执行的新权限或批准选项。
响应类型为 `text/event-stream`，每帧格式由 `sse_frame` 生成：

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
kind 内层为带 type 的 RequestKind，当前只有 approval 及未知种类的兜底。
批准内容包含 itemId/tool/arguments/availableDecisions，未知请求也能按外层 id 移除。
待处理请求由 `request.opened` 加入、`request.resolved` 移除；对已移除请求重复处理
不会改变批准状态。若输入历史本身包含一条新的重复处理事件，只更新其游标和时间。
`warning` 只供实时展示；最近的 `error` 和 `session.usage` 保存在快照中。
结束会话使用 `session.status: closed`，不删除存盘。

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
