# canter-server

本机 Unix socket 上的 HTTP + SSE 服务端；可执行入口为 `canter serve`。
只参考 Codex rust-v0.163.0 的 app-server-daemon README，以及 opencode v2.1.3 的
`packages/cli/src/commands/handlers/default.ts`、`packages/client/src/effect/service.ts`，
没有复制源码。HTTP 对照和错误格式见 `canter-protocol/README.md`。

## 状态与配置

默认状态目录是 `~/.canter`，`CANTER_HOME` 可指定另一个专用的绝对路径；启动时解析为实际路径。
目录 0700；配置、登记、锁、日志文件 0600，socket 也设为 0600。
不在其他程序的目录里读写；测试全部使用 `/tmp` 下的独立目录及隔离 HOME。

```text
<CANTER_HOME>/
  config.json            模型配置；只从此文件读取 key
  server.lock            整个服务期持有的文件锁；不删除锁文件
  server.json            pid、version、protocolVersion、socketPath、instance
  server.sock            HTTP + SSE
  server.log             客户端自动启动时收集 stdout/stderr
  startup.lock           串行化客户端自动启动，避免竞争者重复启动并记录误导错误
  replace.lock           串行化主动升级的请求
  replace.json           主动升级时才存在；目标是完整的旧登记记录
  sessions/<id>/         canter-store 的事件和模型历史
```

用户先创建 `config.json` 并设为 0600，例如：

```json
{
  "base_url": "https://api.deepseek.com",
  "model": "<model-name>",
  "models": ["deepseek-flash", "deepseek-v4-pro"],
  "image_models": ["deepseek-flash"],
  "context_window": 1048576,
  "api_key": "<your-key>"
}
```

启动时收紧已有配置权限；缺失或无效时用不含配置内容的错误退出。配置格式为 JSON，
`base_url`、`model`、`context_window`、`api_key` 必填；窗口必须大于 81,920
（核心的输出预留 65,536 加压缩余量 16,384）。
`models` 是可选的模型名数组，省略时为 `deepseek-flash`、`deepseek-v4-pro`；
默认模型 `model` 不在列表中时，自动加在最前。`GET /v1/models` 返回该列表。
`image_models` 是可选的收图片模型名数组，省略时只有 `deepseek-flash`；空数组表示全部不收。
它不增加可用模型，模型仍须列在 `models` 或作为默认模型；列表接口用可选的
`inputModalities: ["text", "image"]` 或 `["text"]` 告诉终端能否附图。
图片只接收本机路径，按内容签名识别 JPEG、PNG、GIF、WebP，每张最多 8 MiB。
接收消息前把原始字节保存为 `sessions/<id>/image-<随机名>.<格式>`，文件 0600，
随会话长期保留；事件及私有模型历史引用此副本。源文件删除或服务端重启不影响后续请求。
切到不收图的模型时，请求临时用 `[image omitted: 文件名]` 代替，历史引用不变。
终端粘贴图片路径会生成可删除的占位；macOS 的 Ctrl+V 通过系统 `osascript` 调用
AppKit，只读取剪贴板已有的 PNG 表示，不转换格式。临时截图文件在本终端退出／换会话时清理。
非本机 `http://` 地址被拒绝，本机仅接受 localhost 或 loopback IP；模型客户端禁用代理和跳转。
模型、key、窗口在服务端启动时读取，修改后需显式重启。
创建会话的 `model` 为空字符串时使用配置中的默认模型；非空则使用请求里的模型名。
创建和 PATCH 只接受列表内的模型，拒绝时返回 `invalid_request`，不修改设置。
已存的旧模型名仍可恢复并继续对话。终端 `/model` 列出可用模型，`/model <名字>` 直接切换。
`/think` 选择 `none`、`low`、`high`、`max`，`/think <档位>` 直接切换；
下一次模型请求生效，信息行显示模型和档位，`/new` 沿用二者。
旧会话的模型已不在列表时，`/new` 在创建被拒后改用默认模型重试一次，并在新会话中提示。
未知的旧档位在终端按 `high` 显示和选择，`/new` 交由服务端使用默认 `high`。

启动先取得服务端锁，普通第二次启动直接退出，不动已有 socket 或登记文件。
拿锁后清理旧登记和 socket，以及 `sessions/.creating-*` 中能取得事件文件锁的目录；
没有事件文件、拿不到锁或不是普通目录/文件的残留保留。
登记文件用临时文件加 rename 发布，只有 socket 已监听且退出处理已准备好才发布。
正常退出移除登记和 socket；崩溃后的残留由下一次拿锁的服务端清理。

## P1-7 接入

`state_directory()` 返回默认/环境指定的状态目录。
`discover(&home)` 只查找；`ensure_server(&home, &canter_executable)` 查不到时后台启动
指定可执行文件的 `serve`，最多等 15 秒。多个客户端通过 `startup.lock` 串行启动，拿锁后
重新发现已有服务端；服务端本身仍由 `server.lock` 保证单实例。子进程用独立 Unix session，
清空继承环境，仅传 `CANTER_HOME`，stdin 关闭，stdout/stderr 写入 0600 的 `server.log`。
子进程退出且服务端锁空闲时立即返回错误，并附日志最后一个非空行，不等满启动期限。

成功返回 `Connection { registration, client, version_warning }`。使用里面的 reqwest
`client` 请求 `http://localhost/v1/...`；它固定走登记的 Unix socket，不走代理，
没有总请求超时，因此可以持有 SSE。调用者应为普通命令自行设置适当超时。
调用方显示 `version_warning`：协议兼容而版本不同仍连接旧服务端，不自动替换；
协议不兼容则返回错误。判活以 `server.lock` 为准：锁空闲则登记视为残留，即使 PID 已被
其他活进程复用，也按没有服务端处理；探活失败后再检查锁，锁仍被占用才报错，不偷偷替换。

`POST /v1/sessions/{id}/resume { "env": { ... } }` 用于接着聊，回复快照及最后序号。
未打开会话的快照和事件直接从盘读；变更命令返回 `session_not_resumed`，不存在则 404。
重复 resume 返回当前快照，不替换环境；DELETE 关闭后可再次 resume。
DELETE 的关闭事件记录 `closeReason: clientRequest`；停机关闭记录
`closeReason: serverShutdown`，未打开的会话保持原有关闭原因。快照和事件重放均保留原因，
供旁观终端在服务端重启后决定是否自动 resume；用户主动恢复仍可重开任何已关闭会话。
停机已经关闭、但尚在内存登记中的会话收到 DELETE 时，返回 HTTP 409、`code: conflict`、
`message: server is stopping`，保留 `serverShutdown`，不把这次客户端关闭报告为成功。
反过来，停机遇到已被客户端关闭的会话时直接成功，保留 `clientRequest`。
DELETE 即使关闭失败也移除内存登记、唤醒订阅者，并保留原错误回复；随后可 resume 重开恢复。
所有创建/恢复均把状态目录和配置路径传为受保护路径，不读取或合并服务端环境。
环境和请求体不记录日志，也不写公开设置、事件或模型历史；工具本身的输入输出仍照核心协议记录。

SSE 用协议包的 `sse_frame`、`resume_after`，`Last-Event-ID` 优先，15 秒空闲心跳。
未打开会话的订阅等待会话登记变化，resume 后接上核心订阅，不用空闲轮询整份事件文件。
核心订阅仍有“落后时从头重读事件文件”的已知成本，本包未改存盘或核心接口。

## 显式替换和退出

执行新版本的 `canter serve --replace` 才要求替换：新进程先验证配置，再在私有状态目录
写入针对旧 `instance` 的交接请求；旧进程每 200 ms 检查一次，不用 PID 信号通知升级，
不新增 HTTP 命令。新进程继续等待同一把服务端锁，旧进程释放前不能发布新登记。

旧进程停止接受创建、恢复、送话、压缩和修改设置，继续处理快照、事件、批准、打断和关闭。
等已接受的工作空闲，最多 60 秒；到时对所有打开会话并发 await `close_for_shutdown()`，活动轮按核心
逻辑记为 interrupted，排队输入清理，工具退出后释放会话锁。SSE 随后关闭，HTTP 连接
另给 5 秒收尾，避免未传完的请求体无限拖住退出。新进程等锁最多 75 秒，超时只报错，
不会强杀旧进程。新版启动后会话仍需客户端 resume 重新交环境。

SIGINT（Ctrl-C）、SIGTERM、SIGHUP 共用退出路径，直接 await 所有会话 close_for_shutdown；升级排空
期间仍持续接收这些信号，收到即跳过剩余的 60 秒等待。不安装系统服务、不改 PATH。
