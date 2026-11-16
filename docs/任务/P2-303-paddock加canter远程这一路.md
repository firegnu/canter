# 任务：远程 8 · paddock 加 canter 远程这一路：转给 paddock 前先定放哪里、要不要状态

依据：P2-301 试连后，用户要 paddock 也能用 canter 的远程连接（HTTP + 令牌 + 隧道）连远程机器上的 canter。改的是 paddock，canter 不改别的仓库，所以本件先在 canter 这边记成需求单，做的时候转给 paddock 主控。
依赖：P2-301
执行：派出
待用户：什么时候转给 paddock
优先：低
说明：转给 paddock 做（用户转交），canter 不改 paddock；canter 这边要补的公开接口，另开 canter 任务。

## 用户原话

- 11-17：「再加一个任务，现在paddock也要支持现在这种远程以替代之前的那种能远程」
- 11-17（主控问「替代」指哪种后）：「我的意思是在现在的基础上加上现在这路而不是替换之前的，因为其他的agent不支持这种。」
- 11-17（主控问任务文件放哪里后）：「先写到这里，之后做的时候再转给paddock」

## 要的结果

- paddock 现在经 ssh 和远端 corral 管远程机器上的 agent（P5-90、P5-124），这一套**保留不变**，Claude Code、Codex 等其他 agent 照旧走它。
- **另加一路**：远程机器上的 canter 会话，paddock 用 canter 自己的远程连接打开，不经过 ssh 和远端 corral。

## canter 现在已经有的（paddock 可以直接调，canter 不用改）

- `canter remote list`：列出已存的远程服务端名字和网址，不含令牌。令牌只在 canter 自己的 `~/.canter/remotes.json` 里，paddock 不读它。
- `canter stats --server <名字> --json`：列出那台服务端上的会话，带会话 id、目录、最后更新时间和第一句话。
- `canter --server <名字> --resume <会话 id>` 接回一个会话；`canter --server <名字> --cwd <那台机器上的绝对路径>` 开新会话。这两个是普通终端程序，跑在本机的窗格里就行。断线续接由 canter 自己处理（P2-301 试过）。
- 隧道和令牌怎么配，见 `docs/远程连接说明.md`；隧道一直开着、断了自己重连，是 P2-302 的事。

## 开工时要定的（转给 paddock 前，主控和用户一起看）

- 这一路在 paddock 界面上放在哪里：放进现有的机器切换栏（一台机器既有 ssh 那一路，又有 canter 那一路），还是单独列出。
- 铃铛和收件箱要不要显示远程 canter 会话的状态（在干活、等批准）。现在的 canter 命令行没有按会话给出这种状态的 JSON；要的话，canter 这边另开一件补公开接口（只加命令或协议字段，按 DESIGN 写清楚），paddock 不直接读 canter 的令牌，也不自己发 HTTP 请求。
- 别的问题等用户转交时再定，本单不替用户加验收。

## 不要做

- 不改、不替换 paddock 现有的 ssh + corral 远程（用户：「在现在的基础上加上现在这路而不是替换之前的」）。
- canter 这边不在代码和文档里为 paddock 写特例（CLAUDE.md「独立」）；paddock 只用 canter 的公开命令和接口。
- paddock 不读、不复制 canter 的令牌和 `~/.canter` 下的状态文件。
- canter 主控不改 paddock 仓库；本单由用户转交。
- 文档和测试里不写真实的 IP、主机名、用户名。
