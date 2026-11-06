# 任务：第二期-29.3 - doctor自检

2026-11-06，canter/main 交给 canter/dev-doctor（Kimi，`kimi --auto`，用户默认模型 kimi-code/k3.1-pro，不分强度）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.85）、要、碰要害）。交叉审查按 11-05 起的对比条件交 Claude Code `opus[1m]` / high。
类型：功能变更
依据：现在出了问题（配置写错、登录过期、服务端和终端版本不一致、MCP 或沙箱用不了）要主控翻日志才知道。本轮加一个只报告、不修改的自检；LSP 不查。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
对比样本：第 9 件，按轮流派 Kimi，派后 Kimi 7 件 / Codex 2 件（P2-208、P2-219 不算）。用户可以改派。
依赖：P2-208
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-05 用户问第二期还有没有没考虑到的功能，主控列了五条新发现，用户：「这样新发现的1，2，3，4，5都排上。排到gpt任务完成之后（也在第二期）。」
- 11-06 主控问四件事，用户都选推荐（LSP 那项没选）：
  - 入口：`canter doctor` 和 `/doctor` 都要。大部分检查在服务端做，两个入口看到同一份结果；服务端起不来或连不上时，`canter doctor` 自己查本机能查的（配置文件能不能解析、`server.log` 最后几条错误）。
  - 查哪些：配置和版本；登录和各家模型；MCP 和沙箱。不查 LSP。
  - 联网：带 key 请求模型列表这类不计费的接口，看出地址不通、key 无效或过期；某家没有这个接口就显示查不了，不算出错；每家每次最多一次，有超时。都不真调模型。
  - 查出问题：只报告和写怎么修，不改任何东西。

## 现状（主控查过的）

- 已有零散的查看：`/mcp`（MCP 服务连没连上）、`/sandbox`、`/roles`（角色文件写错标红）、`canter auth status`（ChatGPT 账号）、`canter mcp list`、`canter --version`；服务端 `GET /v1/info` 回 `serverVersion`、`protocolVersion`、`features`；`~/.canter/server.json` 记着服务端的版本、协议版本、pid、socket。
- 配置：`config.json` 热重载失败时保留上一份有效配置、用 `warning` 事件报文件名和行列（DESIGN「改配置不用重启（P2-154）」）；`rules.json` 每次权限检查时重读；`tui.json` 只有终端读；角色文件在 `CANTER_HOME/agents/*.md`（P2-223）。
- 各家：`config.json` 的 `providers`（P2-200），`api` 缺省 Chat Completions，ChatGPT 那家是 `responses`，模型目录按账号取（P2-216）。
- 沙箱：macOS Seatbelt（P2-123，`crates/canter-tools/src/sandbox.rs`）。

## 先读

- `docs/DESIGN.md`：第 8 条（协议，新接口照它的写法）、第 15 条（状态目录）、「改配置不用重启（P2-154）」「沙箱（P2-123）」、P2-200 几家同时配、P2-202/P2-203 GPT 登录、P2-216 模型目录、MCP（P2-12）和远程 MCP 授权、第 23 条（核心不绑 DeepSeek）。
- 项目规矩 `CLAUDE.md`。
- 只参考、不搬：Codex `rust-v0.163.0` 的 `codex-rs/cli/src/doctor.rs`、`codex-rs/cli/src/doctor/`（只读克隆在 `/tmp/codex-p2-205`）。

## 在哪里干活

- worktree：`../canter-worktrees/doctor`，分支 `doctor`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/doctor`。
- 主要动 `crates/canter/`（命令行）、`crates/canter-server/`、`crates/canter-protocol/`、`crates/canter-tui/`，按需 `crates/canter-core/`、`crates/canter-model/`，以及 `docs/DESIGN.md`；README 不动（P2-228 再写）。

## 要做的

1. **检查项**（每项结果是 ok / 有问题 / 查不了三种之一，有问题和查不了都带一句英文原因，有问题的再带一句英文怎么修，比如跑哪条命令、改哪个文件哪一行）：
   - 配置：`config.json`、`rules.json`、`tui.json`、`agents/*.md` 能不能读、有没有写错（用各自现有的校验，写错给文件名和行列或字段名）；服务端正在用的是不是上一份有效配置（热重载失败的那种）。
   - 版本：服务端和这个终端（或这个 `canter` 命令）的版本、提交、协议版本是否一致。
   - 状态目录：`CANTER_HOME` 能不能写。
   - 登录：ChatGPT 账号有没有登录、是不是过期或要重新授权；各家 key 有没有填（不打印值）。
   - 各家能不能连上：每家带 key 请求一次不计费的模型列表接口（Chat Completions 那类用现有地址下的模型列表，ChatGPT 用现有取模型目录的那条路）；看出地址不通、key 无效或过期、超时。没有这个接口或回的东西认不出，算查不了。
   - MCP：每个服务连没连上，远程服务的授权有没有过期。
   - 沙箱：配置要用沙箱时，本机沙箱能不能用。
2. **在哪查**：服务端新加一个只读接口返回上面全部结果，`canter doctor` 和 `/doctor` 都用它。服务端起不来或连不上时，`canter doctor` 不去拉起服务端，自己查本机能查的：配置文件能不能解析、版本（读 `server.json`）、`server.log` 最后几条错误（只取错误行，不打印配置内容和凭证），并写明服务端连不上。
3. **显示**：
   - `canter doctor`：一项一行，前面用 `ok`、`fail`、`skip` 这类英文标记（字样你定，写进 DESIGN），有问题的下一行缩进写原因和怎么修；最后一行写有几项有问题。有问题时退出码非 0。
   - `/doctor`：输入框上方弹框，样子照 `/mcp`、`/roles`：第一行 `Doctor` 加数量摘要，按配置、版本、登录和模型、MCP、沙箱分节，有问题的红色，查不了的灰色。命令说明 `Check configuration, sign-in, providers, MCP and sandbox`。
   - 联网那几项要等，界面不能卡住：先显示正在查，查完再填结果；Esc 能关掉。
4. **不能出错的地方**：
   - 不打印、不记日志、不经协议回传任何 key、令牌、请求头的值、配置原文；原因里只写名字（家名、服务名、文件名、字段名）。
   - 每个联网检查有超时（多少你定，写进 DESIGN），几家并行查，整次 doctor 有总时限；一项卡住或出错只影响这一项，doctor 照样给出其余结果。
   - 判断不了一律报查不了，不报 ok。
   - 只读：不改配置、除下面说的令牌刷新外不写凭证、不作废令牌、不删文件、不重连或重启 MCP 服务、不拉起服务端。ChatGPT 走现有取模型目录的路时它本来会做的令牌刷新照旧，doctor 不额外刷新。
5. **协议**：新接口和返回结构写进 DESIGN 第 8 条，这是 canter 自定扩展（Codex 的 doctor 只在命令行）。新终端连旧服务端时 `/doctor` 给一句英文提示服务端太旧、请升级；`canter doctor` 这时退回本机检查。
6. DESIGN 写清：检查哪几项、哪些联网、超时和总时限、不回传什么、和 Codex doctor 不同的地方。

## 怎么算做完

- 照用户 11-06 选的：`canter doctor` 和 `/doctor` 都有，看到同一份结果；服务端连不上时 `canter doctor` 自己查本机能查的；查配置和版本、登录和各家模型、MCP 和沙箱，不查 LSP；各家带 key 请求不计费的模型列表，没有接口的显示查不了，每家每次最多一次、有超时，不真调模型；只报告和写怎么修，不改任何东西。
- 验证只做这些：
  - 每处一条针对它的测试（先确认现在的行为让它失败）：每个检查项各一条有问题的、一条正常的；key 无效（假服务端回 401）、地址不通、超时、没有模型列表接口各一条；服务端连不上时命令行退回本机检查；新终端连旧服务端；`/doctor` 弹框显示。
  - 边角用例：输出、日志、协议回包里都搜不到测试用的假 key 和假令牌；一家卡住时其余几项照常出结果、整次在总时限内结束；doctor 跑完配置、凭证文件、MCP 连接没有变化。
  - 测试只用本机假服务端和隔离的 `CANTER_HOME`，不调真实接口。
  - 改到的包跑全部测试（`cargo nextest run -p canter -p canter-server -p canter-protocol -p canter-tui`，改了 `canter-core`、`canter-model` 一起加），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不查 LSP；不加 `--fix`、不自动修任何东西；不加 `--json` 之类没要的输出格式。
- 不真调模型（不发对话请求），不调真实接口；不碰用户真实的 `~/.canter`，测试用隔离的 `CANTER_HOME`、临时端口和 socket。
- 第二期核心不绑 DeepSeek：不按 DeepSeek 的名字、地址、请求字段分支；查模型列表按 `api` 种类走，不按家名。DeepSeek 这些别家的测试一条都不许改，碰到了停下来问。
- 不改现有的 `/mcp`、`/sandbox`、`/roles`、`canter auth status`、`canter mcp list` 的行为和输出。
- 界面上和命令行输出的字一律英文。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（某项检查现有代码拿不到、要动登录或 MCP 的现有逻辑、上面没写到的取舍），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `doctor` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
