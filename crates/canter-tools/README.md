# canter-tools

四个内置工具、每会话的执行器和权限判断；不依赖模型包，不发送事件、不弹批准框。
本机实现使用 Unix 进程组，面向 macOS / Linux。

## 第 4 件接入

1. 每个会话创建一个 `LocalExecutor::new(absolute_cwd, environment)`，包装为
   `Arc<dyn Executor>` 交给 `Tools::new`；另建一个 `PermissionGate::default()`。
   环境用客户端传入的完整 `BTreeMap<String, String>`，不能合并服务端环境。
   gate 不跨会话共享、不序列化，恢复会话时重新创建。
2. 用 `Tool::ALL` 和 `Tool::definition()` 取得名称、说明、`serde_json::Value`
   参数 Schema、`Risk`。模型给出名字后用 `Tool::from_name()` 查找；未知名字返回 `None`，
   由调用方报告未知工具。
3. 调 `tools.prepare(tool, item_id, raw_arguments, &cancel).await`。
   必须传模型给出的原样字符串。准备阶段只读文件、校验参数和计算 diff，不写文件、不起命令。
   成功返回不可修改的 `PreparedCall`；`raw_arguments()` 保留原文，供模型历史存盘。
   失败返回 `ToolOutput`，其中已有给模型的错误文字和正确种类的失败 `Item`。
4. 调 `gate.evaluate(&call, mode)`。`Execute` 可执行，`Ask(ApprovalRequest)`
   交循环/客户端处理。请求包含完整参数，write/edit 另加顶层 `diff`，shell 保留完整命令。
   先由循环/服务端核对待处理请求归属、重复回答，再把回答交给
   `gate.resolve(&call, decision)`。它返回 `Execute` 或 `Deny { cancel_turn }`；
   未知回答返回 `invalid_decision` 错误，符合协议 README。
5. 得到允许后，把原来的 call 交回**同一会话同一个** `Tools`：
   `tools.execute(call, &cancel, &mut on_output).await`。
   回调是 `FnMut(&str) + Send`，shell 的 stdout/stderr 文字块按收到的顺序合流，
   调用方可转成 `item.delta`；回调须快速返回，不要阻塞运行时。
   `ToolOutput.text` 给模型，`ToolOutput.item` 给客户端最终项目。
   `ToolOutcome::{Completed, Failed, Cancelled, TimedOut}` 供循环区分结果。
   协议没有 cancelled/timedOut 项目状态，这两种最终 `Item.status` 都是 `Failed`，
   一轮是否 interrupted 由循环根据 outcome 决定。

取消使用 `CancellationToken`。取消后继续 await 执行结果，不要 abort/drop 写文件的 future：
它要等已经启动的文件工作线程退出。写入在提交前检查取消，原文件不会变成半份；
原子替换已经完成时返回成功。取消可能留下已创建的空父目录。
同一会话的文件修改由调用方串行执行。写入前会检查文件仍与准备 diff 时一致，
不一致就要求重新准备；这是内容前置条件检查，不是对外部编辑器的文件锁。

## 参数和结果

| 工具 | 参数 | 结果 |
| --- | --- | --- |
| read | `path`, 可选 `startLine`（默认 1）、`lineCount`（默认 2000） | 带行号的 UTF-8 文本，`fileRead` |
| write | `path`, `content` | 整文件写，创建父目录；`fileChange` 和 unified diff |
| edit | `path`, `edits: [{oldText, newText}]` | 一次替换多处；`fileChange` 和 unified diff |
| shell | `command`, 可选 `timeoutMs` | 文字输出、退出码、耗时；`commandExecution` |

所有相对路径按会话 cwd 解析。write/edit 先读原文，用 `similar` 生成 diff；
非 UTF-8 原文件不能生成文本 diff，会报错。
edit 按原始文字精确匹配，不做模糊匹配、空白或换行归一化；每个 oldText 非空且恰好出现一次，
包括彼此重叠的重复出现。各替换区域不能重叠；所有检查通过后一次原子替换。
失败会指出第几处匹配有问题，原文件保持不变。

read 每次最多 2000 行、64 KiB 带行号的内容，续读提示另计；不切开一行。
达到限制时提示下一次 `startLine`。单行本身过长时提示用 shell 分字节查看。
含 NUL 或读取范围不是 UTF-8 时返回二进制/编码错误。

shell 使用 `/bin/sh -c`，非登录、非交互，stdin 关闭；先 `env_clear` 再装入会话环境。
shell 自己按其规则建立的变量（例如 PWD）仍然存在。
默认超时 120000 ms，上限 600000 ms，超限参数报错。
模型结果超过 32768 字节时保留头尾各至多 16384 字节，在 UTF-8 边界截断，
中间标出实际省略字节数；提示和退出信息另计。
客户端的流式输出和最终 `aggregatedOutput` 都不截断，完整输出保留在内存中。
两个输出流分别增量解码，跨块 UTF-8 不损坏；非法字节按替代字符显示。
非零退出、超时、取消都返回失败项目。超时/取消向记录的进程组发 SIGKILL，
等待 shell 回收并读完管道；shell 正常退出也清掉本组剩余后台子进程。
执行器句柄的 Drop 同样会尝试清理进程组。此层不是系统沙箱。

## 权限与执行器边界

| 模式 | read | write / edit | shell |
| --- | --- | --- | --- |
| alwaysAsk / Unknown | 执行 | 询问 | 询问 |
| askForExecution | 执行 | 执行 | 询问 |
| allowAll | 执行 | 执行 | 执行 |

Accept 仅允许当前调用；AcceptForSession 记住本会话的这个工具；Decline 拒绝；
Cancel 拒绝并向调用方返回 `cancel_turn: true`，gate 本身不取消一轮、不发事件。
风险档来自内置 Tool 定义，不接受模型自报的风险档。

`Executor` 是可替换的异步 trait：`cwd`、`open_file`、`write_file`、`spawn`。
读文件返回 `FileReader`（异步字节流）；启动命令返回 `CommandHandle`（stdout、stderr、
`Box<dyn CommandProcess>`），后者提供 `wait` 和 `kill`。第二期可把这些操作实现为
独立子进程的请求，不需要改变工具和权限接口。

参考 pi 的精确文字替换、oh-my-pi 的风险分档，以及 Codex `rust-v0.163.0`
的进程组清理和 UTF-8 头尾截断思路，独立实现，未复制源码。
