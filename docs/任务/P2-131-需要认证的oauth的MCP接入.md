# 任务：MCP 命令行添加和 OAuth 登录

2026-10-29，用户在看板上新建；主控补全。
2026-10-29，canter/main 交给 canter/dev-mcplogin（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害）
类型：功能变更
依据：现在加 MCP 服务要手改 `~/.canter/config.json` 的 `mcp_servers`；要在浏览器里登录（OAuth）的远程 MCP 服务（比如 GitLab 官方的 MCP）canter 接不了。本件照 Codex、Claude Code 加两样：`canter mcp add / list / remove` 用命令改配置；`canter mcp login <名字>` 走标准 OAuth 登录、存令牌、过期自动续。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：P2-12（MCP）
执行：派出

## 用户原话

- 10-27 P2-92：「我理解这两个都是标准，应该无缝支持」；「MCP的话应该读取canter自己的，而不是跑去读codex或者claude code的」
- 10-29 用户：「130后面插入131，因为我有时候会使用gitlab的mcp。这个gitlab mcp是要认证的」
- 10-29 用户：「等一下啊，我看到的codex和claude code是可以安装MCP的，这个是怎么来的，如果canter要做这个需要多大的工作量？」主控说明分两层（命令行往配置里添一项；远程服务在浏览器里登录），canter 用的 `rmcp 3.7.0` 自带 OAuth（`auth` 特性），建议 P2-131 写成这两部分。用户：「按你的建议，131写成这两部分」

## 别家（只参考、不搬）

- Codex `rust-v0.163.0`：`codex mcp add / list / get / remove / login / logout`（`codex-rs/cli/src/mcp_cmd.rs`）、OAuth 令牌怎么存和续（`codex-rs/rmcp-client`）。源码在 `/tmp/p2-104-src/codex`（不在了按 `HANDOFF.md`「机器和环境」重新克隆标签到 `/tmp`，只读）。
- Claude Code：`claude mcp add / list / remove`、`/mcp` 里登录（公开文档 https://code.claude.com/docs/en/mcp ）；`~/.claude` 不读。
- `rmcp 3.7.0` 的 `auth` 特性（`src/transport/auth.rs`、`common/auth/streamable_http_client.rs`）：授权服务器发现、PKCE、动态注册客户端、令牌续期。

## 先读

- `docs/DESIGN.md` 第 15 条 MCP 配置（P2-12）、第 8 条 MCP 那几段、第 14 条（服务端在后台起，拿不到终端环境）。
- `crates/canter-tools/src/mcp.rs`（连接、请求头、脱敏）、`crates/canter-server/src/config.rs`（读 `config.json`）、`crates/canter/src/main.rs`、`lib.rs`（命令行）；终端 `/mcp` 弹框（`crates/canter-tui/src/commands.rs`）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-131-mcp-login`，分支 `p2-131-mcp-login`（派的时候从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-131-mcp-login`。

## 要做的

1. **命令行改配置**：`canter mcp add <名字> [--env K=V]... -- <命令> [参数...]` 加本机程序的服务；`canter mcp add <名字> --url <网址> [--header K=V]...` 加远程服务；`canter mcp list` 列出（不显示环境变量和请求头的值）；`canter mcp remove <名字>`。写法照 Codex，细节你定、写进完成记录。改的是 canter 自己的 `config.json`（保持 0600，保留别的字段和格式能保留的部分），名字重复、写法不对时报错不写。改完提示「Takes effect in new sessions after canter serve --replace」（照现在改配置的规矩，不做热加载）。
2. **OAuth 登录**：`canter mcp login <名字>` 对远程服务走标准 MCP 授权：发现授权服务器、动态注册客户端（服务器不支持时报清楚原因）、PKCE、在浏览器里打开授权页、本机临时端口接回调、换到令牌。用 `rmcp` 自带的 `auth`，不自己从头写。`canter mcp logout <名字>` 删掉令牌。登录在命令行里做（服务端在后台，没法替你开浏览器）。
3. **令牌**：存在 canter 状态目录里（0600），不进 `config.json`、日志、事件、会话存盘和公开接口；服务端连这个服务时带上，过期自动续，续不了时这个服务标「要登录」、照常不连，别的服务不受影响。
4. **`/mcp` 和 `GET /v1/mcp`**：要登录的服务显示「Login required: run `canter mcp login <name>`」。
5. **文档**：DESIGN 第 15 条和第 8 条补上命令、令牌存哪、和 Codex 不同的地方；`canter --help` 和 `crates/canter-server/README.md` 跟着改。

碰到要拿主意的事（命令写法、令牌文件放哪、要不要 `get`、回调端口），停下来问主控。

## 怎么算做完

- （用户 10-29）`canter mcp add / list / remove` 能用命令加、看、删 MCP 服务；`canter mcp login` 能登录要认证的远程 MCP 服务，令牌存好、过期自动续。
- 验证：用本机假的授权服务器和假 MCP 服务（测试里写，Rust，不连外网）：add / list / remove 改对 `config.json`、保持 0600、重复和写错时报错不写、list 不露值；login 走完发现、注册、PKCE、回调、换令牌，令牌文件 0600，服务端连时带上；过期自动续；续不了标「要登录」、别的服务照常；logout 后不再带令牌；令牌不出现在日志、事件、会话存盘、`/v1/mcp`。边角：回调端口被占；用户在浏览器里点拒绝；授权服务器不支持动态注册；登录时服务端没在跑。再跑全套 `cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。
- 真连 GitLab 的实测不在本件：装好后由用户自己用 `canter mcp login` 试。

## 不要做

- 不读 Codex、Claude Code 的 MCP 配置，不做从它们导入。不做 MCP 资源、提示词模板、工具按需加载（P2-92 定了以后再说）。不做配置热加载。
- 不改已有 MCP 服务的连接、批准规矩；不改 P2-108 的命令规则。
- 不连外网、不连真的 GitLab；不碰用户的 `~/.canter`；测试用隔离的 HOME 和 `CANTER_HOME`、临时端口和 socket，不调真实模型接口。测试起的进程用完按记下的 PID 停掉。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-131-mcp-login` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑没有就写「没有」。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 阶段记录（当时暂停，依赖来源现已确认）

- 已实现配置部分的 add/list/remove、帮助文字和说明。写入保留 MCP 字段外原文，独立锁、0600 临时文件原子替换；list 只列名字和传输。测试用隔离 HOME/CANTER_HOME，命令不启动服务端。
- RED：`cargo nextest run --offline -p canter --test mcp_cli` 因现有程序报 `unknown option: --env` 失败。实现后同一测试通过；补充远程配置、删除、无配置创建、兄弟项保留和损坏文件不写入后，`cargo nextest run --offline -p canter --test mcp_cli --lib` 共 5 项通过；`cargo clippy --offline -p canter --all-targets -- -D warnings` 通过。均使用本任务独立编译目录、前台运行。
- 阻塞：尝试启用 rmcp 3.7.0 的 auth 后，`cargo check --offline -p canter-tools` 明确报 `no matching package named oauth2 found`。本机 registry 源码和 crate 缓存没有 oauth2。任务禁止连外网，已询问主控是否允许仅下载缺失的构建依赖；未收到许可前不联网。已撤回暂时的 auth 特性开关，保留当前分支可编译。
- 待做：OAuth 登录/退出、令牌存储和续期、服务端与 /mcp 状态、相关假服务器测试、最终文档和完整验收。当前没有把未实现的 OAuth 行为写入 DESIGN，也未作完成提交。
- 参考：只读 `/tmp/p2-104-src/codex/codex-rs/cli/src/mcp_cmd.rs` 和本机 rmcp 3.7.0 的 `transport/auth.rs`、`common/auth/streamable_http_client.rs`；没有搬代码。未访问真实 GitLab、模型接口或用户状态目录。

## 完成记录

- 做了什么：实现 `canter mcp add/list/remove/login/logout`。配置命令直接操作 canter 的 JSON，用独立文件锁和 0600 临时文件原子替换，保留 `mcp_servers` 外的原文。OAuth 复用 rmcp 3.7.0 auth 做发现、动态注册、S256 PKCE、授权码交换及自动续期；命令行开浏览器，回调只监听本机临时端口。令牌、客户端密钥按服务分别存入 `mcp-auth/<服务名 UTF-8 十六进制>.json`，绑定服务 URL，锁协调登录、续期和退出。服务端读取并自动续期，认证失败只使对应服务需要登录；原 `/v1/mcp` 的 error 和 `/mcp` 弹框直接显示登录命令。更新 DESIGN 第 8、15 条、帮助和 README。
- 验证：先跑出配置命令 RED（unknown option）、OAuth 登录 RED（未识别 login）、服务状态 RED（只有普通连接错误）、说明/schema 脱敏 RED（令牌回显），再实现到 GREEN。本机 Rust 假授权服务器核对资源发现、注册参数、S256 challenge/verifier、resource、回调与换令牌；测试登录不需 daemon、0600、启动及运行中续期、刷新令牌保留、续期被拒、其他服务继续连接、logout 后后续请求不带令牌、浏览器拒绝、无动态注册、回调端口先占住再注册及用完释放。检查工具结果/说明/schema、模型请求、公开状态、事件、会话文件和日志不含测试令牌；终端渲染回归确认显示完整登录命令。配置测试覆盖本机/HTTP 添加、参数和重复名拒绝不写、保留其他字段、无配置创建、删除和列表不露值。
- 完整检查：`cargo nextest run --offline --all-targets` 1014 项全部通过，0 跳过；`cargo test --offline --doc --workspace` 通过（1 个文档测试）；`cargo clippy --offline --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。所有命令使用本任务独立编译目录，在前台跑完。
- 拿主意的地方：名字限 1～48 个 ASCII 字母、数字、下划线、短横线；重复环境变量/请求头拒绝，HTTP 请求头名字按大小写不敏感处理。list 只列名字和传输，不回显命令、网址、环境或头值。不加 get、JSON 导出或预注册客户端选项。回调用 `127.0.0.1:0`，五分钟等授权，打开浏览器失败提供手动网址；先绑定再注册，所以不依赖固定端口是否空闲。凭证与配置分开保存；已配 Authorization 头时 login 明确要求先移除，原静态头连接仍沿原规矩。remove 只删配置，清账号用 logout；logout 不作远端撤销、不影响已发出的请求。无协议字段变更，已有连接重建仍按重启服务端的规矩。
- 依赖：主控明确允许编译时从 crates.io 下载缺失依赖，测试及运行验证仍只用本机假服务。rmcp 保持 3.7.0，只增加 auth 特性；没有升级或删掉任何原有包版本。Cargo.lock 新增 18 个包：`base64 0.22.1`、`block-buffer 0.10.4`、`cpufeatures 0.2.17`、`crypto-common 0.1.7`、`digest 0.10.7`、`generic-array 0.14.7`、`oauth2 5.0.0`、`ppv-lite86 0.2.21`、`rand 0.8.8`、`rand_chacha 0.3.1`、`rand_core 0.6.4`、`sha2 0.10.9`、`thiserror 1.0.69`、`thiserror-impl 1.0.69`、`typenum 1.20.1`、`version_check 0.9.5`、`zerocopy 0.8.63`、`zerocopy-derive 0.8.63`。同名旧包继续保留；其余 lock 变动是依赖引用消歧和 auth 带来的特性依赖。
- 参考：确认 `/tmp/p2-104-src/codex` 为 `rust-v0.163.0`，只读其 CLI 和 `rmcp-client/src/oauth/credential_store.rs`；使用本机 rmcp 已发布 API，没有搬源码。只在任务分支提交，不合并、不推送、不安装或替换用户服务端。
- 没做的事：按任务范围，没有真实 GitLab 或真实模型调用（0 次），没有访问用户 `~/.canter` 或其他 agent 配置；没有配置热加载、MCP 资源/模板、按需加载或 get 命令。真实 GitLab 由用户安装后自行登录验证。
- 踩过的坑：auth 所需 oauth2 不在本机缓存，离线编译明确失败后等待并取得主控许可才下载。令牌除工具结果外还可能被服务回显在工具说明和 schema，测试发现后补上脱敏；原有 SSE 大小限制在包装 HTTP 客户端时需继续转发，已保留并跑过原 MCP 回归。
- 主控审查后：登录提示统一为「Login required: run `canter mcp login <name>`」，重启提示改为「Takes effect in new sessions after canter serve --replace」，同步断言、DESIGN 第 8 条、README 和任务要求，复查本分支新增代码没有其他用户可见中文，前台运行的 9 项相关测试及 `git diff --check` 均通过。
- 10-29 续期审查后：仅 AuthRequired、InsufficientScope、Auth(AuthorizationRequired / TokenRefreshRejected) 标为需要登录，启动时也按此分类，临时续期、网络及存储错误不再误报登录；两条新增测试均先复现 RED 再修到 GREEN，本机假授权服务器首次续期返回 503 时当次工具调用失败但仍连接，第二次续期和工具调用成功，MCP 初始化次数始终为 1、服务端 PID 未变，未加重连，启动凭证读取失败也保持普通初始化错误；同步 DESIGN 和 README，前台完成的 7 项 OAuth 测试、1 项普通 HTTP 503 回归、`cargo clippy --offline --all-targets -- -D warnings` 及 `git diff --check` 均通过。

## 主控审查

2026-10-29，canter/main。
- 看了 diff：配置命令只改 `mcp_servers`、独立锁和 0600 原子替换；令牌按服务存 `mcp-auth/`（目录 0700、文件 0600、绑定 URL）；回调只听本机、核 state、PKCE；出错一律固定文字，令牌在工具结果、说明、schema、`/v1/mcp` 里脱敏。「不要做」的一件没做。
- 依赖：主控允许编译时从 crates.io 下载 rmcp `auth` 带来的 18 个包，rmcp 仍 3.7.0，没升级已有依赖。
- 主控交回改了两处：
  1. `/mcp` 的「需要登录」和命令行「改完要重启」两句是中文（主控任务文件写错），改成英文 `Login required: run \`canter mcp login <name>\`` 等（`938b2c5`）。
  2. 用户 10-29 指出、主控核 rmcp 源码后定为必须改：续期碰上断网会一直标「要登录」直到重启。改成只有 `AuthRequired`、`InsufficientScope`、`AuthorizationRequired`、`TokenRefreshRejected` 才标，临时错误下次调用再试；测试证明第一次续期 503 后连接还在、第二次续期后工具照常（`96face8`）。交叉审查的建议改 1 是同一个问题，改法相同。
  3. `96face8` 的 `cargo fmt --check` 没过，交回格式化（`e5aec41`）。
- 交叉审查（canter/review-mcplogin，审到 `938b2c5`）：可以合并，没有必须改；建议改 2 条（第 1 条即上面第 2 处，已修；第 2 条 OAuth 端点不核 https）、可以不改 4 条，记进 HANDOFF。修复是审查后提交的，主控看了修复的 diff，和审查员给的改法一致，不再请审查员复核。
- 重跑（`96face8`）：`cargo nextest run --all-targets` 1016 条全过；`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`git diff --check` 通过；`e5aec41` 后 `cargo fmt --check` 通过。
结果：返工 2 次
