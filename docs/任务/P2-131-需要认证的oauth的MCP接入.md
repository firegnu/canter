# 任务：MCP 命令行添加和 OAuth 登录

2026-10-29，用户在看板上新建；主控补全。
2026-10-29，canter/main 交给 <派的时候按读表补：名字、哪家、模型 / 强度>。
路由：<派的时候补>
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

1. **命令行改配置**：`canter mcp add <名字> [--env K=V]... -- <命令> [参数...]` 加本机程序的服务；`canter mcp add <名字> --url <网址> [--header K=V]...` 加远程服务；`canter mcp list` 列出（不显示环境变量和请求头的值）；`canter mcp remove <名字>`。写法照 Codex，细节你定、写进完成记录。改的是 canter 自己的 `config.json`（保持 0600，保留别的字段和格式能保留的部分），名字重复、写法不对时报错不写。改完提示「`canter serve --replace` 后新开的会话生效」（照现在改配置的规矩，不做热加载）。
2. **OAuth 登录**：`canter mcp login <名字>` 对远程服务走标准 MCP 授权：发现授权服务器、动态注册客户端（服务器不支持时报清楚原因）、PKCE、在浏览器里打开授权页、本机临时端口接回调、换到令牌。用 `rmcp` 自带的 `auth`，不自己从头写。`canter mcp logout <名字>` 删掉令牌。登录在命令行里做（服务端在后台，没法替你开浏览器）。
3. **令牌**：存在 canter 状态目录里（0600），不进 `config.json`、日志、事件、会话存盘和公开接口；服务端连这个服务时带上，过期自动续，续不了时这个服务标「要登录」、照常不连，别的服务不受影响。
4. **`/mcp` 和 `GET /v1/mcp`**：要登录的服务显示「需要登录：运行 `canter mcp login <名字>`」。
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
