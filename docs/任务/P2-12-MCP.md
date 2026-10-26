# 任务：MCP

2026-10-26，canter/main 交给 canter/dev-mcp（Codex，gpt-6-astra / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 1.0）、要、碰要害）
类型：功能变更
依据：第二期第 12 件（`docs/任务/P2-0-第二期需求单.md`）。本轮让 canter 能接 MCP 服务：在配置里登记本机程序（stdio）和远端地址（HTTP）两种 MCP 服务，服务端启动时连上，把它们的工具交给模型，调用照权限模式批准；终端加 `/mcp` 看连接状态和工具。不做 OAuth 登录，不做 MCP 的资源、提示词模板等工具以外的能力，不做网页抓取（P2-13）。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-4
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-24 勾选第二期范围时：「另外装成系统服务、用量和花费统计、MCP、网页搜索/抓取」；网页搜索「经 MCP，自己只做抓取」。
- P2-0 需求单：「MCP：在配置里登记 MCP 服务，它们的工具交给模型，调用照权限模式批准」；缓存注意点：「工具列表顺序固定；会话中途 MCP 的工具变了怎么办，分解时定」。
- 10-25：主控提了 5 条建议（本机程序和远端地址都接、远端不做 OAuth 只用配置里的 key；每个服务在配置里一条，启动时连，连不上的一行报错、其它照常；MCP 工具一律照权限模式批准，不算只读，不能事先放行；会话打开时定下工具列表，中途变了下次打开才生效；加 `/mcp` 看状态和工具），用户：「都照你的建议，写 P2-12 任务文件给我看」。
- 10-26：主控把「怎么算做完」第一条给用户看，用户：「派，P2-29 也补全给我看」。

## 先读

- `CLAUDE.md`：「语言与依赖」「账号和花钱」「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/DESIGN.md` 第 8 条（协议，和 Codex 不同的地方要写进去）、第 14 条（权限）、第 15 条（状态目录和 `config.json`）、第 17 条（照顾缓存命中，开头只在打开会话时定）。
- `crates/canter-server/src/config.rs`（配置怎么读、怎么校验、错误不回显内容）、`crates/canter-server/README.md`「状态与配置」。
- `crates/canter-tools/src/lib.rs` 的 `Tool`、`Risk`、`ToolDefinition`；`permission.rs` 的 `evaluate`、`evaluate_review`、`resolve`（「本会话都允许」按工具记）；`crates/canter-tools/README.md`「权限与执行器边界」。
- `crates/canter-core/src/runner.rs` 工具调用怎么准备、过闸门、执行、取消；`context.rs` 请求里的工具列表从哪来；`crates/canter-core/README.md` 里「创建或打开会话时读一次」那段（AGENTS.md、技能就是这么定的）。
- `crates/canter-tui/src/commands.rs`、`app.rs`：斜杠命令怎么加；P2-8 的 `/context` 怎么查服务端、怎么显示。
- MCP 规范（只读参考）：工具的列出、调用、`notifications/tools/list_changed`，stdio 和 Streamable HTTP 两种传输。Codex 的 MCP 客户端可以只读参考它怎么给工具起名、怎么配。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-12-mcp`，分支 `p2-12-mcp`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-12-mcp`。
- 可以动协议、核心、工具（权限）、服务端、终端和相关测试、README、`docs/DESIGN.md` 第 8、14、15 条、本任务文件末尾的完成记录。

## 要做的

- **依赖**：用 MCP 官方的 Rust SDK `rmcp`（10-25 最新 3.7.0，只开客户端、子进程、Streamable HTTP 客户端用 reqwest 这几项功能），版本你核实后钉死，和现有 tokio、reqwest 共用。除 `rmcp` 和它自己带进来的，还要加别的依赖，停下来问。
- **配置**：在 `config.json` 里加一项登记 MCP 服务，每个服务一条，有名字；本机程序写命令、参数、要加的环境变量；远端写地址、要加的请求头（放 key 用）。远端地址照模型地址的规矩：不是本机的必须 HTTPS。配置项名字和格式你定，写进服务端 README 和 DESIGN 第 15 条。不写这一项时和现在一样。
  - 配置里的环境变量和请求头可能是 key：不出现在日志、事件、错误信息、测试快照里。
- **连接**：服务端启动时连上全部登记的服务，连接有超时；连不上、中途断开的，记下一行原因（不带 key），其它服务和 canter 照常。断开后要不要自动重连你定，写进完成记录。服务端退出、`serve --replace` 时把起的子进程一起关掉，不留孤儿。
- **交给模型**：
  - 工具名加服务名前缀，名字要满足模型接口的要求（字符、长度），重名、不合法的跳过并记一行原因；工具说明和参数原样转给模型。
  - 一个会话的工具列表（内置工具 + MCP 工具，顺序固定）在创建或打开会话时定下来，和 AGENTS.md、技能一样：会话中途 MCP 服务的工具变了、连上了、断开了，这个会话的请求不变，下次打开会话才生效。会话里的工具断开时，调用它给模型回一条错误结果，不卡住这一轮。
  - 调用结果的文字内容回给模型，照现有工具的输出上限截断；图片等非文字内容回一行占位说明（不在本轮接进图片输入）。MCP 返回的错误作为工具错误回给模型。调用有超时；Esc 打断时取消正在跑的 MCP 调用。
- **权限**：
  - MCP 工具一律算「执行」：一直问、问执行两种模式下弹批准框，全都允许模式下直接跑。不算只读，也不提供在配置里事先放行某个工具。
  - 批准框写明是哪个 MCP 服务的哪个工具，带参数。
  - 「本会话都允许」只放行这一个 MCP 工具（服务名 + 工具名），不放行同一服务的其它工具，也不放行别的 MCP 工具。
  - `/review` 的审查轮里 MCP 工具一律拒绝（`review is read-only`），不弹批准框。
- **`/mcp`**：终端加 `/mcp`，列出每个登记的服务：名字、连上没有（没连上的写原因）、有哪些工具。服务端加一个查询接口给它用，写进 DESIGN 第 8 条。加进 `/` 命令列表和 `/help`。

## 怎么算做完

- 在 `config.json` 里登记本机程序和远端地址两种 MCP 服务，canter 启动时连上，它们的工具交给模型，调用照权限模式批准；`/mcp` 能看到每个服务连没连上、有哪些工具。
- 验证只做这些（碰要害一档，含边角用例）：
  - 针对验收写测试，先写、确认在现在的代码上失败：用假模型和自己起的假 MCP 服务（本机程序一个、本机 HTTP 一个），登记后两种服务的工具都出现在请求里、名字带服务前缀、顺序固定；模型调用后结果回给模型（文字截断、非文字占位、MCP 报错作为工具错误）；三种权限模式下批准行为对，「本会话都允许」只放行那一个工具，审查轮拒绝且不弹框；一个服务连不上时其它照常、`/mcp` 显示原因；会话中途服务的工具变了，这个会话的请求不变，重新打开后才变；会话里的服务断开时调用回错误、这一轮不卡；Esc 打断时 MCP 调用被取消；超时；服务端退出和 `serve --replace` 后假 MCP 子进程不在了；配置里的环境变量和请求头值不出现在日志、事件和错误里；不是本机的 HTTP 地址被拒绝。
  - 做完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的 `CARGO_TARGET_DIR`）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不做 OAuth 登录、MCP 的资源、提示词模板、采样、elicitation 等工具以外的能力；不做网页抓取工具；不在 canter 里内置任何 MCP 服务。
- 不调真实模型接口，不连真实的外部 MCP 服务；测试用自己起的假 MCP 服务（本机程序和本机 HTTP 各一个）。不读用户真实的 `~/.canter` 和配置；测试用隔离的 HOME、`CANTER_HOME`、临时端口和 socket，不碰用户正在跑的服务端（别读、别停它的 pid）。
- 不改内置工具的权限、只读白名单和已有会话的请求内容；不改存盘格式里已有字段的意思。
- 不升级已有依赖。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务端和假 MCP 服务用起的时候记下的 PID。
- 碰到要拿主意的事（包括要动上面范围以外的文件、协议要改已有字段的意思、要加 `rmcp` 以外的依赖），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-12-mcp` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- **做了什么**：固定使用官方 `rmcp = 3.7.0`，关闭默认功能，只显式开启 client、transport-child-process、transport-streamable-http-client-reqwest；复用 workspace 的 tokio / reqwest，没有升级已有依赖。新增 `mcp_servers` 配置、stdio / Streamable HTTP 连接与工具目录、会话打开时的工具快照、经原权限闸门执行的 MCP 调用、取消和超时、原进程组清理。新增 `GET /v1/mcp`、`mcpToolCall` 项目及生成的 Schema / TypeScript，终端 `/mcp`、命令列表、帮助和批准框均接通；README 与 DESIGN 第 8、14、15 条已说明。
- **验证了什么**：新增 9 个服务端端到端测试、1 个配置测试、2 个终端测试，假模型和自建本机 stdio / HTTP MCP 服务均不读用户状态。覆盖两种传输及参数/环境/请求头、分页列工具、固定顺序与原样 schema、结果截断/非文字占位/工具及协议错误、三种权限/单工具会话授权/审查拒绝（含非法 JSON 参数）、单服务故障/无效与重名工具、通知后原会话不变与重新打开更新、断线调用、Esc 对应的取消通知、真实 120 秒调用超时、首次连接总超时、正常退出/replace/握手中退出的进程回收，以及凭证不进入错误/日志/事件和非本机 HTTP 拒绝。先后确认了配置忽略 MCP、模型请求缺工具、/mcp 不派发、跳过原因未报告、子进程组残留、握手中信号退出、前缀重名未记日志、审查参数错误优先级、isError 文字标记、连接预算叠加这些失败，再修复到通过；编译或夹具错误不作为 RED 证据。
- **最终检查**：均加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-12-mcp`；`cargo test --all-targets` 最终 297 passed、0 failed，`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 均通过，`git diff --check` 通过。独立 rustc 编译的 stdio 夹具也用 rustfmt 格式化。全量中曾有一次原有 `thinking_none_low_and_compaction_preserve_their_wire_options` 在隔离服务端启动时报告已有服务端持锁；单独重跑通过，随后未改代码的全量重跑也通过。现有 discover 探锁与 Lease.acquire 首次取锁之间可能存在竞争窗口，未证明为此次失败的唯一原因，本轮没有改该测试、探锁或锁获取代码，也没有放宽断言、缩短超时。
- **拿主意的地方**：配置按服务名映射，transport 用 stdio / http；名字采用 `mcp__服务名__工具名`，非法或碰撞时跳过，保持确定顺序。首次连接含握手和列工具共用 10 秒预算，之后列表刷新每次 10 秒，调用 120 秒；失败连接的收尾仍由 registry 持有并在退出时等待。不自动重建断开的子进程或 MCP 会话，HTTP SSE 流恢复沿用 SDK，过期会话自动重建关闭。配置错误和协议/连接错误只返回安全类别，子进程 stderr 不转发，返回文字中的该服务配置值脱敏。MCP 工具始终按执行批准，只授权完整工具名；不信任 readOnlyHint。
- **没做的事**：没有 OAuth、资源/提示词模板/采样/elicitation、网页抓取或内置 MCP 服务；真实模型调用 0 次、真实外部 MCP 服务连接 0 次。未安装、重启用户服务端、合并 main 或推送。进程清理沿用原进程组边界，主动自行脱离进程组的后代不在范围内，已在 README 写明；本轮不扩展为操作系统级后代收容。上面的偶发启动争锁未作为本任务顺带修复。
- **踩过的坑**：SDK 子进程传输默认继承 stderr，且关闭直接子进程不足以清理它再起的程序，因此改为丢弃 stderr 并持有独立进程组的清理守卫；最初把信号处理放在 MCP 握手之后，启动中退出测试暴露了回收空隙，已提前注册信号并等待连接收尾。分别给握手和初次列工具 10 秒会超过客户端 15 秒启动等待，改为共用总预算，并把失败连接的关闭任务纳入 registry 跟踪。crates.io JSON 接口返回 403，改用 `cargo info rmcp` 核实并下载 3.7.0，再读 SDK 源码核对接口。
- **只读参考**：[MCP tools 规范](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)、[stdio / Streamable HTTP 规范](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)；本机下载的 rmcp 3.7.0 `service/client.rs`、`transport/child_process.rs`、`transport/streamable_http_client.rs`；`/tmp/r49-src/codex` 的 `rust-v0.163.0`，只读其 `app-server-protocol/src/protocol/common.rs`、`v2/mcp.rs`、`v2/item.rs` 核对协议差异。没有复制第三方源码。
- **要主控决定的事**：本任务没有未决项；原有启动争锁的偶发失败已如实留下记录，供后续独立排查。

### 交叉审查返工说明（2026-10-26）

依据主仓库 `docs/任务/P2-12-交叉审查.md`「审查意见」及主控决定，修改第 1—4 条，第 5、6、7 条不改。

- **主控决定与改动**：① HTTP 客户端加 `.no_proxy()`，和模型客户端一致，README 明确不走代理。② 脱敏收集每个配置值及其按空白切开的每一段，只替换长度至少 8 字节的候选；先替换长值并去重，短值不再改坏正文，单独回显的 Bearer token 也会替换。③ stdio 用 `env_clear()`，只复制服务端已有的 HOME、PATH、USER、LOGNAME、SHELL、TMPDIR、LANG、LC_ALL，再叠加配置里的 env，不继承服务端的 CANTER_HOME；README 写明客户端后台起的服务端环境基本为空、command 用绝对路径、需要的 PATH/HOME 放进 env，以及首次 npx 下载可能超过 10 秒连接期限。④ 单次调用失败只回工具错误，不再清空 peer；连接监视任务确认结束后才设断开，列表刷新失败也先关闭连接再设断开。README 和 DESIGN 同步这些约定。
- **验证了什么**：新增 5 个端到端测试，并将原来的 503 假断线改为杀掉测试自己启动的 stdio 进程。5 个新测试先逐项确认目标失败：环境代理收到 1 次连接、短值把 `stdio result` 改坏、token 单独回显未脱敏、非白名单变量进入子进程、一次 503 后 connected 变 false；修复后均通过。最终确认代理收到 0 次连接且本机 HTTP MCP 调用成功；短值保留正文、token 从模型结果和事件中移除；非白名单变量和服务端 CANTER_HOME 不继承，基础变量存在才复制，配置 env 可覆盖；一次 503 后仍 connected，新会话保留工具并能成功调用；真实断线返回 `MCP service disconnected`，这一轮 Completed，已有会话工具列表不变。
- **最终检查**：只选跑新增及直接相关的 11 个 MCP 测试，11 passed、0 failed；覆盖上述返工及原有输出截断/错误脱敏、通知与会话快照、取消、stdio/HTTP 调用和退出/replace/握手中退出的进程回收。命令为 `cargo test -p canter --test server mcp:: -- --skip mcp::permissions_grant_only_one_tool_and_review_never_asks --skip mcp::connection_and_call_timeouts_are_bounded_and_cancel_the_request --skip mcp::colliding_qualified_names_are_skipped_and_reported --test-threads=1`；`cargo clippy -p canter --test server -- -D warnings`、`cargo fmt --check`、stdio 夹具的 `rustfmt --edition 2024 --check`、`git diff --check` 均通过。所有 cargo 命令均前置 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-12-mcp`，全部等待前台结束。
- **没做的事**：按主控决定不改第 5 条的真实 120 秒超时测试、不改第 6 条的重名日志、不改第 7 条的启动登记窗口；本轮不跑全套，也没有重跑这三项对应测试。未升级依赖、调用真实模型、连接外部 MCP、读取用户真实配置、操作用户服务端、合并 main 或推送。代理和非白名单环境变量仅注入测试自己的隔离服务端，没有改测试进程或用户环境。
- **踩过的坑与未决项**：没有新坑；复现与主控诊断一致，没有额外方案取舍，没有需要主控再决定的事。
- **复核 A/B 补充（2026-10-26）**：按主控决定，A 收集脱敏候选时跳过 STDIO_BASE_ENV 中 8 个名字的配置值，其它 env 和全部请求头仍按原规则处理，新增 HOME 路径回显测试先复现 `[redacted]/project/file.txt` 再修复通过，README、DESIGN 同步；B 只在 README 写明 HTTP 远端中途挂掉时 `/mcp` 仍可能显示已连接，以调用结果为准，不改连接状态代码；本轮只跑 HOME 回显、短 env、Bearer token、输出截断及错误脱敏 4 项测试，全部通过，未跑全套，无新坑或未决项。

## 主控审查

- 第一轮 `06a4596`：范围和任务书一致；主控在开发 worktree 重跑全套 297 个全过。
- 交叉审查（`canter/review-mcp-1`，Claude Code opus xhigh）：改完再合并。必须改 1 条：HTTP 客户端走环境代理，从终端起服务端且设了 `http_proxy` 时，本机明文地址的请求头（key）会明文交给代理。建议改 3 条：脱敏太粗（短值弄乱结果、`Bearer` 后的 token 抹不掉）；stdio 子进程环境随启动方式变（后台起的没有 PATH，终端起的带上整个终端环境）；一次 HTTP 失败就永久标断开。主控全部采纳：不走代理；8 字节以上的值和分段才脱敏；子进程只给 8 个基础变量加配置里的 env；只有连接真结束才标断开。另 3 条（超时测试真等 120 秒、重名日志重复、启动窗口拉长）不改。
- 返工 `3fc13d6`，复核可以合并，另提 2 条建议：A 基础变量（HOME、PATH）也被脱敏，会把结果里的路径抹掉，主控采纳；B HTTP 远端挂掉后 `/mcp` 仍显示已连接，主控决定只在 README 写明。`9d211b9` 照改，主控看过 diff。
- 合并前把 main（含 P2-29）并进分支，跑 `cargo test --all-targets` 305 个全过，`clippy -D warnings`、`fmt --check` 通过。
- 结果：返工 1 次

## 用户试用

- 10-26 装 `043a43c` 后，主控在用户的 `~/.canter/config.json` 登记 3 个 MCP 服务（备份为 `config.json.bak-20261026`）：`everything`（官方测试服务，本机 stdio，经 nvm 的 npx，`env` 写 PATH、HOME）、`fs`（官方 filesystem，限 `/tmp/canter-mcp-test`）、`context7`（远端 HTTPS，不带 key）。`npx` 包事先下载好（首次下载约 13 秒，超过 10 秒连接期限）。重启服务端后 `/v1/mcp` 三个都连上，工具 16 / 14 / 2 个。
- 用户在自己的终端照清单试：`/mcp`、批准框和允许一次、「本会话都允许」只放行一个工具、非文字占位、Esc 取消长调用、子进程环境、远端 HTTPS、审查轮拒绝。用户：「我都测试了，都是可以使用的。没问题」。真调 DeepSeek 由用户自己在终端发起，次数没记。
