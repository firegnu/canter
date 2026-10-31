# 任务：第二期-13 - subagent支持

2026-10-27，用户在看板上新建。

## 用户原话

## 要做的

## 怎么算做完

## 主控记下的（10-27）

- 10-27 用户：「把搜网页记进P2-92， 子agent写到P2-78,applu_patch写到P2-80,盯输出、目标续跑：P2-81 长任务可靠性分解时参考 这个单独写一个task。」
- 来由：P2-88 内置工具调研、P2-94 定内置工具时，子 agent 记成「以后再说」：P2-0 定了第二期不做。主控 10-27 判断长任务做多了会想要：一个长活分几块并行查、并行改，主上下文不被撑爆。
- 别家（P2-88 报告第 1、6 节）：
  - Codex `rust-v0.163.0`：`spawn_agent`、`send_input`、`resume_agent`、`wait_agent`、`close_agent`（`multi_agent` 稳定、默认开）；v2 换成 `send_message`、`followup_task`、`list_agents`、`interrupt_agent` 等，默认关。
  - Claude Code：`Agent` 工具。opencode：`subagent`。pi：不内置，只在 `examples/extensions/` 里当示例。
- 分解时要和用户定：子 agent 跑在同一个服务端还是另开进程、能用哪些工具、批准怎么走、和 corral 派活（P2-83）的分界。
