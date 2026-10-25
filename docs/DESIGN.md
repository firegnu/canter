# canter 设计

canter 是用户自己做的 coding agent。本文件记已经定下的事和来由；实现中要改设计，先改这份文档。

调研报告在 `docs/调研/R49-自研agent调研.md`，是从 ranch 仓库（`github.com/firegnu/ranch`）的同名文件原样复制来的（2026-10-23）。报告里的 `docs/任务/…`、`crates/corral/…`、DESIGN §7–§10 等路径指 ranch 仓库；`L`、`W` 编号的出处见报告末尾。报告写完后用户又定了几件事，和报告推荐不同的以本文件为准（见下面「和报告不同的地方」）。

## 用户已定的

2026-10-23，用户在 ranch 主控那里定的。

1. **独立，不和 ranch 这一套绑死**：「这个coding agent不需要和ranch/corral/paddock/cairn等绑定死。这样我随时可以在任何terminal中启动。」canter 仓库里不放任何 ranch 的东西；以后要接 corral，由 corral 去适配 canter 的公开接口。
2. **一台机器一个常驻服务端、多个客户端**：「我要的是一个服务器，多个客户端。主要的考虑是，我肯定会先做tui，然后对接上corral这一套，但是也可以单独发布桌面app」。循环、工具、权限、会话都在服务端；终端界面、桌面应用、网页、corral 都是客户端。
3. **语言 Rust**：服务端和终端客户端用 Rust。用户：「我接受你的建议Rust」。
4. **模型不自研**：「我是想拥有一个自研的agent。model就算了。」
5. **做到什么程度**：「照着调研文档来，但是初步能达到pi我就很满意了」。架构照报告「冲着 Claude Code、Codex 的标准分」，不留死路；第一个目标是能替掉 pi 干日常的活。
6. **模型和账号**：用户有 ChatGPT Pro 订阅和 DeepSeek 的 BYOK（自己的 API key）。第一期两个都接：
   - ChatGPT Pro：经 OpenAI 的「Sign in with ChatGPT」，走 Responses 接口（报告 §4.4）。
   - DeepSeek：API key，走 Chat Completions 接口。
   - 不用 Claude 订阅：「不要查claude的条款，我不会拿claude订阅来做接入自研的agent的」。
   - **2026-10-24 改：第一期只接 DeepSeek。** 用户：「这样，我觉得第一期先不做订阅的接入。就是BYOK」。BYOK 只接 DeepSeek，不接 OpenAI 的 API key。所以第一期只做 Chat Completions 接口，base_url 和 key 可以配置。ChatGPT Pro 登录和 Responses 接口往后放。模型接入层仍按多家来做，以后加 Responses 不用推翻。
   - 跟着改的：第一期改文件用「整文件写 + 按文字替换」；Codex 的 apply_patch 是给 GPT 用的，等接 OpenAI 时再搬（用户 10-24 选定）。
7. **名字和仓库**：`canter`（马的「慢跑」步法），仓库 `github.com/firegnu/canter`，公开，Apache-2.0。用户：「开源，repo就放在github上」。
8. **协议：结构照 Codex，传输和类型 canter 自己定（报告 §5.7 的 C）**。2026-10-24 在 canter 仓库定的，用户：「好，就用 C，改 DESIGN」。
   - 经过：用户先说「协议我觉得做好使用成熟的现有的coding agent使用的协议」，选了 Codex app-server 协议，不用 ACP（「ACP我一直不是很看好」）。10-23 用户又提醒「我们选的 Codex app-server 协议是 OpenAI 自家的协议，不是公共标准，会跟着 Codex 版本一起变」。可选的有两条：A 是以 Codex 某版为底拷一份、自己管；C 是自定 HTTP + SSE（报告 §5.7）。最后定了 C。
   - 传输：客户端用普通 HTTP 请求下命令；服务端用一条 SSE 长连接按顺序推事件，每条带递增序号，断线后从序号接着收（报告 §5.2 的甲，传输学 opencode）。
   - 结构：照 Codex app-server v2，会话 → 一轮 → 一条条项目；批准、提问是服务端等客户端回答的请求；事件分开始、增量、完成。字段名尽量和 Codex 一样。只参考，不拷 Codex 的协议代码；参考的版本写进任务文件。
   - 协议类型放单独的 Rust 包，从它生成 JSON Schema 和 TypeScript 类型；agent 核心和协议之间隔一层转换。
   - 理由：A 拷过来、不跟上游以后，同样是 canter 自己维护的协议，和 C 只差传输方式和拷不拷类型代码。传输上 HTTP + SSE 更贴合 canter：一台机器多个客户端，断线能接着收，以后 corral、pony 这类同步程序直接接，curl 就能排错。
   - 代价：离用户原来要的「使用成熟的现有的coding agent使用的协议」远一步，用的是 Codex 的结构，不是它的协议本身；协议包要自己写，估 1,000–1,500 行（报告 §9.1）。
   - **和 Codex app-server v2（`rust-v0.163.0`）不一样的地方**（第 1 件定的，细节见 `docs/任务/P1-1-协议包和存盘.md` 的完成记录和「审查后修改」，以及 `crates/canter-protocol/README.md`）：
     - 命名：`session`、`sessionId` 代替 `thread`、`threadId`；事件名用点号（`session.status`、`turn.started`、`item.delta` 等）。每条事件外层带 `seq`（每个会话从 1 起）、`sessionId`、`timestamp`（Unix 毫秒）；快照带 `lastEventSeq`；命令的回复带 `eventSeq`。
     - 会话设置：新增 `session.updated`，建会话时和每次修改后各写一条，带公开设置：名字、模型、权限模式、工作目录。环境变量不公开，也不存盘（10-24 改，见第 14 条）。
     - 思考档位（P2-2）：Codex 的 `ThreadSettings`、`ThreadSettingsUpdateParams` 和 `TurnStartParams` 用 `effort`，`thread/start`、`thread/resume`、`thread/fork` 的回复用 `reasoningEffort`。canter 选择回复里的 `reasoningEffort`，统一用于会话设置、创建和 PATCH 参数及 `session.updated` 存盘，理由是明确表示模型推理强度，并让请求、通知和存盘使用同一个名字。省略时新会话和旧存盘均按 `high` 读；PATCH 省略/null 保留已知档位，已存档位为 Unknown 时改写为实际生效的 `high`。值用 DeepSeek 的 `none`、`low`、`high`、`max`，不照搬 Codex 的档位枚举；canter 以会话设置管理之后每次请求的选项。TypeScript 新字段可选。
     - 可用模型（P2-2）：`GET /v1/models` 对应 Codex 的 `model/list`，回复沿用 `data`、`nextCursor`，模型条目沿用 `id`、`model`、`displayName`、`isDefault`。省掉 Codex `Model` 的 `description`、`hidden`、`supportedReasoningEfforts`、`defaultReasoningEffort`、`inputModalities`、`upgrade`、`upgradeInfo`、`availabilityNux`、`modelSpecialty`、`supportsPersonality`、`multiAgentVersion`、`additionalSpeedTiers`、`serviceTiers`、`defaultServiceTier`、`availableAccessPrograms`：本机配置只提供模型名和默认模型，当前终端选择只需这些，四个思考档位和默认 `high` 按 DeepSeek 统一处理，不声明逐模型的其他元数据。`ModelListParams` 省掉 `cursor`、`limit`、`includeHidden`，因为本机配置列表一次完整返回、不区分隐藏项，`nextCursor` 恒为 null。建会话和 PATCH 只接受列表内的模型，已存的旧模型仍可恢复。
     - 技能（P2-3）：`GET /v1/sessions/{id}/skills` 返回会话打开时固定的 `{ data: [{ name, description, path, source, modelInvocation }] }`，按名字排序，未打开时返回 `session_not_resumed`。参考 Codex `skills/list` 的 `data` 和技能的 `name`、`description`、`path`；canter 只查当前会话，不接受 `cwds`、`forceReload`，也不按 cwd 再嵌一层，以保持会话中途的模型前缀不变。`source` 为 `global` / `project`，代替 Codex `scope` 的 user/repo/system/admin，因为本轮只读全局和项目两处；`modelInvocation` 明确表示模型能否自行使用（对应技能头部 `disable-model-invocation` 的反值），不混用 Codex 的配置开关 `enabled`。不提供 shortDescription、interface、dependencies、pluginId 等本轮没有的数据；读取错误沿用会话打开时的 warning 事件，不在列表里重复返回 errors。列表只驻内存，不新增存盘格式。
     - 接着聊：参考 `thread/resume`，用 `POST /v1/sessions/{id}/resume { env }` 重新提供仅驻内存的环境；已打开时保留原环境，未打开时变更命令返回 `session_not_resumed`，快照和事件仍可只读访问。
     - 会话状态是六种字符串（`starting`、`idle`、`working`、`waiting_approval`、`error`、`closed`），不是 Codex 带标记的对象。权限模式的字段名沿用 `approvalPolicy`，值是 canter 自己的 `alwaysAsk`、`askForExecution`、`allowAll`；不要 Codex 的沙箱设置。
     - 关闭原因（P1-13）：`session.status` 事件和会话快照新增可选的 `closeReason`，关闭时为 `clientRequest`（客户端主动关）或 `serverShutdown`（服务端停机，包括升级）；离开 `closed` 时清空。原因随事件存盘，避免服务端重启后旁观终端撤销人的关闭意图。终端连接新服务端时，仅对停机关闭或缺少原因的旧会话自动 resume；未知关闭原因保持关闭，非关闭状态仍按崩溃恢复。用户主动 `--resume` 或在列表里选择会话不受此限制。
     - 送话：一条 input 命令带 `mode: queue / steer`，代替 Codex 的 `turn/start`、`turn/steer`。排队时发 `input.queued` 事件，快照里有 `queuedInputs`。
     - 新增 `input.removed { inputIds, reason, turnId? }`，用于插话/合并消费、打断/关闭丢弃，避免这些路径在恢复和客户端快照中留下已处理的输入；保留 `turn.started.inputId`。插话回复的 `eventSeq` 指接收时的 `input.queued`，`turnId` 指当前目标轮，实际进入上下文时再移出。
     - 手动压缩命令在独立压缩轮的 `turn.started` 落盘后即回复 `{ eventSeq, turnId? }`，结果通过事件报告，避免 HTTP 等待整个摘要过程；压缩轮期间的插话按排队处理，完成后另开一轮。
     - 一轮结束：`turn.completed` 只更新状态、错误、用量，项目以 `item.*` 事件为准，和 Codex 一样。
     - 项目种类：沿用 Codex 的 `userMessage`、`agentMessage`、`reasoning`、`commandExecution`、`fileChange`、`contextCompaction`；新增 `fileRead`；增量统一走 `item.delta`，按种类区分。
     - 待处理请求：外层 `id`、`turnId`、`createdAt`，内层带 `type`（第一期只有批准）。四个回答对应 Codex 的 `accept`（允许这一次）、`acceptForSession`（本会话都允许）、`decline`（拒绝）、`cancel`（拒绝并停下）。`request.resolved` 比 Codex 多 `resolution`（回答、取消、超时）和 `clientName`。
     - 提问（P2-4）：`ask_user` 加在现有工具末尾，参数为 `question`、`options: [{label, description}]`。参考 Codex `ToolRequestUserInput*`（`rust-v0.163.0`），沿用 question/options/label/description/itemId；`request.opened.kind` 新增 `type: userInput`，带 `itemId`、`question`、`options`。单次只问一题，省掉 questions 数组、题目 id/header、isOther/isSecret、isBlocking/autoResolutionMs：请求外层已有 id/turnId，会话由事件标识；固定允许自写、阻塞等待、不隐藏输入、不超时。
     - 回答仍走解决请求接口：`decision` 改为可选，新增可选 `answer`，两者必须恰有一个且匹配请求类型。`answer` 为 `{type: option, label}` 或 `{type: text, text}`，代替 Codex 按题目 id 映射的 answers 字符串数组，因为这里单题单选且要明确区分选项和自写文字。空白或不匹配的回答返回 `invalid_request`，不改变状态；批准的非法 decision 仍返回 `invalid_decision`。`request.resolved.resolution` 新增 `type: userInput` 和 answer；取消、重复处理、重开会话沿用批准机制，等待状态仍用 `waiting_approval`（终端按请求类型显示等待回答）。新增 `userInput` 项目，带 id/question/status 和可选 answer，供快照及重连后的对话保留问答；模型工具结果为 `{question, answer}`。旧客户端的批准请求 JSON 不变，新字段在 Schema/TypeScript 中可选，未知请求、结果和项目沿用 Unknown 处理。
     - 兼容：枚举都有 `Unknown`；不认识的事件、项目、增量、枚举值都跳过，不让整条解析失败。
     - 传输：出错的回复是 `{ code, message }`。SSE 每帧 `id:` 是序号，续传时 `Last-Event-ID` 优先于 `after` 参数。
9. **桌面应用用 GPUI**（和 paddock 一样）。
10. **第一版范围**：照报告「要用户定的」第 6 条的推荐，模型改成只接 DeepSeek（第 6 条 10-24 的改动）：不做 OpenAI Responses 接口和 ChatGPT 套餐登录，改文件不用 apply_patch。
11. **服务端怎么起、怎么升级**：照报告第 7 条的推荐：客户端找不到服务端就在后台起一个，另可装成用户服务；升级第一期等在跑的轮跑完（有上限）再重启、不自动换版本，第二期接 corral 派活前做排空交接；嵌在终端里的单机模式只作备选。
12. **接 corral 往后放**：「corral的接入等agent差不多可以工作了（tui版本的），再考虑接入。」
13. **会话存两份：事件和模型历史**（2026-10-24，第 1 件交叉审查后用户选定）。每个会话除了发给客户端的事件文件，另存一份只追加的模型历史，原样记下发给模型的每条消息（工具调用 id、原样参数、截断后回给模型的结果、压缩后的摘要等），不发给客户端，文件 0600。恢复会话时，模型的上下文从模型历史还原，不从显示用的事件反推。Codex 也是这样分的（rollout 文件存模型层的条目，和 app-server 的通知分开）。
14. **会话环境和 API key**（2026-10-24，派 P1-4、P1-5 前用户选定，都照主控的建议）。
   - 环境变量不存盘：建会话时客户端交来的环境只放服务端内存；服务端重启后，客户端接着聊时重新交一份（用接着聊那个终端的环境）。理由：环境变量里常有令牌，不落盘；服务端重启后在跑的轮本来就断了，接着聊总得有客户端连上来。第 1 件原来把环境存进会话目录的 `private.json`（0600），P1-4 拿掉。
   - DeepSeek 的 API key 放 canter 的配置文件（状态目录下，0600，不进仓库）。理由：服务端是在后台自己起的，以后还可能装成系统服务，拿不到用户终端里的环境变量。
   - canter 的状态目录和配置文件是 read、write、edit 工具的受保护路径，模型读写都拒绝（P1-3 交叉审查后主控定）；shell 拦不住。
   - 只读 shell 命令不弹批准框（P1-9，2026-10-24 用户定：白名单判断；碰状态目录按路径拦，写明挡不全）。「每次问」「只问执行」两种模式下，白名单里的命令（列目录、看文件、搜索、git 查看类，组合命令每段都要在白名单里）直接跑；命令里写出的路径落在受保护路径里（不分大小写）、递归搜索的起点是状态目录或它的上级、git 配置里有会起程序或联网的设置，都照旧问。细节见 `crates/canter-tools/README.md`。已知限制：这不是沙箱，`$HOME/.canter` 这类变量写法一律问但环境变量不追踪，符号链接、硬链接、文件系统别名、执行期间的路径变化、程序内部的隐式读取都挡不全，sort 处理大输入时会在 TMPDIR 写自己的临时文件（P1-12）；要挡全得等套墙。
15. **状态目录**（主控定，P1-5 合并时写入）：`~/.canter/`，可用环境变量 `CANTER_HOME` 改成别的绝对路径（测试靠它隔离）。目录 0700，里面的文件 0600。布局见 `crates/canter-server/README.md`：`config.json`（模型地址、模型名、上下文窗口、API key）、`server.lock`（一台机器一个服务端，以它为准判断服务端在不在）、`server.json`（登记）、`server.sock`、`server.log`、`startup.lock`（客户端后台起服务端时串行）、升级交接用的 `replace.lock` / `replace.json`，会话在 `sessions/<id>/`。服务端只听这个 unix socket，不开 TCP。升级要显式 `canter serve --replace`，新的等旧的手上在跑的轮跑完，最多 60 秒，超时的轮记为被打断。
16. **装到哪、怎么退回**（2026-10-24，P1-8 开始时用户照主控的建议定：「按你的建议装，开始吧」）。第一期用 `cargo install --locked --path crates/canter` 从本地 main 装，程序在 `~/.cargo/bin/canter`（已在用户 PATH 上），不装成系统服务，服务端由客户端在后台起。换版本：重新 `cargo install`，再 `canter serve --replace` 让新服务端接手。退回：停掉服务端（按 `~/.canter/server.json` 里的 pid 发 SIGTERM），`cargo uninstall canter`；`~/.canter/` 里有会话和 key，删不删由用户定。
17. **照顾缓存命中**（2026-10-25，用户同意主控的建议：「同意，加进单子，AGENTS.md 只在打开时读」）。DeepSeek 按前缀缓存，请求开头和上次一样的部分才命中，命中的价格低很多。规矩：发给模型的内容只往后加，前面不改；压缩和切模型是仅有的例外。系统提示词里的日期和项目的 `AGENTS.md` 只在打开会话时定下来，会话中途改了 `AGENTS.md`，下一个会话才生效（Codex 也这样）。第一期是每次请求都重读，第二期 P2-15 改。
18. **终端界面改成全屏**（2026-10-25，用户：「现在再终端中运行canter的时候，就感觉像是一个普通的bash程序，而不是像其他的coding agent那种直接托管整个屏幕了，而且输入框始终在最下面」；听了取舍后：「都能接受，要鼠标滚轮，只做全屏」）。改掉第一期「不占全屏」的做法（调研报告 §8.2 的推荐）：canter 打开后整屏归程序画，对话在上面、输入框固定在屏幕最下面，像 pi、opencode。用户接受的代价：往上翻、搜索靠程序自己做；程序接管鼠标滚轮后，选中复制要按住修饰键再拖；退出后屏幕恢复原样，对话不留在终端里；`corral read`、tmux 里只看得到当前屏幕。只做全屏，不留旧模式的开关。第二期 P2-16 改。

## 和报告不同的地方

- **协议**：最后和报告 §5.2 的推荐一样，自定「HTTP 发命令 + SSE 收事件」（甲）。中间用户一度定了 Codex app-server（乙），10-24 改成 C，即甲的传输加 Codex 的结构（第 8 条）。报告 §5 里讲乙、讲 WebSocket 的部分不再适用；讲数据结构（会话 → 一轮 → 项目）、待处理请求、断线续传的部分照用。
- **模型**：报告推荐第一期只接 Responses 接口，加 ChatGPT 套餐登录；用户 10-24 定为第一期只接 DeepSeek（BYOK），正好反过来：只做 Chat Completions 接口，Responses 和套餐登录往后放。模型接入层一开始就按多家来做。
- **改文件**：报告推荐第一期用「整文件写 + apply_patch」，等接开源模型时再加「按文字替换」；第一期只接 DeepSeek，所以改成「整文件写 + 按文字替换」，apply_patch 等接 OpenAI 时再加。
- **会话存盘**：报告 §2.3 建议只存事件、「存的和发给客户端的是一样的东西」；改成事件和模型历史各存一份（第 13 条）。
- **接 corral**：报告 §6、§9.2 把接 corral 排在第二期；用户定为终端版本差不多能用之后再考虑，到时再定做法（报告「要用户定的」第 9 条的五种做法留作参考）。

## 还没定的

- 第一期怎么拆成具体的活（报告 §9.1 推测 7 件左右）。
- 从 Codex 搬哪些代码（`apply-patch` 等接 OpenAI 时再定；协议类型不搬，见第 8 条），搬来的代码要保留 Apache-2.0 声明，按需加 NOTICE。
