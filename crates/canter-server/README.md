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
  input_history.jsonl    终端的输入历史（目录、文字、时间，最多 2000 条）；服务端不读写
  input_history.lock     几个终端写输入历史时轮流用的文件锁
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
`auto_compact_tokens` 是可选的正整数：上下文占用超过它就自动压缩（压缩后保留多少用户原话也按它算）；
不写时压缩线是窗口减输出预留再减 16,384，比这条线大时也按这条线。和窗口一样启动时读，改了重启服务端生效；
写 0 报错退出。生效的压缩线随用量发给客户端（`TokenUsage.autoCompactTokens`）。
`model_max_retries` 是可选整数，默认 10，范围 0～100（不含首次请求，0 禁用重试）；
服务端启动时读取，修改后重启生效。限流、5xx/过载、网络和超时共用模型层重试；
退避从 0.5 秒翻倍、最多 32 秒并带抖动，`Retry-After` 优先、最多 60 秒。Esc 可取消等待。
`auto_wake_limit` 是可选非负整数，默认 10，0 禁用自动叫醒且不出暂停卡片，服务端启动时读取。
模型后台命令自然结束或 monitor 匹配新输出时，空闲会话在两秒合并窗口后开一轮；忙时附到下次请求。
这一轮结束仍未送出的叫醒事件，重新安排两秒合并窗口；用户命令轮、手动压缩轮同样处理，Esc 打断或排空不重新安排。
用户消息优先且重置连续叫醒计数；达到非零上限写入事件卡片，后续通知保留给下次输入。
不能叫醒时，同一命令尚未送出的匹配继续合并在有界缓冲里，下一次请求或叫醒时一次取走。
用户后台命令只有挂着 monitor 时自然结束才叫醒；显式停止和服务端关闭不叫醒。
重启不恢复监视条件或自动开轮；历史事件卡片和已经记录的模型通知仍可恢复。
`models` 是可选的模型名数组，省略时为 `deepseek-flash`、`deepseek-v4-pro`；
默认模型 `model` 不在列表中时，自动加在最前。`GET /v1/models` 返回该列表。
`image_models` 是可选的收图片模型名数组，省略时只有 `deepseek-flash`；空数组表示全部不收。
它不增加可用模型，模型仍须列在 `models` 或作为默认模型；列表接口用可选的
`inputModalities: ["text", "image"]` 或 `["text"]` 告诉终端能否附图。
图片只接收本机路径，按内容签名识别 JPEG、PNG、GIF、WebP，每张最多 8 MiB。
接收消息前把原始字节保存为 `sessions/<id>/image-<随机名>.<格式>`，文件 0600，
随会话长期保留；事件及私有模型历史只存副本文件名，读取时按当前会话目录拼路径。
源文件删除、整个状态目录搬移或服务端重启不影响后续请求；副本丢失或不可读时，
用 `[image unavailable: 文件名]` 替代并发 warning，仍继续本轮。
切到不收图的模型时，请求临时用 `[image omitted: 文件名]` 代替，历史引用不变。
普通与压缩请求都从最新图片向前累计 base64 大小，只发送 40 MiB 预算内最近的图片，
超限的旧图临时换成同样的 omitted 占位，存盘历史不变。这为请求中的文字和结构留出空间，
不是整个 HTTP 请求体的大小上限。原图读取、格式和大小校验在会话锁外完成；
拿锁后重新检查模型及会话状态，写副本与事件，消息拒绝时清理本次新增副本。
终端粘贴图片路径会生成可删除的占位；macOS 的 Ctrl+V 通过系统 `osascript` 调用
AppKit，只读取剪贴板已有的 PNG 表示，不转换格式。临时截图文件在本终端退出／换会话时清理。
非本机 `http://` 地址被拒绝，本机仅接受 localhost 或 loopback IP；模型客户端禁用代理和跳转。
模型、key、窗口在服务端启动时读取，修改后需显式重启。
`plan_enabled` 是可选布尔开关，默认 `true`；设为 `false` 不向模型提供 `update_plan`，
终端也不显示清单，压缩不附清单。改开关后，新建会话马上生效；服务端已经加载的会话
保持原工具列表，要等服务端重启后恢复会话才会重新读取。退出终端再进入、或另一个终端
连上同一会话，都不会刷新开关；API 显式关闭会话再恢复时也会重新读取。没有斜杠开关。
创建会话的 `model` 为空字符串时使用配置中的默认模型；非空则使用请求里的模型名。
创建和 PATCH 只接受列表内的模型，拒绝时返回 `invalid_request`，不修改设置。
已存的旧模型名仍可恢复并继续对话。终端 `/model` 列出可用模型，`/model <名字>` 直接切换。
`/think` 选择 `none`、`low`、`high`、`max`，`/think <档位>` 直接切换；
下一次模型请求生效，信息行显示模型和档位，`/new` 沿用二者。
旧会话的模型已不在列表时，`/new` 在创建被拒后改用默认模型重试一次，并在新会话中提示。
未知的旧档位在终端按 `high` 显示和选择，`/new` 交由服务端使用默认 `high`。

### 用量和美元花费

`/btw <问题>` 复用当前上下文单独问一次；主线正在运行时也可发起。问答只在发起的
终端显示，不存盘、不广播、不执行工具。Esc 优先取消 btw，下一次 Esc 才打断主线。
服务端同一会话只接受一个 btw；空问题或重复提问不会调用模型。
btw 已收到的用量计入本会话和跨会话汇总（取消后也记），不计入主线轮，不更新 ctx。

`/handoff [方向]` 通过 `POST /v1/sessions/{id}/handoff` 复用这条私有请求和记账流程，
只允许已有对话且主线空闲时生成；生成中拒绝新的主线输入、压缩、设置更新和 btw。
终端收完说明后沿用 `/new` 创建相同设置的新会话，把说明放入输入框供审阅、修改和
手动发送。取消、出错或空回答留在原会话；原会话不关闭，也不增加交接关系或存盘格式。

`/usage` 在对话中按模型列本会话的缓存命中输入、未命中输入、输出 token 和花费，
再列合计及最近已结束的一轮花费（包括压缩轮）。`canter usage` 经服务端汇总所有会话，
按服务端本机时区的事件日期和模型列出，最后一行合计；没有用量时显示 `No usage recorded.`。
服务端没运行时，CLI 沿用正常启动流程自动启动；CLI 不读取会话文件。

程序自带以下美元／百万 token 单价，取自
[DeepSeek 价格页](https://api-docs.deepseek.com/quick_start/pricing/)（2026-10-25）：

| 模型 | 时段 | 命中输入 | 未命中输入 | 输出 |
| --- | --- | ---: | ---: | ---: |
| deepseek-flash | 高峰 | 0.006 | 0.30 | 1.20 |
| deepseek-flash | 非高峰 | 0.003 | 0.15 | 0.60 |
| deepseek-v4-pro | 高峰 | 0.044 | 1.32 | 3.96 |
| deepseek-v4-pro | 非高峰 | 0.022 | 0.66 | 1.98 |

按 UTC 星期一至星期五的 `[01:00, 04:00)`、`[06:00, 10:00)` 算高峰，其余算非高峰，
周末全天非高峰。不维护中国节假日表：落在周一至周五的节假日仍按工作日时段计算，
因此那些时段会多估。每次模型请求（含压缩）使用发出请求的时刻选价，
金额为 `命中 × 命中价 + (输入 − 命中) × 未命中价 + 输出 × 输出价`，再除以百万。
思考 token 已包含在输出中，不重复收费；不收缓存写入费。

`config.json` 可加 `prices` 对象，按模型名覆盖内置价或为自定义模型设置价格，例如：

```json
{
  "prices": {
    "deepseek-flash": {
      "peak": { "cache_hit": 0.006, "cache_miss": 0.30, "output": 1.20 },
      "off_peak": { "cache_hit": 0.003, "cache_miss": 0.15, "output": 0.60 }
    },
    "custom-model": {
      "peak": { "cache_hit": 0.10, "cache_miss": 1.00, "output": 2.00 }
    }
  }
}
```

每个覆盖项的 `peak` 三档必填，金额必须非负且有限。`off_peak` 可省略：省略时全天
使用该项的 `peak`，不会继承内置非高峰价。省略整个 `prices` 就只使用内置价；
价格配置不会增加可用模型。与其他配置一样，修改需显式重启服务端才生效。

每次请求的实际模型、发出时间和金额随用量事件存盘；没单价的请求记为未知，
之后改价格也不改变已记的金额或未知记录。旧事件缺少请求记账字段时，汇总按当前
价格、该用量事件时间和此前最近一条 `session.updated` 的模型补算，只读不回写。
统计用每条事件的单次请求用量，不重复累加其中的会话累计 token。
请求中途被打断或出错、没收到用量的，不计入用量和花费，可能少估。

状态栏第一行最后显示本会话花费；没有用量时不显示。有未知请求时显示已知部分加
`+?`（如 `$0.12+?`）。用量表里全未知的模型显示 `unknown`，混有未知金额的合计也加
`+?`。三处统一用美元：大于零但不到一美分显示 `<$0.01`，其余保留两位小数。

启动先取得服务端锁，普通第二次启动直接退出，不动已有 socket 或登记文件。
拿锁后清理旧登记和 socket，以及 `sessions/.creating-*` 中能取得事件文件锁的目录；
没有事件文件、拿不到锁或不是普通目录/文件的残留保留。
登记文件用临时文件加 rename 发布，只有 socket 已监听且退出处理已准备好才发布。
发布后向 stdout 打一行 `canter server ready · <版本> · pid <进程号>`，版本同 `canter --version`；
客户端自动启动和 `--replace` 起的新服务端把这行落在 `server.log`，`--replace` 的父命令再把同一行打到终端。
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
检查只短暂取得共享锁，服务端持有独占锁。启动抢独占锁遇到竞争时，用共享锁区分短暂
持锁的检查者／其他启动者和已有服务端：能取得共享锁时释放它，每隔 1 ms 重试独占锁，
最多等 2 秒，到期报锁等待超时；已有服务端时仍拒绝普通启动，显式升级仍走原有交接流程。
这样发现检查不会被误认为另一个服务端，也不会因共享锁迟迟不释放而无限空转。

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

执行新版本的 `canter serve --replace` 才要求替换：父命令复用客户端后台启动机制起新进程，
等新登记可连后在终端打出已就绪行并退出；没有旧服务端时也一样。新进程先验证配置，再在私有
状态目录写入针对旧 `instance` 的交接请求；旧进程每 200 ms 检查一次，不用 PID 信号通知升级，
不新增 HTTP 命令。新进程继续等待同一把服务端锁，旧进程释放前不能发布新登记。

旧进程停止接受创建、恢复、送话、压缩和修改设置，继续处理快照、事件、批准、打断和关闭。
等已接受的工作空闲，最多 60 秒；到时对所有打开会话并发 await `close_for_shutdown()`，活动轮按核心
逻辑记为 interrupted，排队输入清理，工具退出后释放会话锁。SSE 随后关闭，HTTP 连接
另给 5 秒收尾，避免未传完的请求体无限拖住退出。新进程等锁最多 75 秒，超时只报错，
不会强杀旧进程。新版启动后会话仍需客户端 resume 重新交环境。

SIGINT（Ctrl-C）、SIGTERM、SIGHUP 共用退出路径，直接 await 所有会话 close_for_shutdown；升级排空
期间仍持续接收这些信号，收到即跳过剩余的 60 秒等待。不安装系统服务、不改 PATH。

## MCP

`config.json` 的可选 `mcp_servers` 按名字登记服务，省略时不增加工具。例如（命令和地址由用户提供）：

```json
{
  "mcp_servers": {
    "local": {
      "transport": "stdio",
      "command": "/absolute/path/to/mcp-server",
      "args": ["--stdio"],
      "env": {"SERVICE_KEY": "<local-key>"}
    },
    "remote": {
      "transport": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": {"Authorization": "Bearer <remote-key>"}
    }
  }
}
```

stdio 直接执行命令，不经 shell；`args`、`env` 可省略。子进程先清空环境，只从服务端复制
已有的 `HOME`、`PATH`、`USER`、`LOGNAME`、`SHELL`、`TMPDIR`、`LANG`、`LC_ALL`，
不继承 `CANTER_HOME` 或其它变量，再用配置里的 `env` 增加或覆盖。
客户端后台启动的服务端环境基本为空；`command` 写绝对路径，需要的 `PATH`、`HOME` 写进 `env`。
HTTP 使用 Streamable HTTP，`headers` 可省略；仅 localhost / 回环 IP 允许 HTTP，其余必须
HTTPS，不走代理，不跟随重定向，不做 OAuth。配置修改在服务端下次启动时生效。
环境变量和请求头只保存在私有配置与运行内存中；配置解析、连接及协议错误不回显配置，
子进程 stderr 不转发到日志。脱敏候选跳过上述 8 个基础环境变量的配置值；其它 `env` 和
全部请求头的配置值及其按空白切开的每一段，长度至少 8 字节时，在工具文字结果中替换为
`[redacted]`；短值不替换。

启动时并发连接；首次连接（握手和工具列表合计）默认限 10 秒，后续每次列表更新同样限时，
工具调用默认限 120 秒。每个服务可选配 `startup_timeout_sec`（连接和列表）、`tool_timeout_sec`
（单次调用），整数秒，1～86400，超出范围启动报错（名字照 Codex）。失败连接的收尾由 registry 跟踪，不阻塞其它服务就绪；退出时等待收尾。
首次运行 `npx` 下载依赖可能超过 10 秒连接期限，可先完成安装再启动服务端，或给它配大一点的 `startup_timeout_sec`。
单个服务失败不阻止 canter 或其它 MCP 服务启动。断开的进程和 MCP 会话不自动重建；
HTTP SSE 流的断线恢复仍由 SDK 的传输层处理，MCP 会话过期自动重建已关闭。
单次工具调用失败只返回工具错误；连接实际结束时才标为断开。
HTTP 远端中途挂掉时，`/mcp` 仍可能显示已连接，以调用结果为准。
正常退出、启动期间退出和 `serve --replace` 都等待 MCP 连接关闭，清理所起进程的原进程组；
与 shell 相同，自行脱离该进程组的后代不在进程组清理范围内。

`/mcp`（`GET /v1/mcp`）显示当前连接、失败原因、工具和跳过原因，不返回启动参数或凭证。
模型工具名是 `mcp__服务名__工具名`，只接受 ASCII 字母、数字、下划线、短横线，
合计至多 64 字节，原服务名和工具名不能为空；不满足要求或重名的工具跳过。
内置工具顺序保持不变，MCP 工具按完整名字排序。说明和参数 schema 沿用 MCP 定义。
每个会话创建或重新打开时冻结工具列表；`notifications/tools/list_changed` 只更新当前目录，
已打开会话继续使用原列表。连接断开后，原会话调用该工具得到错误结果。

MCP 工具始终算执行；alwaysAsk / askForExecution 要批准，allowAll 直接调用。
「本会话都允许」只授权当前服务的当前工具；`/review` 一律拒绝，不弹批准框。
文字输出沿用 shell 的模型 32 KiB 头尾预算和客户端 1 MiB 上限；非文字内容只显示占位行。
Esc 会取消调用并由 SDK 发出取消通知；工具错误、超时和断线均作为工具错误返回，继续这一轮。
MCP 没有添加资源、提示词模板、采样或 elicitation 能力；网页搜索仍经 MCP。

## 网页抓取（P2-13）

内置 `web_fetch {url}` 由服务端执行，不增加 config.json 配置项；固定接在 ask_user 后、
MCP 工具前，仍在会话创建/打开时冻结。HTML 转 Markdown，文字/JSON 原样，其它内容只留
类型和大小占位；下载上限 5 MiB，总超时 30 秒，输出沿用模型 32 KiB / 客户端 1 MiB 截断。
Esc 取消请求；HTTP/连接/证书/超时错误交回模型，本轮继续。
HTML 解析前线性预检：粗估嵌套至多 256、单标签至多 4096 字节；超限或不能确认安全时，
不转换，将原始 HTML 加英文说明后按现有输出上限返回。通过预检后在独立的 256 MiB 栈
线程转换，经 channel 返回；超时/Esc 不等该线程，照旧返回工具错误。
非 identity 的 Content-Encoding 只给占位；声明非 UTF-8 charset 时注明仍按 UTF-8 解码。

独立客户端固定 `no_proxy` 和 `User-Agent: canter/0.1 web_fetch`，不带模型 key、MCP 请求头、
cookie 或会话环境凭证。只收 HTTP(S)，拒绝网址中的用户名/密码、反斜杠、空白和控制字符；
规范化不同于原串时通过 resolvedUrl 显示实际网址，批准说明写明具体主机及有效端口。同主机及有效端口内最多
跟随 5 次跳转，其它目标只报告新网址，由模型重新调用并批准。
alwaysAsk / askForExecution 询问，allowAll 直接访问；会话授权只覆盖一个主机及端口。
`/review` 一律拒绝且不弹框。本机和内网不额外限制，allowAll 可直接访问内部接口或触发
有副作用的 GET；需要逐个把关时使用询问模式。完整边界见 `canter-tools/README.md`。
