# 任务：第二期-19 - /permissions 查看和删除放行规则

2026-10-30，canter/main 交给 canter/dev-permissions（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害）
类型：功能变更
依据：照 Claude Code 的 `/permissions`。「以后都放行」点多了 `rules.json`（P2-108）越积越多，点错一条现在只能手改文件。本件只做查看和删除：终端弹列表，服务端读、改 `rules.json`，改完马上生效（服务端每次判断都重新读这个文件）。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：无
执行：派出

## 用户原话

- 10-29 主控对照 Claude Code、Codex 日常功能，提了改配置和升级会打断长任务、`/init`、`/permissions`、会话分叉四项；用户：「这四项我都需要排进来，还都是我日常都在使用的。」
- 10-30 用户：「能不能剩下的挑几个简单的，不碰核心的做做。例如141 143之类的」；主控建议 `/permissions` 只做看和删，不在里面加、改。用户：「同意，从 /clear 开始」。
- 10-30 写任务前主控说明：规则存在服务端，客户端不直接读写服务端的文件，所以要在协议里加接口，碰到协议、服务端和安全相关的规则，比前几件重。用户：「等一下，我要和你讨论一下。我理解这个其实就是前端配置但是会写到后端之后实时生效。对吧？」主控确认；用户：「照做吧」。

## 现状和参考（10-30 主控查的）

- 规则文件：服务端状态目录的 `rules.json`（默认 `~/.canter`，`CANTER_HOME` 可改），0600，格式 `{"rules":[{"pattern":["cargo","test"],"decision":"allow"}]}`，decision 为 `allow` / `prompt` / `forbidden`。读写在 `crates/canter-tools/src/rules.rs`：`Rules::read`（不存在算空、坏文件报错），`append`（批准框「以后都放行」时写，有进程内写锁 `WRITE_LOCK`）。规矩见 DESIGN「按命令放行（P2-108）」：每次判断重新读文件；读不了或格式错时模型 shell 退回单次询问。
- 服务端接口在 `crates/canter-server/src/http.rs`，不属于某个会话的全局接口有 `/v1/mcp`、`/v1/models`、`/v1/usage` 这类；路由名在 `canter-protocol` 的 `routes`。
- 终端里「列出来、选一个操作」的现成样子：`/ps`（列出后台命令、选一个停掉，`crates/canter-tui/src/background.rs`）。
- Claude Code 的 `/permissions`：按 Allow / Ask / Deny 分组列规则，选中一条可以删，删前确认。

## 先读

- `docs/DESIGN.md`「按命令放行（P2-108）」一节、第 8 条（协议）。
- `crates/canter-tools/src/rules.rs`；`crates/canter-server/src/http.rs` 里 `/v1/mcp` 那种全局接口怎么写；`crates/canter-protocol` 的 `routes` 和类型；`crates/canter-tui/src/background.rs`、`commands.rs`。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-156-permissions`，分支 `p2-156-permissions`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-156-permissions`。

## 要做的

1. **协议和服务端**：加两个不属于某个会话的接口：列出 `rules.json` 里的规则（每条的前缀和 decision，加上文件路径）；删掉一条。
   - 删哪条按规则内容（前缀加 decision）认，不按列表里的位置：列出来之后「以后都放行」又加了规则，也不会删错。同样内容的重复规则一次删掉。要删的已经不在了，回一句说明，不算出错。
   - 删除和「以后都放行」的写入用同一把写锁，写法照 `append`（权限 0600，不把文件写坏）。
   - 文件不存在时列出来是空的；文件坏了（读不了、格式错）时列出接口回错误说明，删除拒绝、不改文件（不覆盖用户手写坏的文件）。
   - 字段照 Codex app-server v2 的结构定，Codex 没有的写进 DESIGN 第 8 条并说明为什么。旧客户端不认识新接口照旧能用；改了协议注释要重新生成协议类型文件。
2. **终端 `/permissions`**：不带参数，打开一个弹框（照 `/ps` 的样子），按 Allow / Ask / Deny 分三组列规则（对应 `allow` / `prompt` / `forbidden`），每条写成命令前缀（比如 `cargo test`）。最上面或最下面写规则文件的路径，并说明加、改规则去改这个文件。
   - ↑↓ 选，选中一条按 Enter 或 Delete 先确认一次（`Delete rule "cargo test" (Allow)? Enter to confirm · Esc to cancel` 这类），确认了才删；删完列表刷新，对话里或状态行提示一句删了什么。Esc 关掉弹框。
   - 没有规则时写一句没有规则、怎么加（批准框里「以后都放行」或改文件）。文件坏了时显示错误说明，不能删。
   - 命令列表和 `?` 帮助里列出 `/permissions`，说明写 `View and delete command rules`。界面上的字都用英文。
3. **文档**：DESIGN「按命令放行（P2-108）」一节末尾那句「只有文件管理，不做规则界面」改成写清本件加的查看和删除（P2-156）；第 8 条记新接口。

碰到要拿主意的事（比如接口路径和字段怎么定、删除用什么方法），停下来问主控。

## 怎么算做完

- （用户 10-30 同意的）`/permissions` 只做查看和删除，不在里面加、改；界面在终端，读、改文件在服务端，删了马上生效。
- 验证只做这些：
  - 服务端：列出（有规则、没文件、坏文件）；删除（删掉一条后下一次判断命令就按新规则、重复的一起删、要删的不在了、坏文件拒绝且文件不变、删除和「以后都放行」同时写不丢规则不写坏、文件权限仍是 0600）；旧客户端兼容。
  - 终端：分组显示、确认后才删、Esc 取消不删、没规则和坏文件的提示、命令列表和帮助里有 `/permissions`。
  - 再跑相关 crate 的测试、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。全套由主控合并前跑。觉得不够，在回复里说，不要自己加。

## 不要做

- 不在 `/permissions` 里加规则、改规则；不显示、不管「本会话都放行」那种会话里的临时授权。
- 不改规则的判断逻辑（前缀匹配、冲突取舍、只读白名单、批准框的建议前缀）和 `append` 的行为。
- 客户端不直接读写 `rules.json`，一律走服务端接口。
- 不做项目级规则、通配规则。
- 测试用隔离的 HOME 和 `CANTER_HOME`、临时端口，不读用户真实的 `~/.canter`；不真调模型接口。
- 不做绕过测试：不专门构造输入去骗过检查。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-156-permissions` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
