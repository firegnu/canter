# 任务：远程（不排）· 升级后服务端不归 systemd 管：碰到再修

依据：P2-302 在 5090 上把 canter 服务端做成 systemd 用户服务（`Restart=on-failure`）。`canter upgrade`、`serve --replace` 起的新服务端是 canter 自己起的独立进程，不在服务里；旧的正常退出后服务记为停了。所以升级以后到下次开机前，服务端崩溃不会自己重起，人不在跟前时远程就一直连不上。补上要改 canter 的代码，P2-302 不改代码，所以单记一件。
依赖：P2-302
执行：派出
待用户：先只记下来，碰到再修
优先：低
说明：真碰到了再补全、给用户看过再派。

## 用户原话

- 11-17（主控说升级后保护失效要改 canter 代码、建议先不开任务后）：「把升级后保护失效集成一个新任务（只是记一下，以后碰到再修）」

## 现在的情况（11-17 在 5090 上实测）

- `canter-server.service` 起的服务端被 `kill -KILL` 后，5 秒重起，链路 5.5 秒恢复。
- `canter serve --replace` 后：旧服务端以 0 退出，服务记为正常结束（`Result=success`），没有抢着起；新服务端 `serve --replace --foreground` 在服务外跑。
- 临时办法：在 5090 上停掉服务外的那个服务端（按 PID 发 SIGTERM），再 `systemctl --user start canter-server`。`docs/remote.md` 和 `docs/远程连接说明.md` 写了。

## 修的方向（碰到时再定）

- macOS 有 `canter service install`（DESIGN P2-14），Linux 没有（DESIGN 写明「不实现 systemd」）。改法大致是让 canter 在 Linux 上认得自己的 systemd 用户服务：装了服务时，`upgrade`、`serve --replace` 经 `systemctl --user restart` 换服务端，等转完再返回。要改 DESIGN 那一条，先问用户。

## 不要做

- 不改「一台服务端只服务一个人」。
- 文档和测试里不写真实的 IP、主机名、用户名。
