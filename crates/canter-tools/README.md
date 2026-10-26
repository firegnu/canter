# canter-tools

六个内置工具、每会话的执行器和权限判断；不依赖模型包，不发送事件、不弹批准框。
本机实现使用 Unix 进程组，面向 macOS / Linux。

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
   以下 gate/execute 步骤用于读写文件、shell、web_fetch 和 MCP 工具。
   调 `gate.evaluate(&call, mode)`。`Execute(permit)` 可执行，`Ask(ApprovalRequest)`
   交循环/客户端处理。请求包含完整参数，write/edit 另加顶层 `diff`；文件路径解析后
   与模型提供的不同时另带 `resolvedPath`，客户端应显示这个实际目标。shell 保留完整命令。
   先由循环/服务端核对待处理请求归属、重复回答，再把回答交给
   `gate.resolve(&call, decision)`。它返回 `Execute(permit)` 或 `Deny { cancel_turn }`；
   未知回答返回 `invalid_decision` 错误，符合协议 README。
   拒绝时用 `call.declined_output()` 得到 `Declined` 项目和模型文字，循环另根据
   `cancel_turn` 决定是否停止一轮。
5. 得到允许后，调用 `tools.execute(permit, &cancel, &mut on_output).await`。
   许可只能由闸门创建、不能复制，借用这一个 call；调用期间保留 call。
   call 绑定准备它的 `Arc<dyn Executor>`，交给其他执行器会报错。文件实际目标在执行前
   再检查，符号链接改了指向会拒绝执行。准备好的 call 本身不能传给 execute 绕过闸门。
   回调是 `FnMut(&str) + Send`，shell 的 stdout/stderr 文字块按收到的顺序合流，
   调用方可转成 `item.delta`；回调须快速返回，不要阻塞运行时。
   `ToolOutput.text` 给模型，`ToolOutput.item` 给客户端最终项目。
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
| read | `path`, 可选 `startLine`（默认 1）、`lineCount`（默认 2000） | 带行号的 UTF-8 文本，`fileRead` |
| write | `path`, `content` | 整文件写，创建父目录；`fileChange` 和 unified diff |
| edit | `path`, `edits: [{oldText, newText}]` | 一次替换多处；`fileChange` 和 unified diff |
| shell | `command`, 可选 `timeoutMs` | 文字输出、退出码、耗时；`commandExecution` |
| ask_user | `question`, `options: [{label, description}]`（2–4 个，标签不重复，内容非空） | 会话循环等待选项或自写文字；模型结果 `{question, answer}`，客户端项目 `userInput` |
| web_fetch | `url`（HTTP(S)，不含用户名/密码） | HTML 转 Markdown，文字和 JSON 原样；有界文字结果，客户端项目 `webFetch` |

`Tool::ALL` 固定按 read/write/edit/shell/ask_user/web_fetch 排列，保证会话每次请求的工具前缀稳定。
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
含 NUL 或读取范围不是 UTF-8 时返回二进制/编码错误。
起始行超过 EOF 时说明总行数，空文件按 0 行处理。

shell 使用 `/bin/sh -c`，非登录、非交互，stdin 关闭；先 `env_clear` 再装入会话环境。
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

## 权限与执行器边界

| 模式 | read | write / edit | shell |
| --- | --- | --- | --- |
| alwaysAsk / Unknown | 执行 | 询问 | 白名单只读命令执行，其余询问 |
| askForExecution | 执行 | 执行 | 白名单只读命令执行，其余询问 |
| allowAll | 执行 | 执行 | 执行 |

Accept 仅允许当前调用；AcceptForSession 记住本会话的这个工具；Decline 拒绝；
Cancel 拒绝并向调用方返回 `cancel_turn: true`，gate 本身不取消一轮、不发事件。
风险档来自内置 Tool 定义，不接受模型自报的风险档。
对 shell 的会话授权等于放行这个会话的所有命令；改回 alwaysAsk 后已有授权仍有效。

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

## MCP 工具

`mcp::Registry` 用固定版本的官方 rmcp 客户端维护进程/HTTP 连接；配置和运行句柄没有
Debug/Serialize。`snapshot()` 为会话复制排序后的工具定义和调用句柄，`Tools::with_mcp`
绑定这份快照。模型名字用 `Tools::find` 查找，`prepare_named` 只解析对象参数；MCP 参数中的
`path` 不经过内置文件工具的路径准备。`Tool::ALL` 只列六个内置工具。

`Tool::Mcp` 是执行风险分类；具体名字、服务和参数由准备好的调用保存。权限闸门仍发放
不可伪造的单次 permit，会话授权改用完整工具名字，MCP 与内置工具没有共用授权。
审查轮的 MCP 调用由同一闸门直接拒绝；不使用 MCP 的 readOnlyHint 等声明。
调用取消、超时、协议错误与远端 `isError` 都返回失败项目；模型和客户端输出使用既有
有界头尾缓冲，非文字只留占位行。连接的关闭和原进程组清理由 registry 负责。

## 网页抓取

`web_fetch` 只接受 `url`，使用独立的 reqwest 客户端 GET HTTP(S) 网址。
没有新增配置项；固定 `User-Agent: canter/0.1 web_fetch`、`Accept-Encoding: identity`，
不走代理（`no_proxy`），不继承模型/MCP 请求头或会话凭证，不保存/发送 cookie，
拒绝 URL 中的用户名、密码、反斜杠、空白和控制字符，跳转 Location 同样检查。
规范化后与原串不同（如 punycode、大小写、数字 IP、默认端口）时，批准参数和项目增加
可选 `resolvedUrl`；`url` 仍保存模型原串，批准框同时显示两者，工具卡片使用实际网址。
保持默认 TLS 证书验证，不执行网页脚本或加载附属资源。

`text/html`、`application/xhtml+xml` 用固定版本 htmd 转 Markdown，忽略 head/script/style；
其余 `text/*`、`application/json` 和 `application/*+json` 按 UTF-8 返回（非法字节替换）。
Content-Type 的 charset 声明不是 UTF-8 时，在返回文字前注明
`[Decoded as UTF-8; declared charset: xxx.]`；没有做转码或读取 HTML meta charset。
响应的 Content-Encoding 任一值不是 identity 时，只返回一行编码占位，不读取或转换正文。
其它类型只返回一行类型、大小占位，不把二进制交给模型；缺少 Content-Type 按
application/octet-stream 处理。已知大小的非文字响应不读取内容，未知大小按流计数。
下载最多 5 MiB：先检查 Content-Length，再逐块检查，超限立即丢弃响应并返回说明。
转换后沿用 shell/MCP 的模型 32 KiB 头尾预算（截断标记另计）和客户端 1 MiB 总上限；
终端显示网址、状态及保留结果的字节数。

一次调用从请求开始到读取、转换完成总共最多 30 秒，包含跳转耗时；Esc 取消会丢弃
正在等待响应头/响应体的请求。解析 HTML 前对原文做线性扫描：粗估嵌套上限 256，
单标签最多 4096 字节（含属性）；超限不调用解析库，作为工具错误返回，本轮继续。
扫描识别引号和普通注释，仅匹配栈顶的结束标签降低深度，非空元素的 `/>` 不当作关闭；
这是保守估计，可能拒绝省略结束标签或含复杂外来元素的正常页面。原始文本元素中的注释、
CDATA、标签内部再含 `<` 等无法确定嵌套边界的输入也拒绝（包括属性值里合法的 `<`）。
该检查限制递归深度和单标签的属性处理成本，
不承诺转换耗时的硬上限：转换仍在线程池执行，已开始的转换不能被强制终止，
Esc/超时只停止等待和使用它的结果。4xx/5xx、连接/TLS 失败和超时都作为工具错误返回，继续这一轮。

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
