# 任务：第三期 - 子 agent 之间直接对话

类型：功能变更
依据：P2-177、P2-178 照 Codex v1 做成一个主 agent 带几个子 agent：子 agent 之间不通信，只和主 agent 打交道，要协调得由主 agent 拿到一个的结果再用 `send_input` 转给另一个；几个通用子 agent 只通过同一个仓库里的文件间接看到彼此。本件让子 agent 之间能直接传话。第三期做，第二期不做。
执行：派出
依赖：P2-177、P2-178、P2-179
待用户：第三期开工时再和用户定

## 用户原话

- 11-02 主控说明子 agent 之间不交换信息、都只和主 agent 交互，Codex v2 有收件箱和 `send_message`，P2-78 定了不做收件箱。用户：「子agent之间直接对话，排一个任务到第三期」

## 主控当时记下的（11-02，未定）

- 可参考：Codex `multi_agent_v2`（`spawn_agent`、`send_message`、`followup_task`、`wait_agent`、`list_agents`、`interrupt_agent`，`wait_agent` 等收件箱有更新），见 `docs/调研/P2-78-子agent调研.md` 第 1 节；Claude Code 的做法开工时再查。
- 开工时要定：谁能给谁发（子 agent 之间、子 agent 给主 agent 中途发）；消息怎么进对方的上下文（下一次请求带上，还是叫醒）；主 agent 和用户能不能看到子 agent 之间说了什么（终端列表、记录）；会不会让子 agent 互相等、绕圈子，怎么防；和现在 `wait_agent` / `send_input` 的关系，要不要换成收件箱。
