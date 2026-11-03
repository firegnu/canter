# 任务：第二期 - 协议 README 补 ranch 指出的三处

2026-11-03，主控建。11-03 用户：「派 196」。canter/main 交给 canter/dev-protocol-readme（pi，默认模型）。
路由：低 / 交叉审查不要 / 影响面：看得见（路由：轻（置信 0.52）、不要、看得见）
类型：样式／文案调整
依据：本轮只照现有代码在协议 README 补三处说明，不改代码、测试和协议。
提示：沿用现有视觉和用语约定，聚焦指定的呈现结果。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-03 主控解释三处是什么、只改 README、canter 代码没有绕路（绕的是 corral 那边）后，用户：「派 196」。

## 来由

11-03 ranch/main 告知（R56a 方案定稿时）：canter 公开文档缺三处，corral 已用保守办法绕开，不挡 ranch。主控核对代码，三处都属实：

1. `GET /v1/info` 的返回字段在 `crates/canter-protocol/README.md` 里没列出；代码里是 `serverVersion`、`protocolVersion`、`features`、`sessions`（`server.json` 里版本叫 `version`，这里叫 `serverVersion`）。
2. 子 agent 的 `agent.request.opened` / `agent.request.resolved` 和快照的 `agentRequests` 只在 DESIGN（P2-178）有一句，协议 README 没写字段形状。
3. `session.status` 的 error 什么时候出现、之后会话还能不能用，没写清；ranch 实测模型出错时那一轮 failed、会话回 idle。

## 先读

- `crates/canter-protocol/README.md`（看现有各节的写法和用语，中英混排照它）。
- `docs/DESIGN.md` 第 8 条下 P2-178（子 agent 工具和批准）那条。
- 代码：`crates/canter-protocol/src/commands.rs`（`/v1/info` 的响应，`server_version` 附近）、`event.rs`（`AgentRequestOpened`、`AgentRequestResolved`、`SessionStatus`）、`types.rs`（`agent_requests`、`AgentRequest`、`SessionStatus`）、`reducer.rs`（这几个事件怎么归并进快照）；error 状态在服务端什么时候发出，到 `crates/canter-core`、`crates/canter-server` 里查。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-196-protocol-readme`，分支 `p2-196-protocol-readme`（已从 main 建好）。只动 `crates/canter-protocol/README.md`。
- 另有 P2-198 在分支 `p2-198-yolo-no-sandbox` 并行改沙箱，可能也改这份 README 的「沙箱状态（P2-166）」那节；你不要动那节。

## 要做的

照现有代码，在协议 README 补上面三处：

1. `GET /v1/info`：列出返回的每个字段和含义，点明版本字段在这里叫 `serverVersion`、和 `server.json` 的 `version` 不同名。
2. 子 agent 请求：`agent.request.opened`、`agent.request.resolved` 两个事件的字段，快照 `agentRequests` 每项的形状，以及回答发到哪里（子会话的请求接口）。
3. `session.status` 的 `error`：什么时候出现；模型出错时那一轮是什么状态、会话回到什么状态、之后能不能接着用。

写的是代码现在的行为。读代码发现和上面「来由」里写的不一致，照代码写，并在完成记录里说。

## 怎么算做完

- README 里能查到这三处，字段名和代码一致。
- 验证只做这些：`git diff --check`，并逐个对照代码核对你写的字段名。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改代码、测试、生成文件（`crates/canter-protocol/generated`）、DESIGN。
- 不改 README 里别的节，不顺手整理格式或用语。
- 不跑测试套件。不用 Python。
- 不改 ranch、paddock 等别的仓库，也不送话给它们。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事（包括代码行为看起来像 bug、某个字段含义读代码定不下来），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-196-protocol-readme` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：只改 `crates/canter-protocol/README.md`，补三处：HTTP 对照表后新增 `### GET /v1/info`，列 `features`、`serverVersion`、`protocolVersion`、`sessions` 的含义，并点明 `server.json` 里版本叫 `version`；「归并」里 `session.status` 那句后补六种取值、`error` 的实际行为，以及模型出错时 failed 轮、`error` 事件和回到 `idle`；待处理请求段后补 `agent.request.opened`、`agent.request.resolved` 的字段、父快照 `agentRequests` 的形状、回答接口和重开/重挂时的转发行为。
- 验证了什么：`git diff --check` 通过；逐项对照 `commands.rs`（`InfoResponse`、`SessionSummary`、`RequestPath`）、`event.rs`（`AgentRequestOpened`、`AgentRequestResolved`、`Error`）、`types.rs`（`AgentRequest`、`PendingRequest`、`SessionStatus`、`TurnStatus`）、`reducer.rs`（`agentRequests`、`error`、`session.status`）、`runner.rs`（failed 轮、`error` 事件、回 `idle`、重试）、`session.rs` 和 `agents.rs`（重开时补 cancelled、重挂时补发）、`http.rs`（`info` 字段和 `/v1/sessions` 复用 `info.sessions`）核对字段名。
- 拿主意的地方：三处放在最相关的现有段落旁；`features` 只举例、不列全，免得以后加能力就过期。读代码发现和「来由」不一致：本版服务端没有任何路径写 `session.status: error`（只写 idle / working / waiting_approval / closed），模型出错时那一轮 failed、另发 `error` 事件、会话回 idle；照代码写，`error` 只作为协议保留值，要求客户端能解码、按未关闭处理。
- 没做的事：没改代码、测试、生成文件和 DESIGN，没动 README 其它节，没跑测试套件，没合并、没推送。看到「审查轮」一节还写着「本版返回 `["review"]`」（实际 `features` 有 17 项），已过时，按任务要求没动，留给主控。
- 踩过的坑：没有。
