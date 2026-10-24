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
`turnId` 在输入排队时可以为空。`SessionInterruptParams.turnId` 是目标轮的前置条件。
审批先到先得：后到的回答由服务端返回 `alreadyResolved: true` 和原处理事件序号，
不再产生一个处理事件。取消、超时的 `clientName` 可为空。
权限模式为 `alwaysAsk`、`askForExecution`、`allowAll`；批准回答为
`accept`、`acceptForSession`、`decline`、`cancel`。

`session.updated` 的 `settings` 是完整的公开设置（名字、模型、权限模式）。
创建会话、PATCH 成功后各写一条。PATCH 中省略/null 表示不修改字段。
初始化工作目录和环境变量只在创建请求中传入，不放入公开设置或快照；
创建请求不实现 Debug，调用方也不得记录其请求体。命令项目的 `cwd` 则按任务要求显示。

## 归并

`apply_event(&snapshot, &event)` 返回新快照，失败不会改变输入。
必须按会话连续顺序使用；错会话、序号缺口和无效的已知项目增量返回错误。
已消费序号的重复事件返回原快照。未知字段忽略；未知事件、项目和增量跳过，
但仍推进事件游标，以便继续处理后续已知事件。未知项目不加入各轮的项目列表。

`session.status` 是会话状态的唯一来源，不从项目状态推测。
`turn.completed` 携带完整最终轮，`item.completed` 携带完整最终项目，均替换已有内容。
待处理批准由 `request.opened` 加入、`request.resolved` 移除；对已移除请求重复处理
不会改变批准状态。若输入历史本身包含一条新的重复处理事件，只更新其游标和时间。
`warning` 只供实时展示；最近的 `error` 和 `session.usage` 保存在快照中。
结束会话使用 `session.status: closed`，不删除存盘。

## 重新生成

在 workspace 根目录运行：

```sh
CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p1-protocol-store cargo run -p canter-protocol --bin export_protocol
```

也可以在 `--` 后传一个输出目录。`generated/protocol.schema.json` 是 schema 类型目录，
具体类型使用 `#/$defs/Event`、`#/$defs/SessionSnapshot` 等引用。
`generated/typescript/` 是 TypeScript 类型及协议版本。schema 描述当前已知的输出种类；
跨版本客户端仍必须在解码时跳过未知种类，不能把旧 schema 当作未来事件的严格白名单。
导出测试在临时目录生成、检查关键字段并与入库产物逐字节对比。
