# canter-tools

四个内置工具、每会话的执行器和权限判断；不依赖模型包，不发送事件、不弹批准框。
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

shell 免批准只识别一小部分语法和参数，白名单在 `src/readonly.rs`：

- 命令：`ls`、`cat`、`head`、`tail`、`wc`、`grep`、`rg`、`pwd`，以及
  `git status/diff/log/show`。支持常用短选项组合、带引号和转义的字面参数；
  Git 可带 `--no-pager`，带值的长选项只认 `--选项=值`，不猜可选参数的边界。
- `&&`、`||`、`;`、`|` 连接的每一段都必须通过检查。未知命令或选项、解析失败、
  重定向（包括输入重定向）、命令替换、变量展开、子 shell、后台 `&`、注释、换行和
  未引用的通配符一律询问。`cd`、`find`、`sed`、命令绝对路径、`git -C/-c` 等本次不放行。
- 每个字面参数（包括 `-f路径`、`--file=路径` 中的值）按会话 cwd 检查，规范化 `.`、`..`；
  `~` 用会话环境中的绝对 `HOME` 展开，不使用服务端 HOME。HOME 缺失、相对 HOME 和
  `~用户名` 都询问。碰到受保护路径本身或子路径时询问，cwd 在受保护目录内也询问。
  这只影响是否免批准，不改变批准后的执行；`allowAll` 和已有会话授权仍可执行这些命令。
- 直接执行仍使用原始命令、`commandExecution` 开始/完成项目和输出回调，不增加或更改事件。

**已知限制：这不是 shell 沙箱，挡不全状态目录的访问。** `$HOME/.canter` 等显式变量语法
目前保守地询问，但不展开或追踪任意环境变量，也不检查环境引入的程序配置、PATH 上实际
执行的程序、Git 的外部 diff/textconv 等配置行为。字面路径检查不追踪符号链接、硬链接、
文件系统别名或执行期间的路径变化；递归搜索父目录、程序内部的路径匹配和隐式读取也可能
读到受保护文件。不要把白名单理解为“绝不会写文件或读取秘密”；read/write/edit 的原有
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
