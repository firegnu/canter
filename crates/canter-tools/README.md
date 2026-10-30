# canter-tools

六个内置工具、每会话的执行器和权限判断；不依赖模型包，不发送事件、不弹批准框。
本机实现使用 Unix 进程组，面向 macOS / Linux。

六个工具的 `description` 用 P2-57 方案 §4 的原文：只属于单个工具的规矩写在这里，
跨工具的选择（看文件用 read 不用 cat）放在系统提示词固定段；参数的 JSON Schema 不变。
原文存在 `tests/data/descriptions/*.txt`，`tests/descriptions.rs` 逐字核对。
P2-130 在 read 的说明中补充了看图能力、图片格式和 8 MiB 上限。

## 第 4 件接入

1. 每个会话创建一个 `LocalExecutor::new(absolute_cwd, environment, protected_paths)`，包装为
   `Arc<dyn Executor>` 交给 `Tools::new`；另建一个 `PermissionGate::default()`。
   环境用客户端传入的完整 `BTreeMap<String, String>`，不能合并服务端环境。
   gate 不跨会话共享、不序列化，恢复会话时重新创建。`protected_paths` 是 P1-5
   提供的绝对路径列表，包含 canter 状态目录和配置文件，也可以包含尚不存在的路径。
   建执行器时解析并保存这份列表；受保护路径本身是悬空链接时，同时保护链接和目标，
   不影响普通文件工具调用，也不在每次调用时重新解析这份列表。
2. 用 `Tool::ALL` 和 `Tool::definition()` 取得名称、说明、`serde_json::Value`
   参数 Schema、`Risk`。模型给出名字后用 `Tool::from_name()` 查找；未知名字返回 `None`，
   由调用方报告未知工具。
3. 调 `tools.prepare(tool, item_id, raw_arguments, &cancel).await`。
   必须传模型给出的原样字符串。准备阶段只读文件、校验参数和计算 diff，不写文件、不起命令。
   成功返回不可修改的 `PreparedCall`；`raw_arguments()` 保留原文，供模型历史存盘。
   失败返回 `ToolOutput`，其中已有给模型的错误文字和正确种类的失败 `Item`。
4. `call.started_item()` 提供 `InProgress` 项目，供循环发送 item.started。
   `ask_user` 属于 `Risk::Interaction`：用 `call.user_input_request()` 取问题，交会话循环
   开待处理请求并等回答，所有权限模式都一样，不交给 gate，也不调用执行器。
   以下 gate/execute 步骤用于读写文件、shell、web_fetch、web_search 和 MCP 工具。
   调 `gate.evaluate(&call, mode)`。`Execute(permit)` 可执行，`Ask(ApprovalRequest)`
   交循环/客户端处理。请求包含完整参数，write/edit 另加顶层 `diff`；文件路径解析后
   与模型提供的不同时另带 `resolvedPath`，客户端应显示这个实际目标。shell 保留完整命令。
   先由循环/服务端核对待处理请求归属、重复回答，再把回答交给
   `gate.resolve(&call, decision, feedback)`。服务端用 `resolve_with_prefixes` 另传请求里已展示的会话前缀，
   不在回答时扩大授权范围。返回 `Execute(permit)`、`Deny { cancel_turn, feedback }` 或带原因的 `Forbidden`；
   未知回答返回 `invalid_decision` 错误，符合协议 README。
   服务端先校验 decision 在可用选项内、feedback 只随 DeclineWithFeedback 提供且非空白。
   拒绝时用 `call.declined_output(feedback.as_deref())` 得到 `Declined` 项目和模型文字，循环另根据
   `cancel_turn` 决定是否停止一轮。
5. 得到允许后，调用 `tools.execute(permit, &cancel, &mut on_output).await`。
   许可只能由闸门创建、不能复制，借用这一个 call；调用期间保留 call。
   call 绑定准备它的 `Arc<dyn Executor>`，交给其他执行器会报错。文件实际目标在执行前
   再检查，符号链接改了指向会拒绝执行。准备好的 call 本身不能传给 execute 绕过闸门。
   回调是 `FnMut(&str) + Send`，shell 的 stdout/stderr 文字块按收到的顺序合流，
   调用方可转成 `item.delta`；回调须快速返回，不要阻塞运行时。
   `ToolOutput.text` 给模型，`ToolOutput.item` 给客户端最终项目；`ToolOutput.images` 是已校验的图片原始字节。
   核心把图片存入会话目录后才填写项目的可选 `contentItems` 并发布，工具层不自行保存会话图片。
   `ToolOutcome::{Completed, Failed, Cancelled, TimedOut, Declined}` 供循环区分结果。
   协议没有 cancelled/timedOut 项目状态，这两种最终 `Item.status` 都是 `Failed`，
   一轮是否 interrupted 由循环根据取消信号和 outcome 决定。

准备失败的项目保留已知 cwd/path，diff 留空；改文件种类尚不确定时用 `PatchChangeKind::Unknown`。
执行失败保留已经准备好的文件种类和 diff。read 失败时 content 留空，错误只在模型文字中，
不把错误提示冒充文件内容。未新增协议字段。

取消使用 `CancellationToken`。取消后继续 await 执行结果，不要 abort/drop 写文件的 future：
它要等已经启动的文件工作线程退出。写入在提交前检查取消，原文件不会变成半份；
原子替换已经完成时返回成功。取消可能留下已创建的空父目录。
同一会话的文件修改由调用方串行执行。写入前会检查文件仍与准备 diff 时一致，
不一致就要求重新准备；这是内容前置条件检查，不是对外部编辑器的文件锁。
新文件权限按 `0666 & umask` 创建，已有文件保留权限位；悬空符号链接、不可写文件都报错。
原子替换会断开硬链接，不保留扩展属性/ACL，所有者可能改变；未增加 sync_all，不能保证断电持久化。

## 参数和结果

| 工具 | 参数 | 结果 |
| --- | --- | --- |
| read | `path`, 可选 `startLine`（默认 1）、`lineCount`（默认 2000） | 带行号的 UTF-8 文本或 JPEG/PNG/GIF/WebP 图片，`fileRead` |
| write | `path`, `content` | 整文件写，创建父目录；`fileChange` 和 unified diff |
| edit | `path`, `edits: [{oldText, newText}]` | 一次替换多处；`fileChange` 和 unified diff |
| shell | `command`, 可选 `timeoutMs`、`background`（默认 false） | 前台结果或后台编号、状态、初始输出；`commandExecution` |
| shell_output | `id`（会话内小整数） | 上次模型读取后的新输出及状态；`backgroundCommandAction` |
| shell_stop | `id` | 停整个进程组，返回剩余输出及状态；`backgroundCommandAction` |
| monitor | `id`、可选正则 `pattern` | 监视新输出；省略 pattern 取消，再设则替换；`backgroundCommandAction` |
| ask_user | `question`, `options: [{label, description}]`（2–4 个，标签不重复，内容非空） | 会话循环等待选项或自写文字；模型结果 `{question, answer}`，客户端项目 `userInput` |
| web_fetch | `url`（HTTP(S)，不含用户名/密码） | HTML 转 Markdown，文字和 JSON 原样；有界文字结果，客户端项目 `webFetch` |
| web_search | `query`（1～128 个 Unicode 字符，非全空白）、`count`（默认 10，1～20） | 可选搜索后端；统一标题、网址、日期、summary；客户端项目 `webSearch` |
| update_plan | 可选 `explanation`，`plan: [{step, status}]` | 整份替换清单，回 `Plan updated`；客户端项目 `plan` |
| submit_plan | `plan`（完整 Markdown） | 计划模式中等待用户批准；客户端项目 `planProposal` |

`Tool::ALL` 固定按 read/write/edit/shell/ask_user/web_fetch/web_search/update_plan/submit_plan/shell_output/shell_stop/monitor 排列；会话用 `Tools::find` 滤掉未配置的 web_search 和关闭的 update_plan，保证会话每次请求的工具前缀稳定。

`web_search::SearchProvider` 是搜索后端接口，返回统一 `WebSearchResult`，错误必须不含凭证。
`WebSearch` 负责 30 秒总超时、取消和统一输出；DeepSeek 适配在 `web_search/deepseek.rs`，
用独立 no_proxy 客户端发送 query/count，禁用跳转和自动重试，响应限制 5 MiB。
模型输出按现有 32 KiB 头尾截断，客户端的 1 MiB 文字预算均分给结果及字段，各自头尾截断。
搜索算执行，session grant 按工具名放行所有查询；审查轮拒绝，计划模式复用普通批准。
增加一家后端只需实现这个接口并在服务端配置层选用，工具定义、格式化、批准、协议、终端不变。
`Tools::with_plan_enabled(false)` 关闭待办；会话通过 `Tools::find` 过滤工具定义和调用，
在创建或打开时冻结。`update_plan` 属于交互风险，但 gate 始终直接发 permit，审查轮也一样，
不会调用执行器。步骤状态为 `pending` / `in_progress` / `completed`，至多一个进行中，
步骤文字不能全空白；校验失败不替换清单，空数组则清空。说明要求三步及以上才用，
每做完一步立刻调用标完成再开始下一步，不攒到最后。已有未完成清单时的定期提醒由 core
在模型工具结果末尾追加，不由工具生成，也不进入客户端项目。
`ask_user` 没有「本会话都允许」，也不超时自动选择；取消由会话循环处理。

所有相对路径按会话 cwd 解析。read/write/edit 会解析符号链接和 `..`；碰到受保护路径
本身或其子路径时拒绝，模型收到「这是 canter 的私有文件」。shell 无法靠这份列表拦住，
批准 shell 仍能访问这些文件；路径保护不是系统沙箱，也不是阻止外部进程竞态的文件锁。
write/edit 先读原文，用 `similar` 生成 diff；
非 UTF-8 原文件不能生成文本 diff，会报错。
edit 按原始文字精确匹配，不做模糊匹配、空白或换行归一化；每个 oldText 非空且恰好出现一次，
包括彼此重叠的重复出现。各替换区域不能重叠；所有检查通过后一次原子替换。
失败会指出第几处匹配有问题，原文件保持不变。
diff 在 `spawn_blocking` 中计算，匹配算法限时 200 ms，超时可生成较粗但有效的 diff；
格式化期间也检查取消。批准请求和最终项目的 diff 至多 1 MiB，保留头尾并注明省略字节数。
模型结果只返回创建/修改、增删行数摘要。模型原样参数仍完整保留，不能用截断后的 diff 重建调用。

read 每次最多 2000 行、64 KiB 带行号的内容，续读提示另计；不切开一行。
达到限制时提示下一次 `startLine`。单行本身过长时提示用 shell 分字节查看。
在同一个经执行器批准和检查的打开文件上，先按内容识别 JPEG/PNG/GIF/WebP；图片不受行参数影响，
原样读取至多 8 MiB，超限返回 `image exceeds the 8 MiB limit`，不交图。其它文件含 NUL 或读取范围不是 UTF-8 时返回二进制/编码错误。
起始行超过 EOF 时说明总行数，空文件按 0 行处理。

图片走 P2-5 的 `Store::save_image`（会话副本 0600），模型结果为
`Image "<name>" is attached in the next user message after this tool-result batch.`。
核心保存待附图记录，等整批工具结果齐全后追加一条 user 图片消息，内部标为工具来源；
压缩不将其当作用户原话，沿用同一近期消息保留预算。恢复、回退和撤回沿用模型历史的轮边界。
每张估 1024 token、请求图片 base64 40 MiB 预算、不收图模型和丢失副本的文字占位均复用 P2-5。
客户端 `contentItems` 只含 `{type: localImage, path, name, mimeType, sizeBytes}`，path 为会话副本文件名，
无图片时省略；终端每张图片一行名字、格式、大小，导出写 `[Image: 名字]`，不内联图片字节。

shell 使用 `/bin/sh -c`，非登录、非交互，stdin 关闭；先 `env_clear` 再装入会话环境。
`background=true` 等 1 秒或命令先结束后返回，不使用 timeoutMs；会话最多同时 8 条。
后台只保留最近 1 MiB，模型每次读新增输出最多最后 32 KiB，报告跳过字节数；终端读取不推进模型游标。
已完成的命令只保留最近完成的 8 条输出，更早的保留状态并说明输出已丢弃。重启后模型读取/停止同样返回已恢复状态、说明输出不可用。
`Tools::background()` 提供列表、读取、停止和按前台 item ID 转后台；Tools 由会话持有，退出/切换终端不影响进程。
`monitor` 无需批准，Plan/审查轮可用。条件只看调用之后收到的新输出，按现有 stdout/stderr 合流分行，
退出时也匹配末尾没有换行的一段；换条件重置未完成行，非法正则保留原条件。命令退出自动移除条件。
每条命令每批只留最近 20 条匹配行、4 KiB，记录跳过行数和字节数；不推进 shell_output 的游标。
会话忙、达到叫醒上限或禁用叫醒时，待送匹配继续留在同一缓冲内合并，下一次请求或叫醒时一次取走。
未完成的一行最多保留最近 1 MiB，避免无换行日志无限占内存。正则使用已有锁定版本的 regex-automata。
核心负责两秒合并、空闲叫醒、用户输入优先和次数限制；工具层只留匹配结果、命令来源及停止状态。
服务端关闭会话前等待 `stop_all()` 清理，Tools 的 Drop 也会取消任务；后台与前台均清理整个原进程组。
所有本机 shell 在 exec 前建立管道看护进程，服务端独占写端；服务端被 SIGKILL 或崩溃时，看护进程在读到 EOF 后杀掉原进程组。正常清理杀组后通知看护进程退出。
看护经过两次 fork 脱离命令，由命令回收中间进程，再 exec 短小的 `/bin/sh` 程序（标记 `canter-shell-guardian`）；不保留服务端内存或信号处理，也不成为命令要等待的子进程。进程级锁保护本机 shell 的管道创建和 spawn，exec 通过 CLOEXEC 关闭描述符，不按 nofile 上限扫描；正常清理向管道写换行让看护退出。
下面的默认超时和头尾保留规则适用于前台；转后台之后沿用后台规则。
shell 自己按其规则建立的变量（例如 PWD）仍然存在。
默认超时 120000 ms，上限 600000 ms，超限参数报错。
模型结果超过 32768 字节时保留头尾各至多 16384 字节，在 UTF-8 边界截断，
中间标出实际省略字节数；提示和退出信息另计。
客户端最终 `aggregatedOutput` 上限 1 MiB，头尾各占一半预算，超限时为省略提示预留空间，
所以连提示也不超过 1 MiB。使用有界头尾缓冲，不累积整份输出。
增量只发最初至多 1 MiB，达到上限后停止发送，最终项目提供保留的头尾并替换增量显示。
两个输出流分别增量解码，跨块 UTF-8 不损坏；非法字节按替代字符显示。
非零退出、执行中的超时和取消都返回失败项目。模型退出信息使用 `Exit code: 0`、`Killed by signal 9`。
取消先向原进程组发 SIGTERM，给最多 50 ms 清理机会，然后 SIGKILL 清理残留；超时直接 SIGKILL。
shell 正常退出也清掉本组剩余后台子进程，执行器句柄 Drop 同样尝试清理原组。
shell 回收后管道最多再读取 2000 ms；取消和原始超时在此期间保持有效，包括另一个终止原因
已触发之后。停止读取时在模型和客户端最终文字中注明后台进程仍占着管道及停止读取的原因。
如果 shell 已自行退出，收尾期间的超时或取消只停止读取输出，结果按实际退出码返回；
如果命令先前因超时或取消被终止，结果仍保留首次终止原因。
`set -m`、`setsid` 等移到其他进程组或会话的进程不在清理范围内，可能继续运行。
第一期只清理原进程组，不提供操作系统级的后代进程收容；第二期的执行器/沙箱再处理这一边界。

## 模型命令沙箱（P2-163）

服务端按启动时的 `sandbox` 开关为执行器调用 `with_sandbox(true)`，模型前台、后台和
Ctrl+B 转后台命令均通过 `/usr/bin/sandbox-exec` 启动；用户 `!` 使用独立的
`spawn_user`，保留原环境。直接嵌入工具库时，`LocalExecutor::new` 仍不自行开启沙箱。
`configure_shell` 更新之后启动的命令使用的权限模式和新增目录；已启动进程及其后代
始终保留原规则。关闭会话先停命令，再通过 `close_shell` 删除会话临时目录。

可写 cwd、新增目录、独占 TMPDIR 和 `~/Library/Caches/canter/{cargo,npm}`；后两种
缓存由所有会话共用，不自动清理。`CARGO_HOME`、`npm_config_cache` 指向缓存，
`TMPPREFIX` 指向 TMPDIR 下的 zsh 前缀；不复制用户 cargo 配置、凭证或 bin。
真实 `.cargo` / `.npm` 只读，canter 状态目录及 `.ssh` / `.aws` / `.gnupg` 不可读写。
Git 实际元数据目录可写，但 config、hooks 和 worktree 元数据指针保持只读。
只有 allowAll 放开 IP 网络，另外两档仍断网；Unix socket 只放行可写范围且排除状态目录。
启动失败不退回墙外，失败输出附沙箱范围说明，不改变退出码或自动重跑。
文件工具、MCP、网页工具不由本件套墙；进程组看护和清理边界保持原样。

## 权限与执行器边界

| 模式 | read | write / edit | shell |
| --- | --- | --- | --- |
| alwaysAsk / Unknown | 执行 | 询问 | 按命令规则、会话前缀和只读白名单判断 |
| askForExecution | 执行 | 执行 | 按命令规则、会话前缀和只读白名单判断 |
| allowAll | 执行 | 执行 | 禁止规则仍拒绝，其余执行；规则文件坏了仍询问 |

Accept 仅允许当前调用；AcceptForSession 对 shell 只记住批准框展示的词前缀，其他工具沿用原范围；Decline 拒绝；
DeclineWithFeedback 拒绝并将原样说明交回模型，结果为 `Tool call declined. The user says: <说明>`；
需要批准时按 Accept、AcceptForSession、Decline、DeclineWithFeedback、Cancel 的顺序提供选项。
Cancel 拒绝并向调用方返回 `cancel_turn: true`，gate 本身不取消一轮、不发事件。
风险档来自内置 Tool 定义，不接受模型自报的风险档。
shell 会话前缀在改回 alwaysAsk 后仍有效，重开会话不恢复。命中 prompt、前缀过宽、不能确定语法或
触及受保护路径时不提供会话/永久前缀选项。永久批准使用 AcceptWithExecpolicyAmendment，列在会话批准后。

### 全局命令规则

服务端把自己的状态目录（默认 `~/.canter`，支持 `CANTER_HOME`）下的 `rules.json` 传给
`PermissionGate::with_rules_file`；核心嵌入方用 `CoreOptions.command_rules_path` 指定。
不读取模型参数或客户端环境来决定规则文件，不改含凭证的 config.json。

```json
{"rules":[
  {"pattern":["cargo","test"],"decision":"allow"},
  {"pattern":["git","status"],"decision":"prompt"},
  {"pattern":["cargo","publish"],"decision":"forbidden"}
]}
```

按解码后的词前缀匹配，`cargo test` 不匹配 `cargo testx`，路径程序必须按完整词匹配。
冲突取 forbidden > prompt > allow；空数组、空词、未知结果或非法 JSON 都使整个文件无效。
每次判断重新读，不存在表示没有规则；读不了或写坏了时三种模式都只提供单次批准，
每次连续故障在会话内提示一次，修复后下一次判断恢复。Forbidden 不弹框，模型收到规则拒绝原因。
Prompt 在询问模式下优先于只读白名单和会话授权；allowAll 只忽略 allow/prompt，不忽略禁止。

组合命令逐段判断，支持 `&&`、`||`、`;`、`|`、换行、`&`、`|&`；所有段都被规则、会话前缀
或原只读白名单放行才直接执行。复用原来的目录流转和路径检查。变量、命令替换、重定向等不能
完整识别时，allow 和会话授权不适用；已识别的字面前缀仍检查禁止，简单变量之后的独立段也检查。
不解释嵌套替换或控制流，这仍是批准判断而非沙箱。

一键永久放行显示每段实际要保存的词数组，只为尚需批准的段提出建议；程序和能确定的子命令
组成前缀，无子命令的已知程序只取程序名，前置选项或未知子命令结构不猜。
解释器、包装命令、裸 git、npm/pnpm/yarn run、find、sed 等不建议；完整清单见 P2-108 完成记录。
相对路径程序只提供会话前缀批准，不提供永久项；这些建议限制不影响用户手写的文件规则。
服务端先验证回答与提议相同，再 `save_rules`：进程内串行读取、去重追加、0600 临时文件原子替换。
保存失败保留批准请求，不执行命令或覆盖坏文件；成功后在对话里提示路径，本次执行。
永久放行不覆盖 prompt/forbidden；允许后执行前仍重新检查禁止规则。
手工编辑与保存之间没有跨进程文件锁；不要同时手改与点击保存。

规则只用于模型 shell（包括后台）。计划/审查先执行原有只读限制，再对允许的 shell 检查规则；
用户 `!` 命令、MCP、web_fetch、文件工具批准和目录边界保持原样。不提供查看/删除规则的界面。

审查轮使用 `gate.evaluate_review(&call)`：只放行 read 和同一白名单中的 shell，
write/edit 及其他 shell 直接拒绝；不看权限模式或已有授权，不开批准请求。
核心在准备 write/edit（读取目标和计算 diff）之前就拒绝，并给模型返回
`review is read-only`。审查结束不改会话的权限或授权，下一轮继续按原规则。
提问工具 `ask_user` 仍由核心在闸门之前处理，审查轮里照常提问和等待回答。

shell 免批准只识别一小部分语法和参数，白名单在 `src/readonly.rs`：

- 命令：`ls`、`cat`、`head`、`tail`、`wc`、`grep`、`rg`、`pwd`、`echo`、`printf`，以及
  `git status/diff/log/show`。支持常用短选项组合、带引号和转义的字面参数；
  head/tail 和 git log/show 支持 `-100` 这样的数字简写。Git 可带 `--no-pager`，
  带值的长选项只认 `--选项=值`，不猜可选参数的边界。printf 只认常见输出格式；
  `-v`、`%n` 等修改 shell 变量的形式仍询问。
- Git 每次判断都同步读当前仓库配置、会话 HOME 下的 `.gitconfig`、XDG 下的 `git/config`
  （未设 XDG_CONFIG_HOME 时用 `~/.config/git/config`）。支持向上查找仓库、裸仓库、
  worktree 的 `gitdir:`/`commondir` 和 `config.worktree`，不启动 Git 或其他子进程。
  未启用 GIT_CONFIG_NOSYSTEM 时，还用同一规则检查 `/etc/gitconfig` 和会话 PATH 中
  第一个可执行 git 所在前缀的 `etc/gitconfig`（例如 `/opt/homebrew/bin/git` 对应
  `/opt/homebrew/etc/gitconfig`；bin/git 是链接时仍检查 PATH 前缀）。PATH 缺失或找不到
  可执行 git 时询问；GIT_CONFIG_NOSYSTEM 为 `0`/`false` 等假值仍检查系统配置。
  出现 fsmonitor、textconv、diff.external、diff.*.command、gpg 段或 gpg 子段配置、
  log.showSignature、remote.*.promisor、extensions.partialClone，
  或 include/includeIf 时询问；`--format/--pretty` 以及配置中的命名/默认格式含 `%G` 也询问。
  `*.gpgsign` 和 `user.signingkey` 不影响只读命令放行。filter 仅接受 `git lfs install`
  写入的标准 `filter.lfs` 值：clean 为 `git-lfs clean -- %f`、smudge 为
  `git-lfs smudge -- %f`、process 为 `git-lfs filter-process`、required 为 `true`；
  其他 filter、其他键或改过的值仍询问。
  即使 filter 是标准值，LFS 的外部程序配置也询问：`lfs` 段中以 `extension` 开头的
  子段（ASCII 不分大小写）、`lfs.customtransfer.*`、`lfs.standalonetransferagent`。
  不存在的可选配置可跳过；无法读取、非普通文件、超过 1 MiB、无法识别的配置语法
  （包括续行/转义）、HOME 缺失或相对路径、相对 XDG_CONFIG_HOME 都询问。
  改变仓库/配置来源或注入配置的 GIT_DIR/GIT_COMMON_DIR/GIT_WORK_TREE/GIT_CONFIG*
  环境覆盖也询问（GIT_CONFIG_NOSYSTEM 除外）。
- `cd <字面路径>` 检查目标并跟踪后续目录，`&&`/`||` 按成功/失败分支检查，`;` 同时考虑
  cd 成功和失败的位置；管道内的 cd 不改变后续目录。多种可能目录都通过才免批准，
  可能状态超过 64 个则询问。无参数的 cd、`cd -`、带变量的 cd，以及会话设了非空
  CDPATH 时的相对 cd 都询问。`~` 是否展开按原始引号/转义判断，避免跟踪错目录。
  后续 cwd 使用字面规范化路径；解析后的 cd 目标只用于额外的受保护路径检查。
- find 支持字面起点、深度/名称/路径/类型过滤、布尔组合及打印等已知只读动作，
  包括 `\(` / `\)` 和引号里的 `(` / `)` 分组；未引用的括号仍按 shell 语法询问。
  `-exec/-execdir/-ok/-okdir/-delete/-fprint/-fprint0/-fprintf/-fls` 和未知参数仍询问。
- sort 支持常见排序/合并/去重/检查选项，以及 `-k/--key`、`-t/--field-separator`；
  `-o/--output`、`--compress-program`、指定临时目录、间接文件列表和未知选项仍询问。
  大输入排序可能使用程序自己的临时文件。
- uniq 支持 `-c/-d/-i/-u`、`-f/--skip-fields`、`-s/--skip-chars` 及对应长选项，
  只允许标准输入或一个输入文件，不允许输出文件参数。选项必须在输入文件之前：
  BSD uniq 会把 `uniq input -c` 的 `-c` 当成输出文件名，因此这种写法仍询问。
- sed 只认可直接检查的脚本：基本地址、打印/删除行等纯文本操作和替换，支持 `-n/-E/-r`
  与 `-e/--expression`。`-i/--in-place`、从文件加载脚本、`w/W/e/r/R` 命令、
  替换的 w/e 标志、复杂或无法识别的脚本都询问；括号组、分支、文本插入及未转义的
  正则字符组暂不放行。输出中的删除行不会修改源文件。
- `&&`、`||`、`;`、`|` 连接的每一段都必须通过检查。未知命令或选项、解析失败、
  重定向（包括输入重定向）、命令替换、变量展开、子 shell、后台 `&`、注释、换行和
  未引用的通配符一律询问。命令绝对路径、`git -C/-c` 等仍不放行。
- 每个字面参数（包括 `-f路径`、`--file=路径` 中的值）按当前可能 cwd 检查，规范化 `.`、`..`，
  按路径组件做 ASCII 不分大小写比较（所有平台一致）；
  `~` 用会话环境中的绝对 `HOME` 展开，不使用服务端 HOME。HOME 缺失、相对 HOME 和
  `~用户名` 都询问。碰到受保护路径本身或子路径时询问，cwd 在受保护目录内也询问。
  这只影响是否免批准，不改变批准后的执行；`allowAll` 和已有会话授权仍可执行这些命令。
- 带 `-r/-R/--recursive` 的 grep、所有 rg、git diff 的路径起点，等于受保护路径、
  位于其中或是其上级目录时都询问；省略路径时检查 cwd。搜索模式和选项值不当成起点。
  Git diff 的 `--` 后按显式路径检查；没有 `--` 时还检查 cwd，避免把版本号误当成路径。
  复杂路径匹配语法/通配起点仍询问。`find ~`、`ls -R ~` 只列名字，继续放行。
- 直接执行仍使用原始命令、`commandExecution` 开始/完成项目和输出回调，不增加或更改事件。

**已知限制：这不是 shell 沙箱，挡不全状态目录的访问。** `$HOME/.canter` 等显式变量语法
目前保守地询问，但不展开或追踪任意环境变量，也不完整检查其他程序的环境配置或 PATH
上实际执行的程序。上述仓库、用户和系统 Git 配置中的外部 diff/textconv 等已检查，
非标准安装在其他编译时位置的系统配置仍不覆盖。
cd 目标存在时会额外检查解析后的路径，
但路径检查仍不完整覆盖符号链接、硬链接、文件系统别名或执行期间的路径变化；
程序内部的其他隐式读取仍可能读到受保护文件。不要把白名单理解为“绝不会写文件或读取秘密”；read/write/edit 的原有
路径保护不变。合并时应把这些限制同步到 DESIGN。

`Executor` 是可替换的异步 trait：`cwd`、`resolve_path`、`open_file`、`write_file`、`spawn`。
另有同步的 `is_readonly_shell` 判断，默认返回 false（仍询问）；本机执行器使用已有的
会话 cwd、环境、受保护路径实现，不需要核心或服务端再传数据，也不在判断时启动子进程。
读文件返回 `FileReader`（异步字节流）；启动命令返回 `CommandHandle`（stdout、stderr、
`Box<dyn CommandProcess>`），后者提供 `wait`、`terminate` 和 `kill`；后两者只作用于原进程组。
第二期可把这些操作实现为
独立子进程的请求，不需要改变工具和权限接口。

参考 pi 的精确文字替换、oh-my-pi 的风险分档，以及 Codex `rust-v0.163.0`
的进程组清理、UTF-8 头尾截断和受限 shell 语法逐段判断思路，独立实现，未复制源码。

## LSP 生命周期、文件同步与诊断（P2-132 / P2-133 / P2-134）

`lsp::LspService` 是服务端持有的共享实例池，提供配置、启动、双向 stdio、关闭和资源管理。
P2-133 接入成功 write/edit、回退及 shell 完成，添加文件监视和诊断收集；P2-134 在核心层交给模型并提供公开协议。
内置 `src/lsp/servers.json` 有 Rust、Python、TS/JS 三种（P2-136，见下文「第一批语言」），没有新增模型工具。

`config.json` 默认 `lsp.enabled: false`，关闭时没有 LSP 进程或采样任务。用户开启即授权运行配置中的
服务器及其构建脚本/分析辅助进程，改配置需 `canter serve --replace`。配置示意（需换成已安装程序）：

```json
{"lsp":{"enabled":true,"servers":{"example":{
  "command":"my-language-server","args":["--stdio"],
  "extensions":{"example":"example"},"root_markers":["project.marker"],
  "initialization_options":{},"settings":{"example":{"diagnostics":true}}
}}}}
```

服务器 id 为字母、数字、连字符或下划线。用户覆盖按字段整体替换，省略字段继承内置值；空数组、
空对象、JSON null 都按显式值处理，不递归合并 settings。不读取仓库的 LSP 启动配置，不自动安装程序，
拒绝 npx/pnpx/bunx 等临时安装入口。用会话完整环境直接传 argv，以会话 PATH 查找，保留符号链接
启动名（兼容按 argv[0] 分派的程序）；不使用服务端的 PATH。环境、settings、原始服务器错误和 stderr
不进入日志/事件；错误只含英文原因和服务器 id，JSON-RPC 错误保留数字代码。

`for_file(file, FileContext, cancel)` 在开启且普通修改轮时按扩展名选择服务器，返回可保留的 `Instance`。
Plan/Review 返回 None。重叠扩展名按服务器 id 排序选择第一项。根目录由文件实际路径向上找最近的根标记，
止于最具体的会话 cwd/新增目录边界及物理 `.git` 边界；无标记或实际路径越界返回 Unsupported。
不合并不同 worktree，不解析各语言的 workspace 结构；Rust 用 `Cargo.lock` 当根标记，让 workspace 的子 crate 归到同一个根。
共用要求同根、同服务器配置和同完整环境；并发首次取得只启动一次。再次取得时检查可执行文件的路径和
文件标识/大小/修改时间，变化就关闭旧实例。项目配置或工具链文件变化会重建实例；不加载其中的启动命令。

保留 `Instance` 即保留订阅；最后一个订阅释放即清理。`request`/`notify` 仅供后续真实文档同步或诊断使用，
活动结束后更新空闲计时；取得实例、`is_running`/`error` 查询和 RSS 采样不续命。请求最多 30 秒，支持取消；
服务池的 `retry` 仅清除该文件对应实例的退避/熔断，不启动进程、不停止健康的共享实例；会话级重试随后重新同步文件。
公开状态接口见下文，终端显示和 `/lsp` 留给 P2-135。

最多两个实例，满时先回收最久未使用的空闲实例，都忙则报 ResourceLimited。空闲十分钟回收。
每五秒直接调用 `/bin/ps` 采样原进程组及后代树 RSS（去掉同一 PID 的重复计数）；连续两次超过单实例
4 GiB 或总计 8 GiB 时，先停空闲实例，再停超限/最久未使用实例，并锁住该键直到显式 retry。
RSS 是软阈值，采样之间可能超过且共享页可能重复计数；采样失败不会伪造内存读数，下一周期再采。
initialize 最多三十秒，启动失败（含未安装）退避六十秒；崩溃后仅由后续真实取得触发重启，五分钟内最多
自动重启两次，再失败就等待 retry 或重启服务端。

传输独立于 shell background 和 rmcp，支持 Content-Length 字节帧、乱序请求 id、取消和反向请求。
`workspace/configuration` 按请求顺序返回 settings 整体/分节值；未知节返回 null。`workspace/applyEdit`、
执行命令和其它未支持请求返回 -32601，不执行服务器要求的操作。支持作用域内的动态文件监视注册和
诊断 refresh。每帧最多 16 MiB、头部最多 8 KiB；发出超限帧只拒绝该条，stderr 排空但不保存。

shell 和 LSP 共用 `process` 内部启动锁、进程组和 guardian 所有权；既有 Executor trait 不变。
正常停止有界等待 shutdown → exit，超过三秒杀原进程组并 reap；服务端停止/替换在释放锁前等全部实例退出。
强杀服务端由 guardian 清理原组，自行脱离原进程组的后代仍在既有看护边界之外。

会话通过 `service.session(context, protected_paths)` 创建 `LspSession` 并由 `Tools::with_lsp` 持有。
只有成功文件修改才调用 `changed(path)`；返回的 `ChangeTicket::wait(Duration)` 允许调用方决定等待预算。
write/edit 的票据放在内部 `ToolOutput.change_ticket`，等待超时或丢弃票据不取消实例启动。
`session.subscribe().recv()` 接收后续批次，`session.diagnostics().await` 读取当前缓存；两者和票据等待均在
交付前重新核对实例代数、文档版本和磁盘摘要。`DiagnosticBatch.id` 可供 P2-134 去重，`source` 区分整表
push 与 pull provider；空表替换该来源的诊断。`Freshness::VersionUnconfirmed` 表示无版本推送，即使空表也
不能解释成已修好；`Stale`、`incomplete` 和英文 `reason` 表达覆盖丢失或超限。没有在本层写死工具等待时长。

使用 notify 8.2.1 原生监视，约 200 ms 合并一批，不监视 `.git`、`target`、`node_modules`、虚拟环境及
受保护目录，不遍历符号链接；文件读取逐级使用目录描述符和 O_NOFOLLOW。shell 完成仅核对已打开文件及
已知配置，不扫描整个仓库；纯 shell 会话不启动服务器。文件删除/改名关闭旧 URI 并清掉其诊断，配置变化或
监视失败标可能过期并重建。监视初始化失败会重新建立一次，仍失败则返回明确错误，之后修改可再试。

每实例最多打开 64 个文件，按最近同步使用回收；每会话诊断缓存和订阅队列各 1 MiB，单文件诊断 256 KiB，
按序列化字节加元数据计费，超限显式标记不完整。关注文件最多 1,024 个且路径元数据不超过 1 MiB；监视最多
4,096 个目录/配置，初次建立最多检查 100,000 个目录项，事件队列限 1 MiB / 4,096 个路径，均为内部上限。
只拉取声明 diagnosticProvider 的已打开文档，支持 full/unchanged、resultId、refresh；修改时取消旧请求，
不做工作区全量 pull。会话 close 释放订阅，Plan/Review 暂停主动同步，恢复普通修改轮后有界核对。

### 交付元数据与会话控制（P2-134）

`ChangeTicket` 提供路径和修改时的单调时钟时间；成功 write/edit 通过 `with_content` 绑定内容和文件标识、
修改时间。`is_current()` 在交付前核对，`failure()` 不等待地读取已发生的启动/同步失败。诊断批次另带项目根、
单调时钟接收时间和 Unix 毫秒采集时间。核心层负责 500 ms 与 60 s 预算，结束快速等待仍不取消启动。

`LspSession::language_servers`、`stop_server`、`retry_server` 操作本会话订阅。id 对应服务器/项目根，
不随进程代数改变。停止只取消本会话的启动等待并释放工作区，其它会话仍可使用共享实例；重试沿既有修改路径
重新同步。状态查询不算 LSP 活动；实例池保留最新 RSS 样本供查询，不为采样写事件。关闭的服务不挂到 Tools，
保持原有模型工具输出逐字不变。

### 第一批语言（P2-136）

打开：在 `~/.canter/config.json` 写 `{"lsp":{"enabled":true}}`，再 `canter serve --replace`。关着时内置表不生效，不起任何进程。

| id | 要装什么 | 扩展名 | 项目根标记 |
|---|---|---|---|
| `rust-analyzer` | `rust-analyzer` 在会话 PATH 上（`rustup component add rust-analyzer`） | `rs` | `Cargo.lock` |
| `pyright` | `pyright-langserver` 在会话 PATH 上（`npm i -g pyright` 或 `pip install pyright`） | `py`、`pyi` | `pyproject.toml`、`pyrightconfig.json` |
| `typescript` | 项目自己装 TypeScript 7（`npm i -D typescript@7`），用 `node_modules/.bin/tsc --lsp --stdio`，node 在会话 PATH 上 | `ts`、`mts`、`cts`、`tsx`、`js`、`mjs`、`cjs`、`jsx` | `tsconfig.json`、`jsconfig.json`、`package.json` |

- Rust 默认关 `checkOnSave`（不在每次改文件后跑 cargo check）；构建脚本、过程宏仍可能运行 cargo 和项目代码。还没有 `Cargo.lock` 的新项目找不到根，跑一次 cargo 后才有诊断。
- Python 只分析打开的文件（`python.analysis.diagnosticMode=openFilesOnly`），其余照项目自己的 pyright 配置。
- TS/JS 运行的是项目 `node_modules` 里的程序。没装 TS 7（没装或 TS 5、6）的项目算不可用，canter 不安装、不用 `npx`；`node_modules` 在上层目录、子目录另有 `tsconfig.json` 时也找不到。TS 5、6 的项目可以自己装 `typescript-language-server`，按下面的办法加一项。

改配置：按 id 覆盖，只写要改的字段，例如打开 Rust 的保存检查：

```json
{"lsp":{"enabled":true,"servers":{"rust-analyzer":{
  "initialization_options":{"checkOnSave":true},
  "settings":{"rust-analyzer":{"checkOnSave":true}}
}}}}
```

加一种语言：在 `lsp.servers` 里加新 id，写全 `command`、`args`、`extensions`、`root_markers`，按需写 `initialization_options`、`settings`（服务器发 `workspace/configuration` 时按配置段回）；`command` 写相对路径时按项目根找。想随 canter 发布，就加进 `src/lsp/servers.json`，在 `lsp::config` 的测试里补一条。根目录规则、虚拟环境这类配置写不出来的，要在 LSP 模块里加适配。

## MCP 工具

`mcp::Registry` 用固定版本的官方 rmcp 客户端维护进程/HTTP 连接；配置和运行句柄没有
Debug/Serialize。`snapshot()` 为会话复制排序后的工具定义和调用句柄，`Tools::with_mcp`
绑定这份快照。模型名字用 `Tools::find` 查找，`prepare_named` 只解析对象参数；MCP 参数中的
`path` 不经过内置文件工具的路径准备。`Tool::ALL` 只列八个内置工具。

`Tool::Mcp` 是执行风险分类；具体名字、服务和参数由准备好的调用保存。权限闸门仍发放
不可伪造的单次 permit，会话授权改用完整工具名字，MCP 与内置工具没有共用授权。
审查轮的 MCP 调用由同一闸门直接拒绝；不使用 MCP 的 readOnlyHint 等声明。
调用取消、超时、协议错误与远端 `isError` 都返回失败项目；模型和客户端输出使用既有
有界头尾缓冲。image 内容块用锁定的 base64 库解码，先检查编码长度，再检查原始字节的 8 MiB 上限和文件签名；
不信任 mimeType 声明，显示名用 `服务名-工具名-image-序号.格式`（序号从 1 起，含无效图片）。
图片与 read 共用上述存盘和模型管线，错误图片只给原因。同一结果的文字和内嵌文字资源按原顺序保留；
没有非空文字时，把 structuredContent 转为 JSON 文字补在末尾（含显式 null），不与已有文字重复。
文字沿用配置值脱敏及输出上限；图片说明若被头尾截断，另补到模型结果末尾，保证能找到随后的图片。
音频、二进制资源和资源链接仍为 `[MCP non-text content omitted]`，不读取资源 URI。
连接的关闭和原进程组清理由 registry 负责。

## 网页抓取

`web_fetch` 只接受 `url`，使用独立的 reqwest 客户端 GET HTTP(S) 网址。
没有新增配置项；固定 `User-Agent: canter/0.1 web_fetch`、`Accept-Encoding: identity`，
不走代理（`no_proxy`），不继承模型/MCP 请求头或会话凭证，不保存/发送 cookie，
拒绝 URL 中的用户名、密码、反斜杠、空白和控制字符，跳转 Location 同样检查。
规范化后与原串不同（如 punycode、大小写、数字 IP、默认端口）时，批准参数和项目增加
可选 `resolvedUrl`；`url` 仍保存模型原串，批准框同时显示两者，工具卡片使用实际网址。
保持默认 TLS 证书验证，不执行网页脚本或加载附属资源。

`text/html`、`application/xhtml+xml` 用固定版本 htmd 转 Markdown，忽略 head/script/style；
转换成功且有非空 `<title>` 时，第一行是 `Title: <标题>`，空一行后接正文。
标题在预检通过后的同一个大栈转换线程里，经 htmd 的元素回调取出，复用同一次 HTML
解析；去掉标签、首尾空白，连续空白压成一个空格，实体沿用转换器的解码规则。
标题最多保留 512 个 Unicode 字符，不拆 UTF-8 字符，不另加截断标记，计入已有输出预算。
无标题或标题为空不加该行；预检回退原文时也不加。未知编码说明保留在标题行之后、正文之前。
HTML 及其余 `text/*`、`application/json` 和 `application/*+json` 在下载上限检查后、
HTML 预检和转换前，用固定版本 encoding_rs 转成 UTF-8：开头的 UTF-8 / UTF-16LE/BE BOM
优先，其次是 Content-Type 的 charset，最后（仅 HTML）预扫前 1024 字节的
`<meta charset=…>` 或 `<meta http-equiv="Content-Type" content="…; charset=…">`。
编码标签按 WHATWG 表识别（GB2312 / GBK 对应 GBK，latin-1 对应 windows-1252）；
meta 中的 UTF-16 标签按 UTF-8、x-user-defined 按 windows-1252 处理。
未声明时仍按 UTF-8，不猜编码；非法字节替换为 `�`。未知编码按 UTF-8 解码，结果前注明
`[Unknown charset: xxx; decoded as UTF-8.]`；识别成功的不再附编码说明。
响应的 Content-Encoding 任一值不是 identity 时，只返回一行编码占位，不读取或转换正文。
其它类型只返回一行类型、大小占位，不把二进制交给模型；缺少 Content-Type 按
application/octet-stream 处理。已知大小的非文字响应不读取内容，未知大小按流计数。
下载最多 5 MiB：先检查 Content-Length，再逐块检查，超限立即丢弃响应并返回说明。
转换结果或回退的原始 HTML 均沿用 shell/MCP 的模型 32 KiB 头尾预算（截断标记另计）和客户端 1 MiB 总上限；
终端显示网址、状态及保留结果的字节数。

一次调用从请求开始到读取、转换完成总共最多 30 秒，包含跳转耗时；Esc 取消会丢弃
正在等待响应头/响应体的请求。解析 HTML 前对原文做线性扫描：粗估嵌套上限 256，
单标签最多 4096 字节（含属性）。预检作为第一道检查，超限不调用解析库。
扫描识别引号和普通注释，仅匹配栈顶的结束标签降低深度，非空元素的 `/>` 不当作关闭；
这是保守估计，可能跳过省略结束标签或含复杂外来元素的正常页面。原始文本元素中的注释、
CDATA、标签内部再含 `<` 等无法确定嵌套边界的输入也不转换（包括属性值里合法的 `<`）。
以上情况均返回原始 HTML，前加一行 `[HTML not converted: <reason>; showing raw HTML.]`，
按现有输出上限截断，工具状态为 completed，本轮继续，不作为工具错误。
通过预检后，每次转换新建独立线程，指定 256 MiB 栈作为第二道保护，用 oneshot channel
传回 htmd 结果，不使用 spawn_blocking。已开始的转换不能强制终止，不承诺转换 CPU 耗时
的硬上限；Esc/30 秒超时不等待该线程，只停止等待和使用其结果，照旧返回工具错误。
4xx/5xx、连接/TLS 失败也作为工具错误返回，继续这一轮。

301/302/303/307/308 的相对 Location 按当前网址解析；规范化主机名及有效端口相同才
自动跟随，最多 5 次。改变主机或端口只返回 `Redirected to <URL>`，不访问新目标，
模型可另调一次工具再过闸门。默认端口按 HTTP 80 / HTTPS 443 计算，子域名不共用授权。
alwaysAsk / askForExecution 均须批准；「本会话都允许」只保存这一个主机及端口，
不按整个工具放行；批准框显示完整网址，授权说明写出具体的 `host:port`。审查轮始终拒绝并返回
`review is read-only`，不会弹框，已有授权和 allowAll 均不能绕过。

本机、内网和明文 HTTP 不另外拦截；allowAll 会直接访问这些地址，也可能触发有副作用的
GET 接口或暴露内部服务内容。需要逐个把关时使用询问模式；这里没有网络沙箱。
不做搜索、JavaScript 渲染、登录、cookie、代理配置、网页摘要、PDF 解析、图片输入或 URL 缓存。
会话创建/打开时固定工具列表；旧会话下次打开会获得这个新内置工具，历史存盘不改写。

`submit_plan {plan: Markdown}` 始终在内置列表中，交互由核心等待用户批准。
计划模式使用 `PermissionGate::evaluate_plan`，限制先于权限档和会话授权；
read/ask_user/submit_plan 允许，shell 复用现有只读白名单，web_fetch/web_search 复用原审批。
write/edit、非白名单 shell、所有 MCP、update_plan 返回 blocked，不生成审批。
核心在准备前也拦一次，避免为禁止的改动读文件或生成 diff。
