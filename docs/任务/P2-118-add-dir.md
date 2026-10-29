# 任务：add-dir

2026-10-28，用户在看板上新建。10-29 主控补全，交给 canter/dev-adddir（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.97）、要、碰要害）
类型：功能变更
依据：一件活常常跨几个仓库（微服务那种，不是 monorepo），会话只有一个工作目录。本件让会话再加几个目录，和工作目录一样对待；同时立一条「目录内外」的边界，让加目录现在就起作用。排在 P2-104 沙箱前面，沙箱一开始就把这些目录算进可写范围。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：无
执行：派出

## 用户原话

- 10-28：「另外118也排到第二期」
- 10-29 主控说明现状（canter 没有工作目录边界，读哪里都不问，Ask for execution 下改哪里都不问），给了范围和 A/B/C 三个选择，建议：加边界、不读加进来的目录里的 `AGENTS.md`、只做加不做移除。用户：「都按你的建议.我觉得这种场合主要是便于类似微服务那种遍布多个repo而不是monorepo的使用场景」

## 现状和参考（10-29 主控查的）

- 批准：`crates/canter-tools/src/permission.rs` 的 `evaluate`。`alwaysAsk` 下读以外都问；`askForExecution` 只问执行类（shell、MCP 等），改文件不问，不管文件在哪；`allowAll` 都不问。另有受保护路径（`executor.rs` 的 `protected_paths`），只读白名单的路径检查在 `readonly.rs` 的 `path_allowed`。本会话放行（`AcceptForSession`）的键是工具名（`permission_key`），改文件就是 `edit` / `write`。
- 会话设置里只有一个 `cwd`（`crates/canter-protocol/src/types.rs`），建会话 `POST /v1/sessions`、改设置 `PATCH /v1/sessions/{id}`、`session.updated` 事件带公开设置（DESIGN 第 8 条）。
- 系统提示词环境段（`crates/canter-core/src/context.rs`）写工作目录和 git 仓库根，打开会话时定下来；DESIGN 第 17 条：发给模型的只往后加，前面不改。
- `@` 引用文件：`crates/canter-tui/src/mention.rs`，只走会话目录。
- 启动参数：`crates/canter/src/lib.rs`（`-c` / `--continue`、`--resume` 等）。
- 别家：Claude Code 有 `--add-dir` 启动参数和 `/add-dir` 命令，加进来的目录和工作目录待遇一样，工作目录外的文件要批准；加进来的目录里的 `CLAUDE.md` 默认不读。Codex 的 `--add-dir` 只用在沙箱上，把目录加进可写范围。要看 Codex 源码，克隆到 `/tmp` 下只读。

## 先读

- `docs/DESIGN.md` 第 8 条（协议）、第 17 条（照顾缓存命中）。
- `crates/canter-tools/src/permission.rs`、`executor.rs`、`lib.rs`（工具怎么解析路径）。
- `crates/canter-core/src/context.rs`、`session.rs`；`crates/canter-protocol/src/types.rs`；`crates/canter-tui/src/mention.rs`、`commands.rs`；`crates/canter/src/lib.rs`。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-118-add-dir`，分支 `p2-118-add-dir`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-118-add-dir`。

## 要做的

1. **加目录**：
   - 启动时 `canter --add-dir <路径>`，可以写几次；和 `-c` / `--resume` 一起用时加到打开的那个会话上。
   - 会话里 `/add-dir <路径>`，加完在对话里提示一句。
   - 相对路径按会话的工作目录算，`~/` 按家目录；路径不存在、不是目录时提示原因，不加。已经在工作目录或已加目录里面的，提示一句、不重复加。
   - 加进来的目录跟着会话存盘，`/resume`、`-c` 回来还在；另一个终端打开同一个会话看到的一样。
   - 只做加，不做移除。
2. **告诉模型**：打开会话时已有的目录写进系统提示词的环境段（和工作目录写在一起）。会话中途加的，照第 17 条不改前面，往后追加一条说明告诉模型。
3. **`@` 引用**：`@` 列表里也能选到加进来的目录里的文件（照工作目录的规矩，按 git 忽略规则）。路径怎么写、和工作目录里的同名文件怎么区分，照你的判断定，在完成记录里写。
4. **目录边界**：`askForExecution` 下，改文件的工具（write、edit 这类）改工作目录和加进来的目录里的文件照旧直接改；改这些目录外的文件要批准，批准框和现在改文件的一样。
   - 判断时把路径规范化（`..`、符号链接）后再比，不能靠 `../` 或符号链接越过；新建文件按它已存在的最近一级父目录算。
   - 读文件照旧不管；`alwaysAsk`、`allowAll`、受保护路径、计划模式、审查轮只读、只读白名单 shell 都不变。
5. **协议和文档**：加的目录怎么放进会话设置、建会话和改设置怎么带，照 Codex app-server 的结构定，比 Codex 多的写进 DESIGN 第 8 条；DESIGN 写一条 add-dir 的规矩（上面这些和用户 10-29 的选择）。旧客户端不认识新字段照旧能用，旧会话没有这项按空的读。
6. **帮助**：`?` 帮助弹框和 `/help` 的命令列表加 `/add-dir`；`canter --help` 写 `--add-dir`。

碰到要拿主意的事（比如目录外改文件时选了「本会话都放行」，是只放行这个目录外的改动还是照现在按工具名放行；协议字段怎么定），停下来问主控。

## 怎么算做完

- （用户 10-29 接受的）照主控的建议：加目录（启动参数和 `/add-dir`）、告诉模型、`@` 能选到、`askForExecution` 下目录外改文件要批准、不读加进来的目录里的 `AGENTS.md`、只做加不做移除。
- （用户 10-29 说的场景）跨几个仓库干活：工作目录在一个仓库，加上另外几个仓库的目录后，在这几个仓库里改文件和在工作目录里一样。
- 验证只做这些：
  - 针对每条的测试：`--add-dir` 和 `/add-dir` 加上、存盘后重开还在；相对路径、`~/`、不存在、不是目录、重复加的提示；环境段有这些目录，中途加的往后追加、前面不变；`@` 能选到加进来的目录里的文件；`askForExecution` 下工作目录和加进来的目录里改文件不问、目录外要问、`..` 和符号链接越不过去、新建文件按父目录算；`alwaysAsk`、`allowAll` 不变；旧会话、旧客户端照旧能用。
  - 再跑全套 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不做沙箱（P2-104）、不做按命令放行的规则（P2-108）；不改 shell 命令的批准规矩。
- 不读加进来的目录里的 `AGENTS.md`；不做移除目录。
- 状态栏的 git 分支和改动行数、`/diff`、`/review` 照旧只看工作目录。
- 不真调模型接口；测试用隔离的 HOME 和 `CANTER_HOME`、临时端口，不读用户真实的 `~/.canter`。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-118-add-dir` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑没有就写「没有」。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：实现可重复的 `--add-dir`、`/add-dir`，以及 `-c`、`--resume` 和 `/resume` 的目录恢复。目录由服务端按会话 cwd / HOME 解析，要求存在且为目录，转为规范路径后去重；一批请求先全部校验再更新。公开设置通过 `session.updated` 存盘、广播，另一个终端立即获得相同目录。新增目录和重复覆盖都有对话提示，无效路径显示原因。帮助弹框、斜杠命令列表和 CLI 帮助均已更新。
- 模型和文件引用：打开时将现有目录写进环境段；中途用独立的系统消息追加完整目录列表，不改已发前缀，不插断一批工具调用与结果。说明存入私有历史，压缩、回退、撤回后补回当前目录，不计作用户原话；新增目录不参与自动加载 AGENTS.md。`@` 对每个目录沿用现有 git 忽略规则、扫描限制和不跟随符号链接的规则；cwd 文件保留相对路径，额外目录文件显示和插入绝对路径，含空格时沿用引号，从而区分不同仓库中的同名文件。
- 目录边界：write/edit 复用现有执行器解析出的路径（含 `..`、符号链接和新文件的最近已存在父目录）判断范围；`askForExecution` 在范围内直接执行，范围外沿用文件修改批准框。目录更新和恢复同步到会话权限门。受保护路径、读文件、shell 批准、只读白名单、计划模式和审查轮沿用原机制。
- 主控决定（10-29，两项均选 A）：目录外选「本会话都放行」仍按工具名授权，write 和 edit 分开，和 alwaysAsk 下原有放行一致，不新增按目录授权。会话设置和建会话参数用 `additionalDirectories`，PATCH 用 `addDirectories` 只追加；旧会话缺省为空。决定及与 Codex 的差别已写进 DESIGN 第 8 条，JSON Schema / TypeScript 已重新生成。复用既有 features 能力协商，新增 `addDirectories`，旧客户端忽略；新终端连到不支持的旧服务端时明确提示更新，避免误报添加成功。
- 参考：只读查看本机 `/tmp/r49-src/codex/codex-rs/app-server-protocol/schema/json/v2/ThreadStartParams.json` 和 `ThreadResumeResponse.json`，参考其会话结构及 `sandboxPolicy` 的 `type: workspaceWrite` / `writableRoots`。版本按 HANDOFF.md 标注为 `rust-v0.163.0`；该本地副本没有 git 元数据，未独立复核标签。没有复制 Codex 代码。
- 验证：先观察到协议丢弃新字段、目录未规范化、目录外修改未请求批准、CLI / 斜杠命令不识别、`@` 未扫描额外目录的目标失败，再实现并通过。新增检查覆盖目录保存及重开、相对路径与 `~/`、无效和重复路径、固定前缀与追加说明、不加载额外 AGENTS.md、跨仓库同名文件引用、三档权限、会话授权、`..` / 符号链接 / 新文件父目录，以及旧设置和旧客户端兼容。隔离的真实服务端与两个 TUI 客户端串测创建、追加、同步、`--resume`、`/resume`、`-c` 和服务端重启；测试模型均为本机假实现，真实模型调用 0 次。
- 最终检查：所有命令均在前台等到结束，使用本任务独立 `CARGO_TARGET_DIR`。`cargo nextest run --all-targets` 最终 948 / 948 通过、0 跳过；`cargo test --doc --workspace` 通过（1 项编译失败示例）；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。
- 没做的事：没有沙箱、移除目录或按命令授权规则；状态栏 git、`/diff`、`/review` 仍只看 cwd。没有改用户配置、安装或切换可执行文件，也没有操作用户正在使用的服务端。只在本任务分支提交，交主控审查、合并和推送。
- 踩过的坑：协议注释也是生成产物的一部分，修改 PATCH 的文档注释后漏重新导出，首轮全套检查被生成文件一致性测试拦下；重新导出后全套通过。测试夹具要使用现有 edit 的 `edits` 数组，TUI 补全后要按既有帧机制重绘再发送；已修正夹具，没有放宽断言或缩短超时。

## 主控审查

- 2026-10-29 canter/main 审 `657cacc`：改动范围对（状态栏、`/diff`、`/review` 没动，没读加进来的目录里的 `AGENTS.md`）；边界在 `permission.rs` 的 `outside_directories`，复用受保护路径同一套解析，解析不出一律问，执行前再解析比对。重跑 `cargo nextest run --all-targets` 948/948 通过，和完成记录一致。
- 交叉审查（canter/review-adddir-1，Claude Code）：可以合并，必须改 0、建议改 2、可以不改 5，见 `P2-118-交叉审查.md`。
- 建议改 1（`/new`、`/handoff`、计划「清空上下文实施」开的新会话不带加进来的目录）是产品取舍，交用户定；建议改 2（会话中途发 `role: system` 的目录说明，没真调 DeepSeek 验证）要真调模型，用户在场时再定。都不挡合并，记进 HANDOFF。
- 结果：一次过
