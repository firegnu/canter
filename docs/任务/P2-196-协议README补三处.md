# 任务：第二期 - 协议 README 补 ranch 指出的三处

2026-11-03，主控建（占号）。
待用户：做不做。

## 来由

11-03 ranch/main 告知（R56a 方案定稿时）：canter 公开文档缺三处，corral 已用保守办法绕开，不挡 ranch；要不要补由 canter 主控和用户定。主控核对代码，三处都属实：

1. `GET /v1/info` 的返回字段在 `crates/canter-protocol/README.md` 里没列出；代码里是 `serverVersion`、`protocolVersion`、`features`、`sessions`（`server.json` 里版本叫 `version`，这里叫 `serverVersion`）。
2. 子 agent 的 `agent.request.opened` / `agent.request.resolved` 和快照的 `agentRequests` 只在 DESIGN（P2-178）有一句，协议 README 没写字段形状。
3. `session.status` 的 error 什么时候出现、之后会话还能不能用，没写清；ranch 实测模型出错时那一轮 failed、会话回 idle。

## 要做成什么（草稿）

照代码在协议 README 补上这三处，只改文档。
