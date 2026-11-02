# 任务：第二期 - 接 opencode go 订阅（配置里写额外请求头）

2026-11-02，主控建。
2026-11-02，canter/main 交给 canter/dev-opencode-go（Codex，gpt-6-astra / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.64）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：用户想像 pi 一样用 opencode go 订阅。opencode go 用的就是 canter 已有的 Chat Completions 格式，但它要求每个请求带 `x-opencode-session`（每个会话固定一个值），还要求客户端用自己的 User-Agent。本件让 canter 能接上它：核心只多传一个通用的会话 ID，模型层加通用的「配置里写额外请求头」，opencode 只出现在用户的配置里，代码里不出现。一次只连一家，几家同时配、`/model` 切换放第三期（和 P2-80 接 GPT 一起）。
执行：派出
优先：高

## 用户原话

- 11-02 子 agent 长程试跑时 DeepSeek 官网余额用完（402）。用户：「帮我换成和pi一样的配置。pi使用的opencode go订阅」。主控照 pi 改了配置，试跑会话报 400。用户同意后，主控直接给 opencode go 发了 1 次最简单的测试请求，回的是 `MissingSessionID`：「Request is missing x-opencode-session」。
- 用户：「我在想啊，这算不算是接入第二家？我觉得不能直接修改deepseek的，而是之前咱们商量好的插件形式接入第二家」。主控对照了 pi：pi 自带 opencode，认出地址就自动加请求头，不是插件；配置能做到的用 `models.json`，配置做不到的才写扩展。
- 用户：「我理解你的意思，就是现在的情况下，就接入一家，多家的接入放到3期那个。对吧？」主控：对。
- 用户：「这样的话，opencode的接入是不是会污染到比较核心的，还是说opencode go的订阅接入也是类似一个插件？」主控给了两种：A 配置里写额外请求头、值里可用 `{session_id}`，代码里不提 opencode；B 像 pi 那样在代码里认出 opencode。主控建议 A。用户：「可以」。

## 先读

- 仓库根的 `CLAUDE.md`（项目规矩）。
- `docs/DESIGN.md`：第 6 条（模型和账号）、第 17 条（缓存）、第 23 条（核心不绑 DeepSeek）、「模型请求重试（P2-110）」一段、搜网页的执行（P2-106）一段。
- `crates/canter-model/src/types.rs`（`Request`、`RequestOptions`）、`chat_completions.rs`（发请求、请求头、错误）、`wire.rs`（请求体）。
- `crates/canter-server/src/config.rs`（配置字段、校验、`model_client`）、`crates/canter-server/README.md` 里写配置字段的那段。
- opencode go 的要求（主控 11-02 读的 https://opencode.ai/v2/docs/console/go/ 「Where can I use it」一节）：发普通 coding agent 的请求；用自己的 User-Agent，比如 `my-coding-agent/1.0`，不用 HTTP 库默认的名字；每个会话在 `x-opencode-session` 里带一个固定的会话 ID，用来分流和做缓存。

## 在哪里干活

- worktree：`../canter-worktrees/p2-185-opencode-go`，分支 `p2-185-opencode-go`（已从 main 建好）。
- 编译目录：cargo 命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-185-opencode-go`。

## 要做的

### 1. 模型请求带上会话 ID（通用）

- 模型层的请求加一个通用的会话 ID（可选），说明里写它是「这次请求属于哪个会话」，不写哪一家用它。
- 核心发模型请求时都填上：这次请求属于哪个会话，就用哪个会话的 ID。主会话的对话、压缩、`/review`、顺口问、交接、子 agent 都算；子 agent 用它自己子会话的 ID。同一个会话里，压缩前后、重开会话、服务端重启后，ID 都不变。
- 核心不看这个 ID 发到哪家、怎么用。

### 2. 配置里写额外请求头（通用）

- `config.json` 加可选的 `headers`：名字 → 值，都是字符串。每次模型请求都带上这些头。
- 值里的 `{session_id}` 换成这次请求的会话 ID；请求没有会话 ID 时，这个头不发。只认这一个占位符，别的文字原样发。
- 请求头名字或值不合法，或者和 canter 自己设的头（`Authorization`、`Content-Type`、`User-Agent`）同名（不分大小写），算配置错误，照现有配置错误的处理办法来（P2-154：沿用旧配置并提示）。
- 改了 `headers`，下一轮生效，和其他配置一样（P2-154）。
- 请求头的值当成和 `api_key` 一样敏感：不进日志、事件、错误信息、遥测和测试快照。
- 只管 Chat Completions 的模型请求。搜网页、读网页不带这些头。

### 3. 模型请求用 canter 自己的 User-Agent

- 所有模型请求带 `User-Agent: canter/<版本>`，版本取 canter 自己的版本号。不论哪一家都带，不需要配置。

### 4. 文档

- `crates/canter-server/README.md` 的配置字段加 `headers`，写一个接 opencode go 的例子：
  ```json
  "base_url": "https://opencode.ai/inference/go/openai/v1",
  "model": "deepseek-v4.1-flash",
  "headers": { "x-opencode-session": "{session_id}" }
  ```
- DESIGN 第 6 条后补一段：一次连一家；额外请求头和 `{session_id}`；User-Agent；opencode go 用这个接（只写在配置和文档的例子里）；接 opencode 时没有搜网页（P2-106 只认官网 DeepSeek 地址）；几家同时配放第三期。写上用户原话。

## 怎么算做完

- 用户定的做到：opencode go 按 A 接，代码里（文档例子之外）不出现 opencode；核心只多了通用的会话 ID。
- 上面第 1～4 节做到。
- 验证只做这些：
  - 每条行为写针对的测试（本机假服务端收请求看请求头，不调真实模型）：
    - 配了 `headers` 时请求头带上，`{session_id}` 换成了对的会话 ID；
    - 同一个会话压缩前后 ID 一样；两个会话 ID 不同；子 agent 带的是自己的 ID；
    - 没配 `headers` 时请求体和以前逐字节一样，请求头只多了 User-Agent；
    - 不合法和同名的 `headers` 报配置错误，错误信息里没有请求头的值；
    - 模型请求出错时，错误、事件和遥测里没有请求头的值；
    - User-Agent 是 `canter/<版本>`。
  - 改了哪个 crate 就跑哪个 crate 的全部测试（`cargo nextest run -p <crate>`）；
  - 加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`；
  - `grep -rni opencode crates --include='*.rs'` 没有结果（测试里也不写 opencode，用别的名字的请求头）。
  - 不跑全套。觉得不够，在回复里说，不要自己加。
- 装好以后，主控在用户在场时用 opencode go 真发一两次确认，不在本件里做。

## 不要做

- 不在代码里认 opencode 的地址或名字，不写 opencode 专用的逻辑（用户选了 A）。
- 不做几家同时配、`/model` 跨家切换、Responses / Anthropic / Google 接口（第三期，P2-80）。
- 不做插件（P2-21）。
- 不改搜网页的规矩（还是只认官网 DeepSeek 地址）。
- 不改请求体、不改提示词和工具定义；现有请求夹具不该变，变了就是改错了。
- 不往内置价格表加 opencode 的模型；价格照现有办法写在用户配置的 `prices` 里。
- 不在核心逻辑里按 DeepSeek 的名字、接口地址、请求字段或缓存行为分支（DESIGN 第 23 条）。
- 不调真实模型接口（DeepSeek、opencode 都不调）。
- 界面和发给模型的文字一律英文。
- 实现、测试和辅助命令不用 Python 或别的脚本语言，只用 shell 和 Rust。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME、canter 状态目录、临时端口和临时 socket。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：停自己起的服务用起的时候记下的 PID。
- 不在 Cargo 里引用 ranch、paddock、cairn 的包，不从 pi、opencode 搬代码（只参考）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。包括：要改 HTTP/SSE 协议；某类请求找不到它属于哪个会话。
- 不合并到 main，不推送。只在 `p2-185-opencode-go` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
