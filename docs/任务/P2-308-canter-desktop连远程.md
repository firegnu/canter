# 任务：canter-desktop 连远程服务端

依据：从 P2-300 第 4 条拆出。桌面端照终端的做法加 `--server`（在 canter-desktop 仓库做，canter 这边只提供公开接口；按 canter-desktop 那边的先后排）。
依赖：P2-299
执行：转给 canter-desktop（用户转交）
待用户：什么时候做；优先级低于 iPadOS 客户端（P2-306）
优先：低

## 用户原话

- 11-17（主控列 P2-300 剩下的 4 条后）：「这里面的拆出来写成任务。然后我理解接下来就是 推送（纯用canter客户端，不牵涉corral，padock之类的，理解我意思吧？）。嗨哟一个就是ipados的客户端任务，这个比canter-desktop优先级还要高（因为mac太重了，我想拿ipados+magic keyboard来工作）」

## 不要做

- canter 这边不为 canter-desktop 写特例；缺的接口在 canter 另开任务。
- 文档和测试里不写真实的 IP、主机名、用户名。
