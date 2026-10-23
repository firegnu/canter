# R49：自研 coding agent 调研

2026-10-23，ranch/dev-r49。只调研，不写代码、不定架构。同日按用户意见改过一次：主推荐改成一台机器一个常驻服务端，§6「接 corral」重写（经过见任务文件的修改记录）。依据：`docs/任务/R49-自研agent调研.md`（用户原话、现状、要摸清的八条、要用户定的六条），ranch 的 `docs/DESIGN.md` §7–§10、`docs/CONTRACT.md`、`crates/corral/src/agents/`。

看了谁：Codex、opencode、pi、omp 四家的源码（按本机装的版本取标签，克隆到 `/tmp/r49-src/`，只读），OpenAI、ACP、Ollama、vLLM、llama.cpp 的官方文档，crates.io 和 GitHub 的公开数据。出处在文末，网上的都是 2026-10-23 查的。

怎么读：

- **查到**：有出处的事实，出处编号见文末（`L` 是本地读的源码，`W` 是网页）。
- **推测**：我根据查到的东西推出来的，没验证。
- **不知道**：查了没查清，或者没查。
- 工作量都是推测。
- Claude 订阅：用户 10-23 说「不要查claude的条款，我不会拿claude订阅来做接入自研的agent的」，本报告不涉及。

---

## 0. 一页看懂

**核心理念。** 聪明的是模型，agent 不聪明。agent 是把模型接到电脑上的那一层，它反复做三件事：

1. 把对话、文件内容、命令输出整理好送给模型。
2. 模型回两种东西：要对人说的话，或者「我要调用某个工具」，比如读这个文件、跑这条命令。
3. agent 照着去做，把结果再送回模型。

模型不再要工具时，这一轮就结束了。所以一个 agent 就是这几样：一个循环、一组工具、一套「什么时候要先问人」的规矩、一份把这些都记下来的会话记录。

**服务端和客户端分开，一台机器一个服务端。** 用户 10-23 定的：「一个服务器，多个客户端」，而且「不需要和corral绑定死」。服务端每台机器一个、常驻在后台；循环、工具、权限、会话都放在服务端。客户端只做两件事：显示服务端发来的事件，把人打的字和人做的决定送回去。客户端不碰文件，也不调模型。所以：

- 终端、corral、pony、网页、桌面应用都是同一种客户端，看到的是同一串事件。
- 关掉终端，会话还在服务端里；换一个终端、网页或手机接着看。
- 一个会话可以同时被几个客户端看。
- agent 不依赖 corral。corral 要的暂停、套墙、停止，由 corral 去适配 agent 的公开接口（§6）。
- 「agent 现在在干什么」由服务端直接说，不用再靠钩子和读屏去猜。现在 corral 为各家 agent 写的一堆认打断、认提问框、认 Bash 命令的特例（R22–R26、R43、R47c），对自研 agent 都不需要。

**八条结论。**

| # | 问题 | 结论 |
| --- | --- | --- |
| 1 | 至少要哪些部分 | 第一版必须有：循环、模型接入、上下文组装、读、写、改文件、shell、权限确认、打断、会话存盘和接着聊、简单的压缩。可以后补：提问框、计划、技能、MCP、子 agent、钩子、网页搜索、图片（§2） |
| 2 | 能借鉴什么 | 协议和服务端的分法学 Codex（Apache-2.0，Rust）和 opencode（MIT）；核心循环学 pi（MIT，循环约 1,600 行）；改文件的办法看 Codex 的 apply_patch 和 omp 的 hashline。四家里只有 Codex 是 Rust，它的 `apply-patch` 能按许可证直接搬，别的模块和它内部耦合太深，搬不出来（§3） |
| 3 | 模型怎么接 | 没有官方 Rust SDK。建议自己写一层薄的：先只做 OpenAI Responses 接口，接本机或 5090 上的开源模型时再加 Chat Completions 接口。**查到 OpenAI 9 月底起有官方预览「Sign in with ChatGPT」：ChatGPT Plus、Pro 用户能把自己的套餐给开源或在本机运行的 app 用，不用 API key，不另花钱**（§4） |
| 4 | 一个服务端、多个客户端 | 一台机器一个常驻服务端：客户端找不到就在后台起一个，也可以装成系统服务；嵌在终端里的单机模式只作备选。数据结构学 Codex（会话 → 一轮 → 一条条项目，每条有开始、增量、完成），传输学 opencode（HTTP 发命令、SSE 收事件，事件带序号，断了能补）。批准和提问做成「待处理请求」：发给所有客户端，谁先答算谁的。服务端升级时，在跑的轮先跑完再交给新版（§5） |
| 5 | 怎么接进 corral | agent 不依赖 corral，corral 去适配。pen 里照旧开终端客户端，连常驻服务端；先写适配器，状态靠 agent 的通用钩子；暂停、停止走 agent 的会话命令；套墙靠 agent 给每个会话一个执行器子进程（动文件、跑命令都在里面），corral 把 fence 当启动前缀传进去；以后 pen 直接订事件流。做不到的：一堵墙围住整个服务端，冻住已经发出去的模型请求（§6） |
| 6 | 和 ranch 其他部分 | 技能读 `~/.agents/skills`，corral 不用改；fence 围住每个会话的执行器，围不住服务端本身；分工表只加一份数据；pony 的按钮以后可以按请求号批准，不读屏（§7） |
| 7 | 放哪、用什么做界面 | 倾向单独一个仓库。终端客户端用 ratatui + crossterm，先做不占全屏的；服务端用 tokio + axum（§8） |
| 8 | 分期和工作量 | 第一期（常驻服务端 + 终端客户端，能干一件小活）推测 1.2 万–1.9 万行 Rust，不算测试，拆 7 件左右；第二期接 corral 推测 agent、corral 两边合起来约 8–10 件小到中等的活（§9） |

---

## 要用户定的（每条给选项和推荐）

### 任务书列的六条

**1. 自研到什么程度**

- **A（推荐）先做一个能替掉 pi、omp 干低档活的。**
  - 第一期、第二期做完就是这个水平：OpenAI 模型、七八个工具、能问人、能打断、能接着聊、corral 认得出状态，分工表里低档可以选它。
  - 架构照「冲着 Claude Code、Codex」的标准分：协议、会话、事件不留死路，以后加功能不用推翻。
- **B 一开始就冲着替掉 Claude Code、Codex 去。**
  - 量级差得远：Codex 不含 vendor 约 187 万行 Rust（含测试，L1），opencode 约 79 万行 TypeScript（L2）。
  - 它们的大头是各种边角功能：沙箱、插件、MCP 的各种认证、远程控制、云端任务、实时语音等。
  - 一开始就追，第一期做不出能用的东西。
- **C（不推荐，列出来作对照）只做客户端，核心用 Codex 的 app-server。**
  - Codex app-server 是 Apache-2.0，协议公开；官方文档说它也能用 ChatGPT 套餐的令牌（W1「Codex app-server」节）。
  - 好处：功能马上就全。
  - 坏处：核心不是自己的，只能用 OpenAI 的 Responses 接口（Codex 只认这一种，L1 `model-provider-info/src/lib.rs`），协议跟着 OpenAI 变。这不满足「拥有一个自研的 agent」。

**2. 用哪家模型、花不花钱**

- **A（推荐，第一期）ChatGPT 套餐，经「Sign in with ChatGPT」。**
  - 查到（W1）：Plus、Pro 用户可以授权一个开源或在本机运行的 app，用自己的套餐调 OpenAI 的 Responses 接口。不用 API key，用量算进套餐。
  - Plus 有个五小时额度，所有用套餐的 app 共用一份，一个 app 用了就算进同一份总数，没有哪个 app 另有额度；Pro 没有这个五小时限制（W1「plan usage」节）。所以自研 agent 和 Codex 会用掉同一份套餐额度（推测：Codex 也算「用套餐的 app」）。
  - 限制：还是预览；只能用 Responses 接口；要 `store:false`、`stream:true`；不收某些参数；不能用 OpenAI 托管的工具（文件搜索、代码解释器、托管的 MCP）；自己定义的函数工具可以用（W2）。
  - 条款要求（W3，原页面挡了抓取，下面是搜索摘要里的内容）：请求必须从用户自己的或只归用户管的运行环境发出；套餐只能给连上的这个 app 用；不能拿它给别的工具当通用接口。
  - **要用户告诉我们：** 现在的 ChatGPT 是 Plus 还是 Pro。
- **B OpenAI API key，按量计费。** 要花钱，但没有预览的那些限制。开发时也能临时用。
- **C 本机或 5090 上的开源模型。**
  - 查到：Ollama、vLLM、llama.cpp 都提供 OpenAI 兼容接口：都有 Chat Completions，都有 Responses；vLLM 和 llama.cpp 还兼容 Anthropic 的 Messages 接口（W11–W13）。
  - 不花钱，但能力弱一档；要第二种接口（Chat Completions）才稳。
- **D 别家按量的 OpenAI 兼容接口**（DeepSeek、Kimi、GLM、OpenRouter 等）。便宜，要 API key；接口同 C。
- **推荐：** 第一期只接 A（同一接口也收 API key，所以 B 顺带就有）。第二期以后加 Chat Completions，接 C、D。这样第一期不另花钱。

**3. 放哪个仓库、叫什么名字**

- **A（推荐）单独一个新仓库。**
  - 理由：
    - 用户要它「不和 ranch/corral/paddock/cairn 等绑定死」。
    - 它要 tokio、axum、ratatui、crossterm，ranch 的规矩不让依赖 ratatui、crossterm。
    - 发布节奏和 ranch 不同。
    - 想用 ChatGPT 套餐，最好开源，见下面第 8 条。
  - ranch 这边：corral 的适配器、分工表里的一家、fence 的一份执行器用的数据；要暂停、套墙、结构化批准，还要给 corral 加几样通用能力（§6）。这些都是 corral 去适配 agent，agent 仓库里不放任何 ranch 的东西。
- **B 放进 ranch。** 好处只有共用打包和 `ranch deploy`。坏处：要为它放开 ratatui、crossterm 的规矩；它会变成「每台机器一份的运行时」的一部分，和「随便哪个终端都能开」的独立产品定位不符。
- **名字：** 沿用马和牧场的叫法，候选：
  - `colt`（小公马，第一匹自己养的马）
  - `foal`（马驹）
  - `filly`（小母马）
  - 查过（2026-10-23）：本机 PATH 上都没有这几个命令，Homebrew 都没有同名 formula，crates.io 上 `colt` 只是占位包。
  - 别用 `wrangler`：那是 Cloudflare 的命令行，crates.io 和 npm 上都是它。
  - 推荐 `colt`，用户定。

**4. 服务端和客户端之间的协议**（详见 §5）

- **甲（推荐）自定，HTTP + SSE。**
  - 什么样：
    - 客户端用普通的 HTTP 请求下命令（送一句话、打断、回答批准框）。
    - 服务端用一条 SSE 长连接按顺序推事件，每条带递增的序号。SSE（Server-Sent Events）是浏览器原生支持的「服务端单向推送」格式，就是一行行文字。
    - 数据结构用 Codex 的「会话 → 一轮 → 项目」。
  - 参考 opencode 的传输、Codex 的数据结构。
  - 好处：curl 就能调，corral、pony 这种同步程序用现有的 ureq 就能读；浏览器原生支持；断线重连可以从上次的序号接着收。
- **乙 自定，JSON-RPC over WebSocket（或 stdio），照 Codex app-server。**
  - JSON-RPC 是「请求带编号、回复带同一个编号、另有不要回复的通知」的一种 JSON 消息约定；WebSocket 是一条双向长连接。
  - 好处：一条连接、请求和回复天然配对，Codex 现成的协议可以照抄。
  - 坏处：curl 调不了；同步程序要另接 WebSocket；断线续传要自己设计。
- **丙（不推荐）直接用 ACP。** 理由见 §5.3。
- **推荐甲。** 不管选甲还是乙，协议类型都放在单独的 Rust 包里，再从它自动生成 JSON Schema 和 TypeScript 类型给网页用（Codex 就是这么做的，用 schemars、ts-rs，L1）。

**5. 桌面应用用什么做**（以后几期的事，现在可以先不定）

- **A（倾向）GPUI。**
  - 和 paddock 一样，Rust 写，能直接用协议包里的类型。
  - 可以有两种：paddock 里加一个原生的会话视图；或者单独做一个精简的 GPUI 应用来发布。
  - GPUI 还没到 1.0：paddock 用的是 `gpui-pre 0.3.8` 第三方快照（paddock DESIGN）。
- **B Tauri 一类：** 把网页客户端包成桌面应用。网页和桌面共用一份 TypeScript 界面，代价是和 paddock 两套界面技术。
- **推荐：** 等网页客户端的需求清楚了再定。如果先做桌面、要和 paddock 一致，选 A。

**6. 第一版的范围**（详见 §9）

- **推荐要的：**
  - 服务端：每台机器一个，常驻；客户端找不到就在后台起一个；只听本机 unix socket；几个终端客户端能同看一个会话。
  - 升级：先用「等在跑的轮跑完（有上限）再重启、从盘恢复」，版本不同不自动换（第 7 条）。
  - 终端客户端，不占全屏；服务端重启后自动重连、从断开处续上。
  - OpenAI Responses 接口，加 ChatGPT 套餐登录。
  - 工具：read、write、改文件（一种）、shell。工具都经「执行器」去动文件、跑命令，第一期执行器在服务端进程里（§6.4）。
  - 权限：全放行、每次问、本会话放行；Esc 打断。
  - 会话存盘、接着聊、手动压缩加超限自动压缩。
  - 读 `AGENTS.md`。
  - 带首句启动、免确认启动。
- **推荐不要的：** 提问框、技能、MCP、子 agent、网页搜索、图片、远程连接、网页客户端、桌面应用、ACP、多家模型、执行器子进程和套墙前缀（第二期）、排空式升级（第二期）、单机模式、自带的系统沙箱。

### 调研中发现、建议一并定的（任务书没列，我加的）

**7. 服务端怎么起、怎么升级**（详见 §5.4）

用户 10-23 已定：一台机器一个常驻服务端、多个客户端；架构不迁就 corral（「这个你要给他，我已经说了不需要和corral绑定死」）。剩下要定的是怎么起、怎么升级、留不留单机模式。

- **怎么起：**
  - **A（推荐）客户端自动起，另可装成系统服务。** 敲 `colt` 时先读登记文件找服务端，找不到就在后台起一个 `colt serve` 再连上。opencode 2.x 就这样（L2 `default.ts`）。想开机就在，再用一条命令装成 launchd、systemd 的用户服务（和 pony 一个路子）。
  - **B 只能装成系统服务，客户端不自动起。** 简单，但每台新机器第一次用都要多一步。
- **怎么升级（服务端换新版本时，在跑的会话怎么办）：**
  - **甲 排空交接（推荐，第二期派活前做好）。**
    - 怎么做：新版起来、接管登记文件；旧版不再接新的一轮，手上在跑的轮跑完（可以设上限）就把这些会话交出去、退出；新版从盘上接着加载这些会话，事件序号接着编。客户端发现连接断了，就读登记文件连新版，从最后的序号续订。
    - 好：在跑的命令不被打断。
    - 代价：升级时短暂有两个版本的服务端同时在跑（每个会话同一时刻只归一个，靠每个会话一把文件锁）；旧程序文件在旧服务端退出前不能删。
  - **乙 等一会儿再重启（Codex、opencode 现在的做法）。**
    - 查到：Codex 的 daemon 升级时，最多等在跑的轮 60 秒（可配 0–300 秒）再重启，文档明说「active or queued work may be interrupted」（L1 `app-server-daemon/README.md`）；opencode 发现版本不同就替换后台服务（L2 `default.ts`）。
    - 好：简单。坏：超时还没跑完的轮被打断，重启后记成「被打断」。
  - **丙（不推荐）进程内交接**：把监听的 socket 和在跑的命令一起交给新进程。理论上一轮都不断，但很复杂。
  - **推荐：** 第一期用乙，但不自动换：版本不同时，只要协议兼容就继续用旧服务端，只提示；要换由用户敲命令，或者等服务端空闲时自己换。第二期接 corral、开始派活之前做甲，因为派出去的 agent 常常一跑很久。
- **单机模式（备选，不作推荐）：** `colt --standalone`，服务端嵌在这个终端客户端的进程里，不连常驻的那个。Codex 的 Embedded、opencode 的 `--standalone` 就是这个（L1 `tui/src/lib.rs`、L2 `standalone.ts`）。
  - 同一份服务端代码，额外工作很少。
  - 适合临时 ssh 到别的机器、跑测试、不想留后台进程的场合。
  - 取舍见 §5.4；第一期不做，以后要再加。

**8. 新仓库开不开源**

- 查到（W1 Quickstart）：
  - 「ChatGPT plan usage is available to all open-source partners and selected private clients」：开源项目都能用，私有的只给选中的。
  - 文档是写给开源和在本机运行的 app 的；收费的或在远程托管的 app 要先填意向表。
- 不知道：只是公开仓库、不带开源许可证（ranch 现在就是这样，HANDOFF「仓库」节），算不算「open-source」。动态注册的流程看起来不检查这一点（W1 Registration 节没提），但条款怎么认定没查到原文。
- **推荐：** 新仓库公开，加一个常见的开源许可证（比如 MIT 或 Apache-2.0），用户定。许可证还决定别人能不能用我们的代码；从 Codex 搬来的代码要同时遵守 Apache-2.0（§3.1）。

**9. 接 corral 怎么做**（详见 §6）

原则：agent 不依赖 corral，corral 去适配 agent 的公开接口（命令行、协议、钩子）。agent 只做通用功能，不写 corral 的名字。

- **做法一（第二期先做）：pen 里开终端客户端，状态靠 agent 的通用钩子。**
  - agent 要有：会话名字、按会话的环境和钩子、客户端退出时关掉会话。
  - corral 加一个适配器，和现有六家一样。
  - 覆盖：开、看状态、回复、打断、attach、paddock 终端、`send`、`stop`。
  - 不覆盖：暂停和套墙。做法二、三做好之前，corral 对它的 `pause`、`--sandbox` 直接报「不支持」，不悄悄不生效。
- **做法二：暂停、继续、停止走 agent 的会话命令。** agent 加会话的暂停、继续、关闭；corral 的适配器加一项通用能力「这几样走 agent 的命令」。
- **做法三：套墙走 agent 的执行器前缀。**
  - agent 把每个会话动文件、跑命令的事放进一个执行器子进程，建会话时可以给执行器一个启动前缀。
  - corral 在 `--sandbox` 时把 `fence run … --` 当前缀传进去；fence 加一份执行器用的数据。
  - 备选：agent 自带系统沙箱，corral 只把 `--sandbox` 翻成 agent 的参数。
- **做法四：pen 直接订服务端的事件流。** 加 `corral approve`、`corral answer`，pony 的按钮按请求号批准。
- **做法五（不推荐第二期做）：corral 不开终端，直接当客户端。**
- **推荐：** 第二期按一、二、三、四的次序做，两边都要动的，agent、corral 各开一件（§9.2）。做法二到四都要动 corral 的共用代码，要用户同意。做出来的是通用能力，以后接别的常驻服务端式 agent（比如 opencode 2.x）也能用（推测）。

---

## 1. 核心理念和组成

### 1.1 术语（第一次出现的词都在这）

| 词 | 意思 |
| --- | --- |
| 模型 | OpenAI、开源模型这些。只会读一段文字、写一段文字；会「调用工具」，也只是它写出一段格式固定的「我要调用 X，参数是 Y」 |
| token | 模型计算长度的单位，大致一个英文词或半个汉字。模型一次能读的长度有上限（叫上下文窗口，常见几十万 token） |
| 上下文 | 每次发给模型的全部东西：系统提示词（「你是一个 coding agent，规矩是……」）、工具清单、到目前为止的对话和工具结果 |
| 工具调用 | 模型回复里的一段结构化请求，比如 `shell {"command":"cargo test"}`。agent 执行它，把结果作为「工具结果」放回上下文 |
| 一轮（turn） | 用户说一句话，到 agent 干完、模型不再要工具为止。一轮里可能调很多次模型、跑很多个工具 |
| 会话（session，Codex 叫 thread） | 同一段对话的所有轮。存盘后能接着聊 |
| 项目（item） | 一轮里的一个单位：用户的话、模型的一段文字、模型的一段思考摘要、一次命令、一次改文件、一次提问…… |
| 流式 | 模型边写边发，客户端边收边显示，不等写完 |
| 压缩 | 对话太长、快到上下文上限时，让模型把旧的部分写成一份摘要，用摘要替换旧对话，接着干 |
| 权限确认／批准 | 模型要做有风险的事（跑命令、改文件）前先停下问人，人选允许或拒绝 |
| 打断 | 人按 Esc：停掉正在调用的模型和正在跑的命令，这一轮以「被打断」结束 |
| 技能 | 一个目录里放一份 `SKILL.md` 说明书。agent 把技能的名字和一句话简介放进系统提示词，需要时再读全文。corral、corral-dispatch 就是技能 |
| MCP | 一种让 agent 接外部工具服务器的标准协议（fence 给各家接的 Exa 查资料就是 MCP） |
| 子 agent | agent 自己再开一个 agent 去干一部分活，干完把结果交回来 |
| 钩子 | agent 在某些时刻（开始一轮、调工具前后、一轮结束）运行一条用户配置的命令。corral 现在主要靠各家的钩子知道状态 |
| 事件流 | 服务端把发生的事按顺序一条条发给客户端，每条带一个递增序号 |
| 常驻服务端（daemon） | 在后台一直跑的程序，不随哪个终端关掉而退出。这里指 agent 的服务端：每台机器一个，所有会话都在它里面 |
| 执行器 | 真正动文件、跑命令的那一层。工具决定做什么，执行器去做。每个会话一个；拆成单独的子进程后，可以单独冻住、单独套墙（§6.4） |
| OAuth | 「用某某账号登录」的标准做法。浏览器里登录、授权，app 拿到一个令牌；以后带着令牌调接口 |

### 1.2 由哪几块组成

```text
┌────────────────── 客户端：只显示事件、只转达人的输入和决定 ──────────────────┐
│  终端客户端(TUI)   corral 的 pen   pony   网页   桌面应用   (以后) ACP 转接   │
└───────────────┬───────────────────────────────────▲───────────────────────┘
       命令：送一句话、打断、回答批准框、建会话…        事件：按序号推送，断了能补
                │                                   │
┌───────────────▼───────────────────────────────────┴───────────────────────┐
│ 服务端（agent 的核心；每台机器一个，常驻）                                       │
│                                                                            │
│  连接与认证 ──→ 会话管理 ──→ 事件总线（编号、留最近一段、分发给每个订阅者）       │
│                    │                    ▲                                  │
│                    ▼                    │ 每一步都变成事件                    │
│            ┌──────────────┐             │                                  │
│            │ 一轮的循环    │─────────────┘                                  │
│            └─┬────┬────┬──┘                                                │
│              │    │    └──→ 模型接入层 ──→ OpenAI Responses / 兼容接口 / 本机  │
│              │    └──→ 上下文组装：系统提示词、AGENTS.md、技能清单、压缩        │
│              ▼                                                             │
│         权限闸门（这个工具这次要不要先问人）                                     │
│              ▼                                                             │
│         工具：read  write  改文件  shell  (以后：搜索 计划 提问 MCP 子 agent)   │
│              │ 真正动文件、跑命令的事交给这个会话的执行器                          │
│  会话存盘：每个会话一个只追加的文件，重开时照它重建                                │
└──────────────┼─────────────────────────────────────────────────────────────┘
               ▼
   每个会话一个执行器（第一期在服务端进程里；第二期拆成子进程，可以加启动前缀套墙）
   会话 A 的执行器 ──→ A 跑的命令        会话 B 的执行器（套了墙）──→ B 跑的命令
```

每块管什么：

- **连接与认证**：管谁能连。本机 socket 靠文件权限（目录 0700），远程靠令牌。
- **会话管理**：建会话、列会话、从存盘恢复、关会话。记每个会话当前的状态：空闲、在干活、等批准、等回答、出错。
- **事件总线**：循环里发生的每件事都变成一条事件，编上号，发给订阅了这个会话的所有客户端；留最近一段，晚连上或断线重连的客户端从自己收到的最后一个序号往后补。
- **一轮的循环**：见 1.3。整个 agent 最核心的一段代码；pi 的这一段约 1,600 行 TypeScript（L3 `packages/agent/src/agent-loop.ts` 959 行 + `agent.ts` 623 行）。
- **模型接入层**：把我们自己的消息格式翻成某家接口的请求，把它流式回来的东西翻回我们的格式（文字增量、工具调用、思考摘要、用量、错误）；管重试和登录令牌刷新。
- **上下文组装**：每次调模型前拼出要发的全部东西；太长了就压缩。
- **权限闸门**：按规矩（工具的风险档、用户选的模式、本会话已经允许过的）决定直接执行、先问人还是直接拒绝。要问人时，生成一个「待处理请求」，发给所有客户端，等任意一个客户端回答。
- **工具**：决定做什么、检查参数、把结果整理给模型。每个工具都要能被打断。
- **执行器**：替工具真正动文件、跑命令。每个会话一个。Codex 也是这么分的：它有一个单独的 exec-server，管起进程（`process/start`、`process/signal`……）和读写文件（`fs/readFile`、`fs/writeFile`……），工具可以交给本机的或远处的执行器去做（L1 `exec-server/README.md`、`exec-server-protocol/src`）。拆成子进程后，暂停、套墙都可以只针对一个会话（§6.4）。
- **会话存盘**：每发生一件事就往会话文件末尾追加一行，崩了也不丢已经发生的。

### 1.3 一次对话怎么流：从用户回车到这一轮结束

```text
 人       终端客户端        服务端·循环          权限闸门       模型(OpenAI)        工具(shell)
 │ 打字回车  │                 │                   │              │                   │
 │─────────→│ 送一句话         │                   │              │                   │
 │          │────────────────→│ 存盘；事件「一轮开始」  │              │                   │
 │          │←────────────────│ （所有客户端都收到）    │              │                   │
 │          │                 │ 拼上下文，发请求 ──────────────────→│                   │
 │          │ 事件「文字增量」…  │←──────── 流式：先一段文字，再一个工具调用 ─│                   │
 │  看到字   │←────────────────│                   │              │                   │
 │          │                 │ 模型要跑 cargo test │              │                   │
 │          │                 │──────────────────→│ 规矩：要先问人  │                   │
 │          │ 事件「待处理请求」  │←──────────────────│              │                   │
 │  弹框     │←────────────────│ （pony 也收到同一条） │              │                   │
 │ 按「允许」 │ 回答请求 #7       │                   │              │                   │
 │─────────→│────────────────→│ 事件「请求 #7 已处理：允许，来自终端」，所有客户端关框     │
 │          │                 │──────────────────────────────────────────────────────→│ 跑
 │          │ 事件「命令输出增量」│←──────────────────────────────────────────────────────│ 输出
 │          │←────────────────│ 事件「命令完成，退出码 0」                                 │
 │          │                 │ 把输出放进上下文，再发请求 ─────────→│                   │
 │          │ 事件「文字增量」…  │←──────── 最后一段文字，不再要工具 ────│                   │
 │          │ 事件「一轮结束：完成」│ 存盘                               │                   │
 │          │←────────────────│                   │              │                   │
```

一步步说：

1. 人在终端客户端里打字回车，客户端把这句话发给服务端，自己什么都不判断。
2. 服务端先存盘，再发事件「一轮开始」。corral 看到这一条，状态就是 working；不用钩子。
3. 循环拼好上下文调模型。模型流式回来的文字，变成一条条「文字增量」事件，客户端边收边显示。
4. 模型写出一个工具调用。循环先问权限闸门。
5. 要问人：服务端生成「待处理请求 #7」，里面有工具名、完整参数（比如完整命令）、可选的回答，作为事件发给所有客户端。终端弹框；pony 在 Slack 发带按钮的提醒；corral 把状态记成 blocked。
6. 不管谁先回答，服务端只认第一个，再发「#7 已处理」。别的客户端看到就关掉自己的框或按钮。Codex 就是这样：批准请求发给订阅这个会话的所有连接，处理完再广播 `serverRequest/resolved`（L1 `app-server/src/outgoing_message.rs`、`request_processors/thread_lifecycle.rs`）。
7. 工具执行，输出也作为增量事件流出去，结束时带退出码。
8. 结果放回上下文，再调模型。如此反复，直到模型只回文字、不再要工具。
9. 发「一轮结束：完成」，带用量。corral 看到就是 idle，最后那段文字就是 `reply`。

**按 Esc 打断时：**

1. 客户端发「打断」。
2. 服务端掐断正在进行的模型请求，杀掉正在跑的命令的整个进程组，把还没处理的请求都标成「已取消」。
3. 往会话里记一条「被打断」，让模型下次知道。
4. 发「一轮结束：被打断」。

corral 拿到的 `interrupted` 是服务端直接说的，不用像 R24 那样去对话记录里找标记。

### 1.4 服务端和客户端之间传什么

两类东西：客户端发给服务端的「命令」，服务端发给客户端的「事件」。完整清单和格式建议在 §5.5。最要紧的是：

- 事件分三层：会话的状态变了；一轮开始、结束；一轮里每个项目的开始、增量、完成。
- 批准和提问是「待处理请求」：有编号、有完整内容、有可选回答；处理完广播一次。
- 每条事件有序号。客户端随时可以「给我这个会话现在的完整样子，加上最后的序号」，再从这个序号往后订。

---

## 2. 一个 coding agent 至少要有哪些部分（第 1 条）

### 2.1 总表

| 部分 | 管什么 | 四家怎么做（查到，L1–L4） | 第一版 |
| --- | --- | --- | --- |
| 一轮的循环 | 调模型 → 执行工具 → 再调，直到不要工具 | 都有。pi 的最小：一轮里还处理「插话」（干活时人又打的话）和「后续」（干完再处理的话） | 必须 |
| 模型接入 | 流式、工具调用格式、重试、用量、登录 | Codex 只接 OpenAI Responses；opencode、pi、omp 各自写了多家通用的库（opencode `packages/ai` 约 7 万行，pi `packages/ai` 约 6.5 万行） | 必须，只接一家 |
| 上下文组装 | 系统提示词、环境信息（目录、系统、时间）、项目说明（`AGENTS.md`）、技能清单 | 都有；都读 `AGENTS.md` | 必须（技能清单后补） |
| 读文件 | 带行号读、限制长度、分段读 | pi、opencode、omp 有 read 工具；Codex 没有，让模型用 shell 里的 `cat`、`sed`、`rg` | 必须（有专门工具，模型更省 token、更少出错——推测） |
| 写、改文件 | 新建、整文件重写、局部改 | 见 2.2 | 必须 |
| shell | 跑命令：超时、输出太长时截断、打断时杀整个进程组 | 都有。Codex 的 unified_exec 还能开带终端的长命令会话；omp 把 bash 做进进程里（brush） | 必须（简单版） |
| 搜索 | 按内容找（grep）、按名字找（glob） | pi、opencode、omp 有；Codex 让模型用 `rg` | 可后补，第一版靠 shell 里的 `rg` |
| 权限确认 | 什么要问人、怎么记住「允许过」 | 见 2.4 | 必须（简单版） |
| 打断 | Esc 掐断模型和命令 | 都有。Codex `turn/interrupt` 后这一轮状态是 `interrupted`（W6） | 必须 |
| 会话存盘、接着聊 | 存每一步，重开时重建上下文 | 见 2.3 | 必须 |
| 压缩 | 快到上限时把旧对话写成摘要 | 见 2.5 | 必须（手动加超限自动的简单版） |
| 提问框 | 模型给几个选项问人 | Codex `item/tool/requestUserInput`；opencode 的 form；omp 的 ask 工具 | 第二期（pony、corral 用得上） |
| 计划、待办 | 模型列出步骤并更新进度 | Codex update_plan；opencode、omp 的 todo | 可后补 |
| 技能 | 读 `SKILL.md` | 都支持；Codex、pi、omp 都读 `~/.agents/skills` | 第二期（接 corral 的派活需要） |
| MCP | 接外部工具服务器 | Codex 用官方 Rust SDK `rmcp`；opencode、omp 都有；pi 有 mcp 包 | 第二期以后（fence 查资料用得上） |
| 子 agent | 开小 agent 分活 | Codex 的 spawn_agent、send_input、wait_agent 等工具；opencode 的 subagent；omp 的 task（每个在单独 worktree，交回有格式的结果）；pi 没有内置（L1 `core/src/tools/handlers/multi_agents*`、L2、L4） | 以后 |
| 钩子 | 在某些时刻运行用户的命令 | Codex、Claude Code 有；pi、omp 是扩展（TypeScript 插件） | 第二期（做法一接 corral 要用，见 §6） |
| 网页搜索、读网页 | | Codex 用 OpenAI 托管的搜索；omp 自己串了二十多家；opencode 有 webfetch、websearch | 以后（或经 MCP） |
| 图片输入 | 截图给模型看 | 都有 | 以后 |

### 2.2 改文件的四种办法

| 办法 | 怎么做 | 谁在用 | 好处 | 坏处 |
| --- | --- | --- | --- | --- |
| 整文件写 | 模型写出整个文件的新内容 | 都有（write） | 最简单，不会「找不到」 | 大文件费 token；容易覆盖掉别的改动 |
| 按文字替换 | 模型给出「旧的一段」和「新的一段」，旧的那段必须在文件里只出现一次；一次可以给好几处 | pi、opencode（edit）；pi 的说明见 L3 `src/core/tools/edit.ts` | 所有模型都会用；规则简单 | 旧的那段抄错一个空格就失败，模型会反复重试 |
| 补丁（apply_patch） | Codex 自定的补丁格式：`*** Begin Patch`、`*** Update File: 路径`、`@@` 定位、`+`/`-` 行；一次可以改、加、删、改名多个文件 | Codex（L1 `apply-patch/src/parser.rs` 有完整语法）；omp 可选 | OpenAI 的 GPT 系列专门按这个格式训练过（Codex 把它做成带语法约束的「自由格式工具」，L1 `core/src/tools/handlers/apply_patch_spec.rs`）；Codex 的实现能直接搬（Apache-2.0，约 4,800 行） | 别家模型不一定熟 |
| 按行号加哈希定位（hashline） | 读文件时每行带一个短标签；模型改的时候引用标签，不用重抄原文；文件变了标签就对不上，直接拒绝 | omp 默认（L4 `docs/tools/edit.md`） | omp 自己的测试说对弱一些的模型提升很大（README：某模型 6.7% → 68.3%，没验证） | 最复杂；omp 还要按模型挑格式，有几家模型会自动退回按文字替换 |

建议（推测）：

- 第一期主力是 OpenAI 模型，用「整文件写 + apply_patch」，apply_patch 直接搬 Codex 的实现。
- 接开源模型时再加「按文字替换」。
- 按模型选用哪种：omp 有现成的做法，按模型名匹配。

### 2.3 会话存盘

- 查到：
  - Codex：每个会话一个只追加的 JSONL 文件，在 `~/.codex/sessions/年/月/日/rollout-时间-<id>.jsonl`（L1 `rollout/src/list.rs`），另有一个状态数据库供列表和搜索。
  - pi：JSONL 文件，每条记录指向父记录，构成一棵树，能从中间分叉（L3 `docs/how-pi-works.md`）。
  - opencode：用 SQLite（L2 `packages/core/src/database`）。
- 建议（推测）：
  - 每个会话一个只追加的 JSONL 文件，每行一条事件，就用事件本身，存的和发给客户端的是一样的东西。
  - 重开时按顺序重放出上下文。
  - 和 corral 的事件文件一个路子，好查、好修、崩了不丢。
  - 列表慢了再加索引。

### 2.4 权限

- 查到：
  - Codex：两层。一层是什么时候问人（不信任的项目一律问、模型自己决定、按类别细调、从不问）；另一层是操作系统级沙箱，每条命令都在里面跑：只读、只许写工作区、不限（macOS 用 Seatbelt，Linux 用 Landlock 或 bubblewrap）（L1 `protocol/src/protocol.rs`、`config_types.rs`、`core/README.md`）。批准的回答有：允许、本会话都允许、允许并记成规则、拒绝、拒绝并停下这一轮（L1 `app-server-protocol/src/protocol/v2/item.rs`）。
  - omp：每个工具声明一个风险档：读、写、执行；四种模式：读以外都问、问且记住、只问执行、全放行（默认全放行）（L4 `docs/approval-mode.md`）。
  - pi：根本不问，文档说安全要靠容器或虚拟机（L3 `docs/security.md`）。
  - opencode：权限是独立的资源，有「问」「答」「存下来的允许」（L2 审计表 Group 7）。
- 建议（推测）：
  - 第一期照 omp 的三档、四模式里的三个：每次问、只问执行、全放行。
  - 回答四个：允许这一次、本会话都允许、拒绝、拒绝并停下。
  - 系统级沙箱第一期不自己做。第二期把执行器拆成子进程后，可以由外面给它加启动前缀套墙（比如 fence，§6.4、§7.2）；要不要像 Codex 那样自带沙箱，以后再看。
  - 「免确认启动」就是全放行模式。

### 2.5 压缩

- 查到：
  - Codex：让模型写一份「交接摘要」（进度和决定、重要的约束、还剩什么、需要的数据），新的上下文是：开头的固定内容 + 最近的用户原话（最多约 2 万 token）+ 摘要（L1 `prompts/templates/compact/prompt.md`、`core/src/compact.rs`）。
  - pi：超过「窗口减去预留 16,384 token」就触发；往回保留最近约 2 万 token，更早的写成结构化摘要；原始记录留在会话文件里不删，只是不再发给模型（L3 `docs/compaction.md`）。
- 建议：
  - 第一期照 pi 的触发条件、Codex 的提示词，各一两百行（推测）。
  - 摘要作为一条事件存盘、发给客户端，人看得到「这里压缩过」。

---

## 3. 能借鉴的开源实现（第 2 条）

### 3.1 Codex CLI（重点）

- **读的版本：** `rust-v0.163.0`（提交 `83086188`，2026-10-15），和本机装的 0.163.0 一致。
- **许可证：** Apache-2.0，带 NOTICE（L1）。GitHub 约 13 万星（W19）。
- **规模：** 不含 vendor，Rust 代码约 187 万行（含测试，scc 统计），约 130 个包（crate）。其中：

| 包 | 代码行 | 是什么 |
| --- | --- | --- |
| `core` | 42.1 万 | 循环、工具、压缩、权限、会话等全部业务 |
| `tui` | 41.2 万 | 终端界面 |
| `app-server` | 18.2 万 | 服务端 |
| `app-server-protocol` | 3.2 万 | 协议类型 |
| `protocol` | 2.8 万 | 核心的内部协议 |
| `app-server-transport` | 1.7 万 | stdio、unix socket、WebSocket、远程控制 |
| `apply-patch` | 4,776 | 补丁格式 |
| `codex-api` | 1.6 万 | 调 OpenAI |
| `rmcp-client`、`codex-mcp` | 6 万 | MCP |

**服务端的分法（app-server）**（L1、W6）：

- JSON-RPC 2.0，线上省掉 `"jsonrpc"` 头；三种消息：请求（有 `id`）、回复（同一个 `id`，带 `result` 或 `error`）、通知（没有 `id`）。
- 传输：
  - 默认 stdio，一行一个 JSON。
  - `unix://`：本机 socket 上跑 WebSocket。
  - `ws://IP:端口`：官方标成「experimental and unsupported」。
  - 非回环地址必须配认证：一个令牌文件（或它的 SHA-256），或者签名的 JWT（L1 `websocket-auth/src/lib.rs`）。
- 三个核心概念：
  - **Thread**：会话，包含很多 turn。
  - **Turn**：一句话加 agent 为它干的所有活，包含很多 item。
  - **Item**：一个单位，有十几种：用户的话、模型的话、思考摘要、计划、命令执行、改文件、MCP 调用、子 agent 活动、看图、压缩……（L1 `app-server-protocol/src/protocol/v2/item.rs` 的 `ThreadItem`）。
- 一次的流程：
  1. 连上先 `initialize`。
  2. `thread/start`、`thread/resume` 或 `thread/fork`。
  3. `turn/start` 回复里这一轮是 `inProgress`。
  4. 之后流 `item/started` → `item/agentMessage/delta` 等增量 → `item/completed`。
  5. 最后 `turn/completed`，状态是完成、被打断或失败。
  - 干活时插话用 `turn/steer`，打断用 `turn/interrupt`。
- 会话状态：`notLoaded`、`idle`、`systemError`、`active`。`active` 带标记：等批准、等回答（L1 `thread.rs` 的 `ThreadStatus`、`ThreadActiveFlag`）。**这几乎就是 corral 要的 working／blocked／idle。**
- 批准和提问：服务端反过来发请求（`item/commandExecution/requestApproval`、`item/fileChange/requestApproval`、`item/tool/requestUserInput` 等），请求里有完整命令、目录、原因。发给订阅这个会话的所有连接；处理完广播 `serverRequest/resolved`（L1 源码）。
- 多客户端：`thread/start` 的那个连接自动订阅；别的连接 `thread/resume` 也能订；最后一个订阅者走后，会话再留 30 分钟才卸载（W6）。
- 终端界面连服务端有三种目标：进程内嵌入、本机 daemon（unix socket）、远程；默认先找 daemon，连不上就自己嵌一个（L1 `tui/src/lib.rs` 的 `AppServerTarget`、`app-server-daemon/README.md`）。
- 常驻 daemon（L1 `app-server-daemon/README.md`，官方标成实验性）：
  - 命令 `codex app-server daemon start|restart|update|stop|version…`，每条只输出一个 JSON 给程序读。
  - 用 pid 文件和文件锁保证只有一个。
  - 升级时最多等在跑的轮 60 秒（可配 0–300 秒）再重启，「active or queued work may be interrupted」。
  - daemon 用的是它启动时继承的环境变量，原话「per-client environment isolation is not provided」：在新终端里改了环境变量，daemon 跑的命令看不到。
- 执行器单独一个服务 `exec-server`（JSON-RPC）：起进程、给进程发信号、读写文件、发 HTTP 请求都在这里做，可以在本机也可以在远处；app-server 的工具把这些事交给它（L1 `exec-server/README.md`、`exec-server-protocol/src`）。`exec-server`、`exec-server-protocol`、`app-server-daemon` 三个包合计约 7.4 万行。
- 给其他语言的客户端：协议类型用 `schemars` 生成 JSON Schema、用 `ts-rs` 生成 TypeScript（L1 `Cargo.toml`）。

**能直接搬的（查到许可证，推测可行性）：**

- `apply-patch`：补丁格式的解析和应用，约 4,800 行，依赖少，最值得搬。
- `prompts/templates/compact/` 等提示词文本。
- 少数叶子小包（文件搜索 `file-search` 约 1,300 行、`execpolicy` 约 2,700 行）。
- 搬的条件（Apache-2.0 第 4 条）：附许可证全文、保留原有版权和 NOTICE 内容、改过的文件标明改过。
- 和 ranch「迁入的代码开头注明来源」的规矩一致。

**搬不了的：** `core`、`tui`、`app-server` 互相依赖很深（`codex-utils-*`、`protocol`、`login`、`config`……），一抽就连带几十个包。只能读懂了照着写。

**其他值得学的：**

- 终端界面不占全屏：说完的历史直接写进终端自己的滚动区，只有底部输入区由程序重画；少数界面（回看历史）临时切全屏（L1 `tui/src/insert_history.rs`、`app_backtrack.rs`）。
- 依赖：ratatui 0.30.2、crossterm 0.29（用了 OpenAI 自己的分支）、axum 0.8、tokio-tungstenite（分支）、reqwest 0.12、eventsource-stream 0.2.3、rmcp 3.5.1（L1 `codex-rs/Cargo.toml`）。

### 3.2 opencode

- **读的版本：** `v2.1.3`（`bb25a2f`，2026-10-22），和本机装的 opencode-v2 2.1.3 一致。
- **许可证：** MIT。GitHub 约 22 万星（W19）。
- **规模：** 整个仓库 TypeScript 约 79 万行（含网站、控制台等）。`core` 16.1 万、`tui` 10.3 万（opentui + Solid）、`server` 8,800、`ai` 7.1 万、`cli` 2.3 万、`client` 2.1 万、`protocol` 4,400。
- **服务端的分法：**
  - HTTP 资源接口，审计过的有 139 个（L2 `V2_HTTP_API_AUDIT.md`）：会话的建、列、取、删；送一句话（`POST /api/session/{id}/prompt`）；打断（`.../interrupt`）；压缩（`.../compact`）；回退；排队的话（inbox）；权限（`.../permission/{id}/reply`）；表单，也就是提问（`.../form/{id}/reply`）……
  - 所有事件走一条 SSE：`GET /api/event`。事件名如 `session.execution.started`、`session.text.delta`、`session.tool.called`、`permission.asked`、`permission.replied`、`session.execution.interrupted`（L2 `packages/schema`、`core`）。
  - 每个订阅者有 4,096 条的缓冲，满了就断开它（L2 `server/src/event-feed.ts`）。
- **服务端怎么起**（L2 `cli/src/commands/handlers/default.ts`、`client/src/effect/service.ts`、`cli/src/services/standalone.ts`）：
  - 直接敲 `opencode`：先找每台机器一个的后台服务。没有就起一个，提示「Starting background server...」；版本对不上就替换，提示「Restarting background server (version mismatch)...」。
  - 后台服务把自己的地址、pid、版本、密码写进一个登记文件（权限 0600）。客户端读这个文件就能连，「That file is the complete discovery contract」。
  - `--standalone`：起一个私有的子进程服务端（`serve --stdio --port 0`），子进程把标准输入当「租约」，客户端一退、管道一断它就退。
  - `--server`：连指定的远程服务端。
- **认证**（L2 `server/src/auth.ts`、`pairing.ts`）：
  - HTTP basic auth，用户名固定 `opencode`，密码随机生成。
  - 「配对码」5 分钟有效、用一次就作废；换来一个 30 天的会话令牌（存成 cookie，cookie 名带端口，免得同一台机器的几个服务端互相串）。
  - 改密码就让所有会话令牌失效。
- **值得学的：**
  - 「后台服务 + 登记文件 + 版本不同就换」这整套。
  - 权限和提问做成资源、靠事件通知、任何客户端都能回答。
  - 配对码给手机浏览器用。
  - `POST` 和 SSE 分开、curl 就能调。
- **能直接用的：** 代码是 TypeScript，不能直接用；设计、提示词按 MIT 可以参考和引用。

### 3.3 pi

- **读的版本：** `v1.4.0`（earendil-works/pi，`4a080252`，2026-10-22），和本机装的 `@earendil-works/pi-coding-agent` 1.4.0 一致。
- **许可证：** MIT。GitHub 约 11.7 万星（W19）。
- **规模：** `agent`（核心循环）的 `src` 约 2,500 行，其中循环 1,600 行；`ai` 6.5 万；`coding-agent` 13.5 万；`tui` 3.2 万（自己写的终端界面库）。
- **值得学的：**
  - **最小的核心循环。** 外层循环处理「后续」的话；内层每次调模型前先收「插话」；输出被长度上限截断时，那批工具调用一律当失败，不执行可能被截坏的参数（L3 `agent-loop.ts` 的注释）。代码短、结构清楚，最适合照着理解「一轮」。
  - **七个工具：** read、write、edit（按文字替换，一次可以多处）、bash、find、grep、ls（L3 `src/core/tools/`）。
  - **会话是一棵树：** JSONL，能从任意一条分叉；压缩只是加一条摘要记录，原始记录不删（L3 `docs/how-pi-works.md`）。
  - **RPC 模式：** `pi --mode rpc`，标准输入进命令、标准输出出回复和事件，一行一个 JSON。文档专门提醒：不能用把 U+2028、U+2029 当换行的读行函数（L3 `docs/rpc.md`）。
  - **不做权限：** 理念是安全靠隔离，不靠弹框（L3 `docs/security.md`）。
- **能直接用的：** TypeScript，不能直接用；按 MIT 可以参考。

### 3.4 omp（oh-my-pi）

- **读的版本：** `v18.12.6`（`45c35e4`，2026-10-21），和本机 Homebrew 装的 18.12.6 一致。
- **许可证：** MIT，pi 的分支。GitHub 约 3.6 万星（W19）。
- **规模：** `coding-agent` 约 90 万行 TypeScript（含生成文件和测试），`ai` 26 万、`tui` 22 万；全仓 TypeScript 约 167 万行、Rust 约 35 万行；Rust 里大头是 `pi-natives` 9.1 万、`pi-builtins` 9.1 万、`pi-shell` 3.4 万、`pi-edit` 2.9 万（scc 统计，含测试和生成文件）。README 说「31 个内置工具」「60 多家模型服务」。
- **值得学的（都是点子，不是第一期的事）：**
  - hashline 改文件，按模型挑改文件的格式（L4 `docs/tools/edit.md`）。
  - 工具风险档和权限模式，还有「问且记住」，能记到文件夹（L4 `docs/approval-mode.md`）。
  - 子 agent 各在一个 worktree 里干，交回有格式校验的结果；Alt+A 看所有子 agent 的实时对话（README）。
  - `/collab`：把正在跑的会话放到一个中转服务器上，给一个链接，别人在终端或浏览器里看，可以只读也可以一起操作；内容在客户端加密，中转看不到（L4 `packages/wire/README.md`、`packages/collab-web/README.md`）。这是「手机上看同一个会话」的另一种做法：不开端口，走中转。
  - ACP 模式，能在 Zed 里用。
- **能直接用的：** Rust 部分是给 TypeScript 用的原生扩展（N-API），接口不是给别的 Rust 程序用的；能参考 `pi-edit` 的 hashline 实现（MIT）。

### 3.5 顺带发现：rig（没细读）

- 查到（W16）：`0xPlaygrounds/rig`（MIT，约 9,000 星）原本是 Rust 的 LLM 应用库，现在 README 说它还带了一个 Rust 写的终端 coding agent（`cargo install rig`）。
- 它能用 ChatGPT 套餐登录；工具、命令、会话、登录、终端界面都是插件（基于 Bevy）；有 `rig -p --json` 的管道协议。
- 不知道：它的代码质量和架构。本轮没克隆、没细读。可以作为第二个 Rust 参照；主控 10-23 定先不细读。

### 3.6 对照总表

| | Codex | opencode | pi | omp |
| --- | --- | --- | --- | --- |
| 语言 | Rust | TypeScript（Bun） | TypeScript | TypeScript + Rust 原生 |
| 许可证 | Apache-2.0 | MIT | MIT | MIT |
| 规模 | 约 187 万行 | 约 79 万行 | 核心循环 1,600 行，全部约 35 万行 | 约 200 万行 |
| 服务端／客户端 | 有：app-server，JSON-RPC，stdio／unix socket／WebSocket／远程 | 有：HTTP + SSE，每台机器一个后台服务 | 没有服务端；RPC 是子进程一对一 | 没有服务端；collab 走加密中转 |
| 模型 | 只有 OpenAI Responses | 多家（自写库） | 多家（自写库） | 60 多家 |
| 改文件 | apply_patch | edit、write、patch | edit（按文字替换） | hashline（默认），可换 |
| 权限 | 问人策略 + 系统沙箱 | 权限资源 | 不问 | 三档 + 四模式 |
| 会话存盘 | JSONL + 状态库 | SQLite | JSONL 树 | JSONL 树 |
| 最值得学 | 协议、多客户端批准、服务端三种起法、apply_patch | 传输、后台服务、配对认证 | 核心循环、压缩、简洁 | 改文件、子 agent、共享会话 |

---

## 4. 模型怎么接（第 3 条）

### 4.1 有哪几种接口

| 接口 | 谁提供 | 说明 |
| --- | --- | --- |
| OpenAI Responses（`POST /v1/responses`） | OpenAI；Ollama、vLLM、llama.cpp 也兼容（W11–W13） | OpenAI 现在主推的接口。返回的是一串「项目」：文字、思考、函数调用；流式事件如 `response.output_text.delta`、`response.completed`。Codex 只用这个。ChatGPT 套餐只能走这个（W2） |
| OpenAI Chat Completions（`POST /v1/chat/completions`） | 几乎所有「OpenAI 兼容」的服务：DeepSeek、Kimi、GLM、OpenRouter、Ollama、vLLM、llama.cpp…… | 老接口，事实上的通用标准；工具调用格式各家略有出入 |
| Anthropic Messages（`POST /v1/messages`） | Anthropic；vLLM、llama.cpp 也兼容（W12、W13） | 按用户的话，Claude 订阅不接；按量的 API key 以后要不要接另说 |

结论：只写一个 Responses 客户端，就能接 OpenAI（API key 或 ChatGPT 套餐）和本机三种开源模型服务。再写一个 Chat Completions 客户端，基本就能接所有别家。

### 4.2 Rust 有没有现成的库

- 查到（W15，2026-10-23 的 GitHub 组织仓库列表）：
  - OpenAI 官方 SDK 有 Python、Node、Go、Java、Ruby，没有 Rust。
  - Anthropic 官方 SDK 有 Python、TypeScript、Go、C#、Java、PHP、Ruby，没有 Rust。
  - `openai/openai-rust`、`anthropics/anthropic-sdk-rust` 都是 404。
- 社区的库（W14，crates.io，「近 90 天」是 crates.io 的近期下载数）：

| 库 | 最新版 | 总下载 | 近 90 天 | 最近更新 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `async-openai` | 0.42.2 | 973 万 | 307 万 | 2026-10-10 | 非官方，按 OpenAI 的 OpenAPI 生成；Responses（含流式、WebSocket 事件）和 Chat Completions 都有；可以换自定义类型、改路径和请求头，能接兼容服务（W17） |
| `genai` | 0.7.0 | 52 万 | 24 万 | 2026-10-20 | 多家「原生协议」：openai、openai_resp、anthropic、gemini、ollama 等三十多家，还有自定义的兼容地址；1.0 以前，0.7 是「大版本、小改接口」（W17） |
| `rig-core` | 0.45.0 | 375 万 | 195 万 | 2026-10-23 | 一整套 LLM 应用框架（agent、工具、向量库）；抽象层厚 |
| `openai-api-rs` | 10.0.2 | 79 万 | 12 万 | 2026-10-03 | 非官方 OpenAI |
| `llm` | 1.3.8 | 13 万 | 2 万 | 2026-04-19 | 多家；半年没更新 |
| `misanthropic` | 0.5.1 | 2 万 | 0.6 万 | 2026-10-18 | 小，Anthropic |
| `anthropic` | 0.0.8 | 3 万 | 0.5 万 | 2024-09-03 | 两年没更新 |
| `clust` | 0.9.0 | 1.5 万 | 415 | 2024-06-30 | 两年没更新 |
| `anthropic-sdk-rust` | 0.1.1 | 1.4 万 | 0.4 万 | 2025-06-11 | 一年多没更新 |

- 底下要用的（W14）：
  - `reqwest` 0.13.5（异步 HTTP）。
  - `eventsource-stream` 0.2.3（解析 SSE；2022 年后没发过新版，但下载量很大，Codex 在用）。
  - `reqwest-eventsource` 0.6.0。

### 4.3 自己写还是用库

- **A（推荐）自己写一层薄的，只写用得到的。**
  - 推测工作量：
    - Responses 流式客户端（请求类型、SSE 事件到我们的项目的映射、工具调用拼接、思考摘要、用量、错误和重试）约 800–1,200 行。
    - 以后的 Chat Completions 约 500–800 行。
    - ChatGPT 套餐登录约 500–800 行（见 4.4）。
    - Codex 的 `codex-api` 约 1.6 万行，做的事多得多。
  - 理由：
    - ChatGPT 套餐这条路有一堆专门的规矩：必须 `store:false`、`stream:true`；不许 system 角色；不收一串参数；有专门的错误码，比如 `subscription_sharing_usage_limit_exceeded`，可能在流到一半时才来（W1、W2）。通用库不会替我们处理这些。
    - 打断要能随时掐断流，自己写更好控制。
    - 上面几个库都还没到 1.0，接口还在变。
    - 测试要用假的服务端回放 SSE，自己的类型更好造（ranch 的规矩：测试不连真服务）。
- **B 用 `async-openai` 的类型，外面包一层自己的接口。** 省掉写请求、回复类型的功夫，它对 Responses 覆盖全、更新勤。代价是跟着它升级，而且它不认套餐这条路的专门规矩。
- **C 用 `genai` 或 `rig`。** 多家现成，但抽象是它们的，和我们「会话 → 一轮 → 项目」的结构隔一层；pre-1.0。
- 推荐 A。觉得写类型麻烦，可以先 B，接口包在我们自己的 trait 后面，以后换掉不影响别处。

### 4.4 登录和计费：订阅能不能给自己做的 agent 用

- **ChatGPT（查到，W1–W3、W4、W5）：**
  - **「Sign in with ChatGPT」**：OpenAI 的官方预览，里面有一项可选能力「ChatGPT plan usage」，app 可以请求「用这个用户的 ChatGPT 套餐去调 Responses 接口」。
    - 文档原话：「Eligible ChatGPT Plus and Pro users can use their ChatGPT plan for AI requests in your product」。
    - 开源 app 「without requiring them to provide an API key」。
    - 「ChatGPT plan usage is available to all open-source partners and selected private clients」。
    - 文档写给「open-source and locally hosted apps」；收费的或远程托管的 app 要先填意向表。
  - 上线时间：二手资料说是 2026-09-29 的 DevDay 宣布的（W5，机器翻译的草稿，没核对）。OpenClaw 已经在 2026-09-24 合并了这种登录（W4）。
  - **流程**（W1 Registration、Models 节）：
    1. 浏览器 OAuth，用 PKCE（一种不需要 app 密钥的 OAuth 做法），回调到本机 127.0.0.1。
    2. 第一次登录用占位的 `client_id=dynamic_agent_client`，带一个固定的「本机标识」（`ext_agent_host_id`）和 app 名字；OpenAI 发一个真正的 `client_id`，存下来以后复用。
    3. 要的权限范围：`openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`。
    4. 令牌存在本机，0600，原子写，不进日志，也不进仓库。
    5. 调用：`POST https://api.openai.com/v1/responses`，`Authorization: Bearer <令牌>`；模型列表用同一个令牌查 `/v1/models`。
    6. 远程虚拟机：在本机登录，再用 SSH 把凭证文件传过去（文档有「Self-hosted VMs」一节）。5090 能用这个办法（推测）。
  - **限制**（W2「Preview limitations」）：
    - 必须 `store:false`、`stream:true`。
    - 不能用 system 角色的消息，用 `instructions` 或 developer 消息代替。
    - 不收 `temperature`、`max_output_tokens`、`previous_response_id` 等一串参数，所以每次都要把需要的历史放进 `input` 整个发过去。
    - 自己定义的函数工具、自由格式工具可以用。
    - OpenAI 托管的工具（图片生成、文件搜索、代码解释器、托管 MCP、`tool_search`）不能用。
    - 音频、视频不行；文字、图片、文件看模型。
  - **额度**（W1）：Plus 的五小时额度所有用套餐的 app 共用；Pro 没有五小时限制。用户可以在 ChatGPT 设置的 Usage 页看各 app 用了多少、关掉某个 app。
  - **条款**（W3，原页面 403 拿不到，下面是搜索摘要）：
    - 令牌只能经官方登录流程拿到；不能要用户的密码或 cookie。
    - 请求必须从用户自己的运行环境发出，或者从只归这个用户管的远程环境发出；后台使用要用户明确同意；别人的操作不能触发对这个用户账号的请求。
    - 套餐只能给用户连上的这个 app 用，不能给别的工具当通用接口。
  - 推测：
    - corral 在本机派出去的 agent 用同一个账号，都是「用户自己的运行环境」，符合。
    - 不能做成「给别的程序代调 OpenAI 的通用接口」：我们的服务端只服务自己的会话，不对外提供通用的模型接口，不冲突。
    - 不知道：pony 从 Slack 触发一轮算不算「用户自己的操作」。pony 只认 owner 一个人，推测算，但条款原文没拿到。
- **API key 按量**：任何一家都行，要花钱，按 token 计费。
- **Claude**：用户 10-23 说不拿 Claude 订阅接自研 agent，本报告不查。
- **本机、5090 开源模型**：不花钱，见 4.1。

---

## 5. 一个服务端、多个客户端（第 4 条）

### 5.1 opencode 和 Codex 两种分法对照

| | opencode 2.x（L2） | Codex app-server（L1、W6） |
| --- | --- | --- |
| 消息长什么样 | HTTP 资源：命令用 `POST`、`GET`、`PATCH`、`DELETE`；事件一条 SSE `GET /api/event` | JSON-RPC 2.0：请求、回复、通知；服务端也能反过来发请求 |
| 传输 | TCP 上的 HTTP，默认 127.0.0.1 | stdio（默认）、unix socket 上的 WebSocket、TCP 上的 WebSocket（实验性） |
| 数据结构 | 会话 → 消息 → 消息里的片段（part） | 会话（thread）→ 一轮（turn）→ 项目（item） |
| 批准、提问 | 权限、表单是资源；事件通知「有一个在等」；任何客户端 `POST .../reply` | 服务端发请求给所有订阅的连接；谁先回谁算；再广播「已处理」 |
| 多客户端同看 | 天然：都订 SSE、都能 POST | 每个连接各自订阅会话；`thread/resume` 订阅；最后一个走后留 30 分钟 |
| 认证 | basic auth 密码；配对码换 30 天令牌；本机登记文件 0600 | 回环不认证；非回环必须令牌文件或 JWT；远程控制经 OpenAI 自己的后端配对 |
| 服务端怎么起 | 敲命令自动起每台机器一个的后台服务；`--standalone` 起私有的 | 终端界面默认找 daemon，连不上就进程内嵌入；`--remote` 连远程；`codex app-server daemon start…` |
| 给前端的类型 | OpenAPI 文件生成客户端 | Rust 类型生成 JSON Schema、TypeScript |
| 好调试吗 | curl 就行 | 要写客户端；stdio 方便当子进程 |

两家其实在收敛：Codex 也有了常驻的 daemon 和远程；opencode 也有了 stdio 起私有服务端。差别主要在消息形状：资源接口加 SSE，还是 JSON-RPC。

### 5.2 自定协议的建议

**推荐甲：HTTP + SSE，数据结构用 Codex 的「会话 → 一轮 → 项目」。** 理由：

1. **corral、pony 是同步程序。** ranch 用 ureq，pony 还特意不引入异步运行时（DESIGN §10「用户 10-21 定的」第 4 条）。HTTP 请求加「按行读一条长响应」用 ureq 就能做（推测：要在本机 socket 上跑，ureq 能不能直接连 unix socket 没查清，见 §10）。WebSocket 要再接 tungstenite。
2. **浏览器原生。** SSE 是浏览器的 `EventSource`，断线自动重连，还会带上收到的最后一个事件 id（`Last-Event-ID`）。服务端照这个序号补发（推测：服务端留最近一段事件，比如几千条或几分钟；更早的让客户端重新取完整快照）。
3. **curl 就能调。** 排错、写测试、在 shell 里临时看一眼都方便。
4. **批准和提问做成资源，不做成「服务端发给某个连接的请求」。** 多客户端时谁都能答、先到先得，和 Codex 最后广播 resolved 的效果一样，但少了「这个请求发给了哪些连接、连接断了怎么办」这层。
5. **WebSocket 的长处在这里用处不大。** coding agent 的通信大头是服务端往外推事件；客户端发命令很少，用不着双向长连接。

**甲的坏处和对策：**

- 命令和事件走两条路，顺序要设计：命令的回复里带上「这条命令对应的事件序号」，客户端据此对齐。
- 每个客户端一条 SSE 长连接，量很小，不成问题。

**乙：JSON-RPC over WebSocket（加 stdio），照 Codex。** 好处是 Codex 的协议可以几乎照抄，request id 天然配对。如果用户更看重「直接对照 Codex 学」，乙也合理。

**不管甲乙都要做的：**

- 协议类型单独一个 Rust 包，服务端、终端客户端、以后的桌面应用共用。
- 生成 JSON Schema 和 TypeScript 给网页。
- 连上先握手，交换协议版本。
- 只做兼容的新增；不认识的字段、事件，客户端要忽略（和 ranch CONTRACT 的规矩一样）。

### 5.3 ACP 只作对照

查到：

- ACP（Agent Client Protocol）是 Zed 2025-08-27 提出来的，首个实现是 Google 的 Gemini CLI（W10）。
- 官方介绍（W7）：「Local agents run as sub-processes of the code editor, communicating via JSON-RPC over stdio」，远程「is a work in progress」。
- 远程传输（Streamable HTTP 和 WebSocket）的 RFD 2026-04-22 进入 Draft 阶段；四月成立了传输工作组（W9 及搜索结果）。
- 会话列表、会话恢复已经定稿（2026-03、2026-04，W9）。
- 关于多个客户端同看一个会话，官方的更新页里没有任何东西（W9）。
- **方向和我们相反：文件和终端归客户端。** agent 要读写文件、跑命令，得反过来请编辑器做（`fs/read_text_file`、`fs/write_text_file`、`terminal/create` 等，W8）。

不合适做主协议的地方：

1. 一对一：编辑器把 agent 当子进程拉起来，一个编辑器一个 agent；没有「一个服务端、多个客户端」。
2. 远程、认证还在草稿。
3. 「文件和命令归客户端」跟我们「工具在服务端跑」正好相反。
4. 用户也不看好。

**要不要以后单独做一层转接给编辑器用：**

- 可以做成一个子命令 `<名字> acp`：编辑器把它当子进程拉起，它在里面连我们的服务端（或者自己嵌一个），把 ACP 的 `session/prompt`、`session/cancel`、`session/update`、`session/request_permission` 和我们的命令、事件互相翻译。
- 不声明要用编辑器的文件、终端能力，工具照样在我们这边跑。
- opencode、omp 都有 ACP 模式（L2 `cli/src/acp`，L4 README §14），说明这层可以独立做。
- 推测 1,000–2,000 行。只在用户想在 Zed 之类的编辑器里用它时再做，优先级最低。

### 5.4 服务端怎么起、怎么升级

（用户定了一台机器一个常驻服务端。还要定的选项和推荐在「要用户定的」第 7 条，这里补细节。下面的做法都是推测，标了查到的除外。）

**在任意终端里敲 `colt`（暂名）时：**

1. 读登记文件，比如 `~/.colt/run/server.json`（目录 0700、文件 0600），里面有 pid、版本、协议版本、socket 路径。opencode 的登记文件是同一个思路，原话「That file is the complete discovery contract」（L2）。
2. 找到了、协议版本对得上，就连上去。
3. 找不到，或者登记的那个进程已经不在：在后台起 `colt serve`，等它写好登记文件再连。只能有一个：`colt serve` 起来先拿一把文件锁，拿不到说明已经有一个在跑，自己退出。Codex 的 daemon 也是用 pid 文件和文件锁（L1）。
4. 客户端建一个新会话，或者接上一个旧会话，然后订它的事件。
5. 关掉终端客户端，会话还在服务端里：在跑的一轮接着跑。下次 `colt --resume <会话>`，或者用别的客户端接着看。
6. 想开机就在：`colt service install` 装成 launchd、systemd 的用户服务（和 `pony service install` 一个路子）。

**会话里的命令用谁的环境、在哪个目录跑**（常驻服务端要专门处理）：

- 查到：Codex 的 daemon 跑命令用的是 daemon 启动时继承的环境，不随客户端变（L1，见 §3.1）。
- 建议：建会话时，客户端把自己的工作目录和环境变量一起交给服务端，这个会话的命令都用它们。
  - 这样在哪个终端开，命令就像在哪个终端里跑，和现在直接在终端里开 agent 的感觉一样。
  - corral 给 agent 设的 `CORRAL_NAME` 等环境变量也就带进了命令里，corral 不用另做（§6.3）。
- 环境里可能有密钥：只放在这个会话的内存和本机文件里（0600），不进事件流，不发给别的客户端。

**升级时（推荐的排空交接）：**

```text
 旧服务端 v1                   新服务端 v2                       客户端
   │ 在跑：会话 A（这一轮没完）、会话 B（空闲）
   │                            │ 起来，接管登记文件
   │←──────── 请你排空 ───────────│
   │ 不再接新的一轮
   │ B 空闲，马上交出（放开 B 的锁）
   │                            │ 从盘上加载 B，序号接着编
   │                            │←──────────── B 的客户端断线，读登记文件重连，从最后序号续订
   │ A 这一轮跑完（或者超过上限被打断），存盘，交出 A
   │                            │ 加载 A
   │ 退出                        │←──────────── A 的客户端重连
```

- 每个会话同一时刻只归一个服务端：每个会话一把文件锁，谁拿着谁写。
- 事件序号存在会话文件里，新版接着编，客户端续订不漏也不重。
- 旧版的程序文件要等旧服务端退出后才能删。照 ranch 那样装进不可变的版本目录就行。
- 旧客户端连新服务端：协议只做兼容的新增，旧客户端照旧能用；协议大版本变了，客户端提示换版本。

**服务端崩了：**

- 所有会话在跑的轮都断。这是常驻的代价，单机模式没有这个问题。
- 重启后从盘恢复，在跑的轮记成「被打断」。
- 执行器拆成子进程后（§6.4 做法三），执行器发现和服务端的连接断了，就杀掉自己起的命令、退出，不留孤儿进程。opencode 的 `--standalone` 子进程就是把标准输入当租约，管道一断就退（L2）。

**单机模式（备选，不推荐作主力）的取舍：**

| | 常驻服务端（推荐） | 单机模式 `--standalone` |
| --- | --- | --- |
| 关掉终端 | 会话还在，在跑的轮接着跑 | 会话停了（存盘还在，能接着聊） |
| 网页、桌面、手机 | 连一处就看到所有会话 | 每个终端一个服务端，要另做网关汇总 |
| 升级 | 要做排空交接 | 各终端各跑各的版本，不用管 |
| 崩了 | 所有会话一起断 | 只坏一个 |
| 后台进程 | 要管一个常驻进程（自动起，或者装成系统服务） | 没有 |
| 套墙、暂停 | 按会话做，靠执行器和会话命令（§6） | 整个进程一起套墙、一起冻，照 corral 现在的办法就行 |

单机模式适合：临时 ssh 到别的机器、跑测试、不想留后台进程的时候。用的是同一份服务端代码，只是不经 socket，客户端在进程里直接和它说话。Codex 的 `app-server-client` 就是这样一层「进程内客户端」（L1）。

**桌面应用怎么带服务端（以后）：**

- **A** 随应用一起装一份服务端程序，自己拉起一个本机服务端。opencode 的桌面版就直接起它的后台服务（L2 `server-process.ts` 的注释「Desktop can launch the service directly」）。paddock 单独分发时也是打包带一份 ranch（DESIGN §2）。
- **B** 只连已经在跑的。
- **推荐 C：** 先读登记文件，找到协议版本对得上的常驻服务端就连；没有，就用自带的那份起一个常驻的（不是只给桌面用的私有服务端）。这样终端、网页、桌面看到的是同一批会话。

### 5.5 协议里要有哪些消息

建议的最小集合（甲的写法；乙就是同名的 JSON-RPC 方法）：

**命令（客户端 → 服务端）**

| 命令 | 作用 |
| --- | --- |
| `GET /v1/info` | 服务端版本、协议版本、这个服务端里有哪些会话 |
| `POST /v1/sessions` | 建会话：工作目录、模型、权限模式、可选的首句、名字 |
| `GET /v1/sessions`、`GET /v1/sessions/{id}` | 列表；一个会话的完整快照：状态、各轮、各项目、待处理请求、最后的事件序号 |
| `POST /v1/sessions/{id}/input` | 送一句话：空闲就开新一轮；在干活时按参数排队或插话（照 Codex 的 steer、opencode 的 inbox） |
| `POST /v1/sessions/{id}/interrupt` | 打断这一轮 |
| `POST /v1/sessions/{id}/requests/{rid}` | 回答一个待处理请求：批准（允许这一次、本会话都允许、拒绝、拒绝并停下）或提问（各题选了什么、或者文字）；带上自己的客户端名，好写进「已处理」事件 |
| `PATCH /v1/sessions/{id}` | 改模型、权限模式、名字 |
| `POST /v1/sessions/{id}/compact` | 手动压缩 |
| `DELETE /v1/sessions/{id}` | 结束会话（存盘还在） |
| `GET /v1/events?session={id}&after={序号}` | SSE 事件流 |

**事件（服务端 → 客户端）**，每条都有：序号、会话 id、时间。

| 事件 | 带什么 | 客户端拿它做什么 |
| --- | --- | --- |
| `session.status` | `starting`／`idle`／`working`／`waiting_approval`／`waiting_answer`／`error`／`closed` | corral 的状态、pony 的 `?` |
| `turn.started` | 轮 id、触发这一轮的输入、来源（哪个客户端） | 「在干活」，起计时 |
| `turn.completed` | 结果：完成、被打断、失败（带错误）；用量 | 「一轮结束」，corral 的 `interrupted`、pony 贴回复 |
| `item.started`／`item.delta`／`item.completed` | 项目 id、种类、增量或最终内容 | 显示 |
| 项目种类 | 用户的话；模型的话（文字增量）；思考摘要；命令（完整命令、目录、输出增量、退出码、用时）；改文件（每个文件的 diff）；读文件、搜索；计划；压缩（摘要）；以后的 MCP 调用、子 agent | 各自的卡片 |
| `request.opened` | 请求 id、种类（批准／提问）、工具名、**完整参数**（完整命令、要改的 diff、题目和选项、单选还是多选）、可选的回答、开始时间 | 终端弹框；pony 发按钮；corral 状态 blocked，带 `questions`、`command` |
| `request.resolved` | 请求 id、结果（哪个回答，或者取消、超时）、谁答的 | 所有客户端关框、撤按钮 |
| `session.usage` | 上下文用了多少、离上限多远 | 显示，决定提醒压缩 |
| `warning`、`error` | 可以重试的、不能重试的 | 显示 |

**会话状态的变法：**

```text
           送一句话                 要问人
  idle ─────────────→ working ─────────────→ waiting_approval / waiting_answer
   ▲                    │  ▲                         │
   │   一轮结束          │  └────── 有人回答了 ───────┘
   └────────────────────┘
   （打断：从 working 或 waiting_* 直接回 idle，turn.completed 写「被打断」）
```

这和 Codex 的 `idle`、`active`（带等批准、等回答两个标记）一一对应（L1）。

### 5.6 几个客户端同看一个会话、远程连接、认证

- **同看**：协议本来就支持。任何客户端先取快照，再从快照里的序号订事件。批准先到先得。两个客户端同时送话就进队列（见 `input` 的排队参数）。
- **手机上看、手机上批准**：要有一个手机能连上的地址。推测几种办法：
  1. **局域网或 Tailscale**：常驻服务端另开一个 TCP 端口，带令牌认证，手机浏览器打开网页客户端。照 opencode 的配对码：终端里显示一个 5 分钟有效的码或二维码，手机输一次换一个长期令牌（L2）。
  2. **中转**：照 omp `/collab`，会话内容在本机加密，经一个中转服务器转给浏览器；中转看不到内容（L4）。不用开端口，但要有一个中转服务器。
  3. **已经有的 pony**：Slack 本来就是手机上的入口，接 corral 做到做法四之后，pony 的批准按钮就是「手机上批准」（§7.4）。
- **认证建议（推测）**：
  - 本机 unix socket 只靠文件权限（目录 0700、属于当前用户），不要令牌。
  - TCP 一律要令牌：令牌文件 0600，照 Codex 的令牌文件和 opencode 的密码。
  - 只监听 127.0.0.1，除非用户显式改。
  - 要加密就交给 Tailscale 或反向代理，不自己做 TLS。
  - 和 pony「凭证只放本机、0600、不进日志」一个路子。

---

## 6. 独立，但能接进 corral（第 5 条）

### 6.1 corral 现在怎么认一个 agent

查到（DESIGN §7，`crates/corral/src/agents/mod.rs`）：

- 一个 agent 就是 corral 的 pen 在一个终端（PTY）里起的一棵进程树。
- 状态主要靠各家的钩子往事件文件里写 v1／v2 事件，命令行再按状态机算出 starting、working、idle、blocked、unknown。
- 钩子报不全的，靠标题（amp）或读屏（agy）兜底。
- 各家都有特例，比如：
  - 认打断：R22 Codex 的 Interrupt、R24 去 Claude Code 的对话记录里找标记、R25 omp 的错误位、R26 agy 认不出。
  - 认提问框题目：R43 只认 Claude Code。
  - 认 Bash 原始命令：R47c 只认 Claude Code。
- 适配器接口（`Adapter` trait）：
  - 种类（程序的文件名）、启动时加的钩子参数、停止步骤、读哪些技能位置。
  - 带不带首句、首句怎么传。
  - 全局安装、读屏规则、信任列表等可选项。

### 6.2 自研 agent 从根上不需要这些特例

服务端直接说「一轮开始」「在等批准，命令是这条」「被打断了」「回复是这段」。事件和 corral 字段的对应：

| corral 要的 | 从哪来 |
| --- | --- |
| `starting` | 服务端起来了、会话还没收到第一句（`session.status: starting`） |
| `working` | `turn.started` 之后 |
| `blocked` | `request.opened` 之后、`request.resolved` 之前 |
| `idle` | `turn.completed` 之后 |
| `interrupted` | `turn.completed` 的结果是「被打断」 |
| `reply` | 这一轮最后一个「模型的话」项目的全文 |
| `questions`（R43） | `request.opened` 里提问的题目和选项，完整的，不用截 |
| `command`（R47c） | `request.opened` 里批准的完整命令 |
| `last_tool` | 最近一个工具项目的种类 |

### 6.3 常驻服务端下，corral 的每样功能怎么办

**原则**（用户 10-23：「这个你要给他，我已经说了不需要和corral绑定死」）：agent 不依赖 corral，架构不为 corral 的套墙、暂停、停止、升级来定；corral 去适配 agent 的公开接口（命令行、协议、钩子）。

下面每一样都写：照 corral 现在的办法会怎样、可行的做法、两边各要做什么。

- 「agent 要有的」只放通用功能，不写 corral 的名字，别的客户端、用户自己也用得上。
- 「corral 要做的」是 corral 去适配的部分。

前提：pen 里照旧开终端客户端 `colt`，它连常驻服务端、建一个会话并接上去。所以 corral 现有的 attach、paddock 终端、`read`、`keys`、`send` 都对着这个终端客户端，照旧能用。

| corral 的功能 | 照现在的办法会怎样 | 可行的做法 | agent 要有的（通用） | corral 要做的 |
| --- | --- | --- | --- | --- |
| `start`（名字、标签、首句、免确认） | pen 起 `colt`，终端客户端去建会话 | 照旧 | 会话可以带名字和标签；`--yolo`；`--` 后面是首句 | 加适配器（做法一） |
| 命令里的 `CORRAL_NAME` 等环境变量（派活、R18 子 agent 套墙都靠它认调用者） | 命令在服务端里跑，默认拿不到客户端的环境（Codex 的 daemon 就是这样，§3.1） | 会话用建它的客户端的环境和目录 | 建会话时带上客户端的环境和目录（§5.4） | 不用做 |
| 状态、`reply`、`interrupted`、`questions`、`command` | 不接钩子就不知道 | 甲：通用钩子；乙：订事件流 | 按会话的钩子：服务端在这个会话的环境里跑钩子命令；事件流本来就有 | 甲：适配器挂上 `corral __hook`（做法一）；乙：pen 当协议客户端（做法四） |
| attach、paddock 终端、`read`、`keys`、`send` | 对着终端客户端，照旧 | 照旧 | 不用做 | 不用做 |
| `stop` | 只杀了客户端，会话还在服务端里跑 | 客户端退出时关掉会话；客户端被 `kill -9` 时由服务端收尾 | 「客户端退出就关会话」的选项；带这个选项的客户端连接断了、限时内没重连，服务端就打断并关掉会话（租约） | 适配器带上这个选项，停止步骤发 `TERM`（做法一） |
| `pause`、`resume`（对进程组发 SIGSTOP，DESIGN §6） | 只冻住客户端，会话照跑、照调模型 | 服务端按会话暂停 | 会话的暂停、继续：不再发新的模型请求、不再起新工具，冻住这个会话正在跑的命令 | 适配器加一项通用能力「暂停、继续、停止走 agent 的命令」（做法二）；`paused` 照旧由 corral 记 |
| `--sandbox`（fence 套墙，DESIGN §8） | 墙只围住客户端，命令在墙外的服务端里跑，**等于没套** | 甲：执行器前缀；乙：agent 自带沙箱 | 甲：执行器拆成子进程，建会话时可以给它启动前缀；乙：自带 Seatbelt、Landlock | 甲：`--sandbox` 时把 `fence run … --` 当前缀传进去，fence 加执行器用的数据；乙：把 `--sandbox` 翻成 agent 的参数（做法三） |
| `upgrade`（换 pen） | 照旧：pen 里是终端客户端，换 pen 不碰服务端 | 照旧 | 不用做 | 不用做 |
| agent 服务端自己升级 | 不归 corral 管 | 排空交接（§5.4） | 排空交接；客户端自动重连、续订 | 不用做（corral 看到的只是终端客户端短暂重连） |

### 6.4 几种做法细说

**做法一：适配器 + 通用钩子（第二期先做）**

- agent 侧（通用功能，推测 500–800 行）：
  - 钩子：配置里或者建会话时写「某个事件发生时运行某条命令，事件的 JSON 从标准输入给它」。事件名照 Claude Code、Codex 的习惯：会话开始、送一句话、工具前后、等批准（带完整参数）、一轮结束（带回复）、被打断。钩子由服务端在这个会话的环境和目录里运行。cairn 以后也能用同一套。
  - 会话的名字和标签；会话用客户端的环境（§5.4）；「客户端退出就关会话」和连接租约。
- corral 侧（推测 100–200 行加测试）：
  - 加 `agents/colt.rs`，在 `ADAPTERS` 登记一行（DESIGN §7「加一家」）。
  - `hook_args` 挂上 `corral __hook`，再带上「退出就关会话」；停止步骤写 `[{"signal":"TERM","wait":5}]`。
  - 事件翻成现有的 v1 事件，状态机不改。
  - 要用到 `questions`、`command` 字段的话，照 Claude Code 那份翻译补上。
- 暂停、套墙：做法二、三做好之前，corral 对这一家的 `pause`、`--sandbox` 直接报「不支持」，照 fence「出错一律启动失败，不悄悄不套墙」的规矩。这要给适配器加一个开关，是 corral 共用代码的小改动（推测几十行）。
- 好：corral 核心几乎不动，接上就能派活，paddock 看得到状态。
- 坏：还是「钩子报事件」；批准还得按键，pony 按钮照旧要读屏核对。

**做法二：暂停、继续、停止走 agent 的会话命令**

- agent 侧（推测 300–500 行）：
  - `colt session pause|resume|close <名字或 id> --json`，命令行背后调服务端的接口。
  - 暂停：不再发新的模型请求、不再起新工具；正在跑的命令每条有自己的进程组，服务端记着，对它们发 SIGSTOP。已经发出去的那次模型请求怎么办见 6.5。
  - 继续：解冻，接着干。
- corral 侧（推测 200–400 行加测试，写进 DESIGN）：
  - 适配器加一项通用能力：声明「暂停、继续、停止时运行这几条命令」，命令模板里填会话名字。pen 照它做，不再只发信号。
  - `status` 的 `paused` 等照旧由 corral 记；paddock 用的命令和 JSON 不变。

**做法三：套墙走执行器前缀**

- **甲（推荐）执行器前缀。**
  - agent 侧（推测 800–1,500 行）：
    - 执行器拆成子进程，每个会话一个。照 Codex exec-server 的分法，起进程、读写文件都在执行器里做（L1）。第一期先把接口定好，这时只换实现。
    - 建会话时可以给执行器一个启动前缀，服务端用「前缀 + `colt __executor`」起它。agent 不知道前缀是 fence 还是别的。
  - corral 侧：适配器声明「套墙走 agent 的参数」。`--sandbox` 时 pen 不再用 fence 包整条命令，而是把 `fence run --kind … --` 当前缀交给 `colt`。`fence check`、出错就启动失败的规矩照旧。
  - fence 侧：加一份执行器用的种类数据：放行工作区、临时目录；不用放行模型服务的域名，因为调模型在服务端，在墙外。查资料的 MCP 以后如果也放进执行器里跑，再给 `[mcp]` 模板。
  - 不知道：`fence run` 能不能直接包一个不是 agent 的程序，还是要小改 fence。没读 fence 的代码。
- **乙 agent 自带系统沙箱。** 照 Codex：macOS 用 Seatbelt，Linux 用 Landlock 或 bubblewrap。
  - agent 侧工作量大，Codex 这部分是好几个包。
  - corral 只把 `--sandbox` 翻成 agent 的参数。
  - 坏处：fence 的规则（`dev` 档案、`local.toml` 里给 IP Guardian 的放行）用不上，变成两套规则。
- **推荐甲。** 规则还是 fence 一处管，agent 只多一个通用的「前缀」。

**做法四：pen 订服务端的事件流**

- pen 照旧在终端里起 `colt`。适配器多声明一项「状态来自服务端」。
- pen 用 agent 公开的办法找到服务端和会话：读登记文件，或者调 `colt session info <名字> --json`。登记文件和这条命令要写进 agent 的公开约定。
- pen 连上去订 SSE，把事件翻成 v1 事件写进事件文件。这一层只认协议，不写 `colt` 的名字，以后别的说同一种协议的 agent 也能用。
- 再加兼容新增的命令：`corral approve NAME <请求号> <回答>`、`corral answer NAME <请求号> …`，由 pen 调服务端的「回答请求」。pony 的按钮直接带请求号，不读屏、不按键。
- 好：状态和批准都是结构化的，没有歧义（请求号对上才算）；不用每个事件起一个钩子进程。
- 坏：
  - corral 的共用代码要加「pen 当这种协议的客户端」一块，推测 400–700 行，加测试。
  - 新命令要写进 CONTRACT，paddock、pony 在各自的任务里跟。

**做法五：corral 不开终端，直接当客户端（不推荐第二期做）**

- 常驻服务端下，这条比以前顺：会话本来就在服务端里，corral 还能把别的客户端（网页、桌面）开的会话也列出来。
- 但 attach、paddock 终端、`read`、`keys` 都对着终端，要为它另做一套，paddock 也要跟。好处只是省一个终端进程。
- 等桌面、网页做出来，真要在 corral 里管别的客户端开的会话时再看。

**次序和改哪边：**

| 做法 | agent 要做 | corral 要做 | fence 要做 | 动 corral 共用代码 |
| --- | --- | --- | --- | --- |
| 一 | 钩子、会话名字、会话环境、退出就关会话 | 适配器；「不支持暂停、套墙」的开关 | 不用 | 很少（开关） |
| 二 | 会话的暂停、继续、关闭命令 | 适配器能力「生命周期走命令」 | 不用 | 要 |
| 三（甲） | 执行器子进程和启动前缀 | 适配器能力「套墙走参数」 | 一份执行器数据（可能小改） | 要 |
| 四 | 公开登记文件或 `session info` | pen 当协议客户端；`approve`、`answer` | 不用 | 要，新增公开约定 |

做法二到四都要动 corral 的共用代码，要用户同意。做出来的都是通用能力，以后接别的常驻服务端式 agent 也能用（推测；opencode 2.x 默认就是每台机器一个后台服务，L2）。

### 6.5 做不到的，或者只能做到一部分的

1. **一堵墙围住整个 agent。** 服务端是大家共用的，不能为一个会话把它整个关进墙里；能围住的是这个会话的执行器，也就是它跑的命令、动的文件。调模型、存会话、跑钩子都在服务端里，在墙外。现在 fence 是「整个 agent 在墙里」，这一家会变成「agent 干的活在墙里」。推测：防「模型让命令乱写、乱连」够用，防不了服务端本身出问题。
2. **冻住已经发出去的模型请求。** 暂停时如果正在收模型的回复，只能二选一：
   - 收完，但先不照着做：暂停后还会再用一会儿网络。
   - 掐断，继续时重发：要再花一次 token，套餐额度也多用。
   - 用户当初要的是「不让其对外有连接或者之类的行为」（DESIGN §6）。对这个会话能做到「不再发新的请求」；服务端本身还在线，别的会话、别的客户端照常。
3. **升级时一个都不打断。** 排空交接能让在跑的轮跑完再换；设了上限、到点还没跑完的，只能打断。直接重启（Codex、opencode 的做法）做不到不打断。
4. **服务端崩了不影响别的会话。** 做不到，这是常驻的代价（§5.4）。
5. **corral 看到别的客户端开的会话。** 做法一到四只管 corral 自己开的；要列出网页、桌面开的会话，得做法五。
6. **做法三之前套墙。** 执行器还在服务端进程里时，没法只围住一个会话，所以 `--sandbox` 只能报「不支持」。

### 6.6 带首句、免确认、退出

- **带首句启动：** `colt [选项] -- <首句>`，首句以 `-` 开头也不会被当成选项。和 corral 默认的 `prompt_args`（在 `--` 后面）一致，适配器不用改这部分。
- **免确认启动：** `colt --yolo`（或 `--approval-mode yolo`），写进分工表的启动参数（§7.3）。
- **退出：**
  - 终端客户端收到 SIGHUP、SIGTERM 时：带了「退出就关会话」，就先请服务端打断在跑的一轮、关掉会话，再退出，几秒内退完；没带，就只是断开，会话留在服务端里。
  - 适配器的停止步骤就能写成 `[{"signal":"TERM","wait":5}]`。不像 Codex 要两次 Ctrl-C 加 60 秒（`agents/codex.rs` 的注释）。
- **信任目录：** 不做「每个目录都要人点信任」，或者认上级目录（R13 里 agy 每个 worktree 都要点信任，是派活时的麻烦）。

---

## 7. 和 ranch 其他部分的关系（第 6 条，都是可选的，不接也能单独用）

### 7.1 两份技能（corral、corral-dispatch）

- 查到：corral 的技能目标 `codex` 就是 `~/.agents/skills`，几家共用；Codex、pi、omp 都从这里读（DESIGN §7「技能位置」）。
- 建议：自研 agent 读 `~/.agents/skills` 和项目里的 `.agents/skills`，格式照 Agent Skills（`SKILL.md` 带名字和简介）。
- 这样 `corral install-skills`、`ranch dispatch install-skills` 一行不用改；适配器的 `skill_targets` 写 `["codex"]`。
- 技能本身推测 200 行左右：列清单进系统提示词，用到时用 read 工具读全文。

### 7.2 fence 的 sandbox

- 查到（DESIGN §8）：fence 用 `fence run … -- <原命令>` 把整个 agent 起在 srt 的墙里，按种类数据放行它要的路径和域名；各家的数据（模型服务的域名、配置目录、查资料的 MCP 模板）都是 TOML，编进 fence，也能在 `~/.fence/` 下整份替换。
- 常驻服务端下，fence 包不住整个 agent（§6.5 第 1 条），只能围住每个会话的执行器：§6.4 做法三甲，agent 给执行器留一个启动前缀，corral 把 `fence run … --` 传进去。
- 要加一份执行器用的种类数据：
  - 放行工作区和临时目录的读写。
  - 不用放行 `api.openai.com` 这些模型服务的域名，也不用放行 agent 的凭证文件：调模型、刷新令牌都在服务端里，在墙外。比现在「整个 agent 在墙里」少放行一类东西。
  - 查资料的 MCP 如果放在执行器里跑，再给 `[mcp]` 模板。
  - 尽量只是 fence 的数据，不改 fence 的 Rust（R30 的设计）。
- 不知道：
  - `fence run` 包的是执行器而不是 agent 本身，fence 要不要小改（比如种类数据里的启动参数、MCP 模板都是按 agent 设计的）。没读 fence 的代码。
  - srt 包一个长时间运行、反复起子进程的执行器，有没有别的问题。要实测。
- 备选是 agent 自带系统沙箱（做法三乙），那样就不经 fence，规则变两套。

### 7.3 派活分工表（R31、R34）

- 查到（DESIGN §9）：`dispatch.toml` 的 `agents` 按名字定义各家的 `argv`、`labels`、`efforts`、`models`，`cells` 选用哪几家；本机 `~/.ranch/dispatch.toml` 能覆盖、能加新家；Rust 不认具体是哪家。
- 加它只要加一家数据，比如 `[agents.colt] argv = ["colt", "--yolo", "-m", "${model}", "--effort", "${effort}"]`，再由用户决定哪些格子选它。比如低档 `low-*` 从 pi、omp 换成或加上它。不改 Rust，不重装技能（R35 之后改本机表下次读就生效）。
- 要用户定：它进内置默认表，还是先只放本机表试用。

### 7.4 pony 的按钮

- 查到（DESIGN §10）：现在 pony 的「拒绝」「打断」只按 Esc；「允许这一次」只给认得出的 Claude Code 框，要读屏、比对原始命令、核对六样、按一个数字键，还有已知的空隙（R47c 已知限制：`keys` 不带实例、不原子）。
- 做法一下：pony 照 Claude Code 的办法处理它：`questions`、`command` 字段有，批准仍要读屏核对。要在 pony 的识别表里加它的框长什么样，是 pony 自己的任务。
- 做法四下：按钮直接对应请求号，「允许这一次」「拒绝」「选项」都调 `corral approve`、`corral answer`，服务端只认对得上的请求号，**读屏、按键、空隙这一整套都不需要**。这是做法四最大的收益。
- 不变的：pony 只走 corral 的公开命令（DESIGN §10「结构」）。做法四新增的是 corral 的命令，pony 仍不直接连 agent。
- 另一条路（以后，要用户定）：常驻服务端本来就能让多个客户端同时批准，pony 也可以直接当这个 agent 的客户端。但这样 pony 就绕开了 corral，和 DESIGN §10「和 corral 只走命令」的结构不一样。

### 7.5 cairn、paddock

- **cairn：** 不知道 cairn 现在怎么接各家（本轮范围外，没查）。它要在开局注入、结束时记录，做法一的通用钩子就是接口：会话开始时能往上下文里加一段，一轮结束时能拿到回复。
- **paddock：**
  - 照旧经 corral 看它、在终端里开它，什么都不用改。
  - 以后如果要在 paddock 里用原生界面显示这个 agent 的会话（不是终端画面），可以直接连这个 agent 的协议。它是一个独立产品的公开协议，不是 ranch 的内部状态。是否允许 paddock 直接连它，要用户定，属于以后的事。

---

## 8. 放在哪、用什么做界面（第 7 条，只列选项和取舍）

### 8.1 仓库

见「要用户定的」第 3 条。

放进 ranch 的理由只有共用打包和部署。不放的理由：

- 独立产品。
- 依赖不同：tokio、axum、ratatui、crossterm；ranch 是同步风格，还明令不用 ratatui、crossterm。
- 发布节奏不同。
- 可能要开源。

### 8.2 终端客户端

**要不要全屏：**

| 做法 | 谁这样 | 好 | 坏 |
| --- | --- | --- | --- |
| 不占全屏：说完的话写进终端自己的滚动区，只有底部输入区归程序管 | Codex 默认（L1）；Claude Code 也是这样（凭使用经验，没查出处） | 终端的滚动、复制、搜索都能用；tmux、`corral attach`、`corral read` 看得自然 | 已经写出去的内容改不了（比如流式时的 markdown 重排）；复杂的界面要临时切全屏 |
| 全屏：整屏都归程序画 | opencode（opentui）；pi 默认全屏，可改普通模式（L3 `docs/usage.md`） | 想怎么画都行，能做侧栏、面板 | 滚动、复制靠程序自己做；`corral read` 读到的是一堆重画指令 |

推荐先做不占全屏的。会话列表、看 diff 这类再临时切全屏，Codex 就是这样。

**Rust 的终端界面库**（W14，2026-10-23）：

| 库 | 版本 | 总下载 | 最近更新 | 说明 |
| --- | --- | --- | --- | --- |
| `ratatui` | 0.30.2 | 6,302 万 | 2026-06-19 | 最主流（约 2.3 万星，W19）；Codex 用；即时模式画控件 |
| `crossterm` | 0.29.0 | 2.17 亿 | 2025-04-05 | 底层的按键、光标、颜色；ratatui 默认配它；Codex 用了自己的分支 |
| `termwiz` | 0.23.3 | 2,862 万 | 2025-03-20 | WezTerm 的底层库 |
| `termion` | 4.0.6 | 1,504 万 | 2025-11-21 | 只支持类 Unix |
| `iocraft` | 0.9.3 | 20 万 | 2026-10-14 | 像 React 的声明式写法；小 |
| `tui-realm-stdlib` | 4.1.0 | 19 万 | 2026-05-02 | 建在 ratatui 上的组件框架 |
| `cursive` | 0.21.1 | 213 万 | 2024-08-03 | 老牌，两年没发新版 |

推荐 ratatui + crossterm：最成熟、Codex 是现成的范本。ranch「不依赖 ratatui、crossterm」只管 ranch 仓库。

### 8.3 服务端用的库（W14、W18）

| 用途 | 库 | 说明 |
| --- | --- | --- |
| 异步运行时 | `tokio` | 要同时管模型流、命令输出、多个客户端、随时打断，同步写法吃力 |
| HTTP 服务、SSE、WebSocket | `axum` 0.8.9 | tokio 官方出的；自带 SSE 响应；`serve` 直接支持 `TcpListener` 和 `UnixListener`（W18），本机 socket 和 TCP 一套代码 |
| （选乙时）JSON-RPC | `jsonrpsee` 0.26.1，或者自己在 `tokio-tungstenite` 0.30.0 上写 | Codex 是自己写的 |
| 调模型的 HTTP | `reqwest` 0.13.5 + `eventsource-stream` 0.2.3 | Codex 同款 |
| 协议类型导出 | `schemars` 1.2.3、`ts-rs` 12.0.1（或 `specta` 1.0.5） | |
| MCP（以后） | `rmcp` 3.7.0 | 官方 Rust SDK，Codex 用 |
| ACP（以后） | `agent-client-protocol` 3.5.0 | 官方 Rust SDK |
| 改文件、diff | `similar` 3.2.0 | 算 diff 给客户端显示 |
| 搜文件 | `ignore`、`globset` | ripgrep 作者的库，认 `.gitignore`。这两个在 crates.io 查询时解析出错，版本没核对 |

### 8.4 网页客户端

- **用什么做：** TypeScript；框架（React、Svelte、Solid……）不定。类型从协议包生成。
- **谁起网页服务：**
  - **A（倾向）** 常驻服务端在它的 TCP 端口上同时发网页的静态文件。opencode 这样做（L2 `server-process.ts` 里的 `WebUi.handler()`）。
  - **B** 单独一个小网关：发网页，同时把几个服务端汇总成一个端口。只有大量用单机模式（每个终端一个服务端）时才需要。
- 常驻服务端定了以后，A 最省事。

---

## 9. 分期和工作量（第 8 条）

工作量都是推测：按 Rust 代码行（不含测试）估。参照：pi 的循环 1,600 行 TypeScript；pi 的整个 coding-agent 13.5 万行；Codex 187 万行。按 ranch 现在一件活一个 agent 的做法，给出大概能拆成几件。

### 9.1 第一期：常驻服务端 + 终端客户端，在任意终端里开、能干一件小活

| 块 | 内容 | 推测行数 |
| --- | --- | --- |
| 协议包 | 会话、一轮、项目、待处理请求、事件、序号；命令；版本握手；导出 Schema | 1,000–1,500 |
| 模型接入 | Responses 流式客户端；ChatGPT 套餐登录（PKCE、本机回调、令牌存取和刷新）；API key；重试；用量 | 1,500–2,500 |
| 循环 | 一轮的循环、插话和排队、打断（取消令牌传到模型请求和工具）、截断时不执行工具 | 800–1,200 |
| 上下文 | 系统提示词、环境信息、`AGENTS.md`、压缩（手动、超限自动） | 600–1,000 |
| 工具 | read、write、apply_patch（搬 Codex 的约 4,800 行不算在内）、shell（超时、截断、杀进程组）；权限闸门；执行器接口（第一期在服务端进程里） | 1,500–2,500 |
| 会话存盘 | 追加写 JSONL（带事件序号）、每个会话一把锁、恢复、列表 | 500–900 |
| 服务端 | 常驻：`colt serve`、只许一个（文件锁）、登记文件、客户端找不到就在后台起；本机 unix socket 上的 HTTP + SSE；多客户端；待处理请求先到先得；会话用客户端的环境和目录；升级先做「等一会儿再重启」、版本不同不自动换 | 1,800–2,800 |
| 终端客户端 | 输入框（多行、粘贴）；流式 markdown；工具卡片；diff；批准框；Esc；会话列表和接着聊；登录界面；服务端重启后自动重连、续订 | 3,000–5,000 |
| 命令行和配置 | 参数（首句、`--yolo`、`--resume`、`-m`、`--effort`）、`colt serve`、配置文件、打包 | 700–1,200 |
| **合计** | | **约 1.2 万–1.9 万行**，另加测试（假的 Responses 服务端回放 SSE，照 ranch 的规矩不连真服务） |

建议拆成（推测）：

1. 协议包 + 会话存盘。
2. 模型接入（先 API key，再套餐登录）。
3. 循环 + 打断 + 压缩。
4. 工具 + 权限 + 执行器接口。
5. 服务端（常驻、自动起、登记文件、socket、SSE、多客户端）。
6. 终端客户端（一）：对话、流式、工具卡片。
7. 终端客户端（二）：批准框、Esc、会话列表、登录、重连。

前五件有依赖，但 2 和 4 可以并行；6、7 可以先对着假服务端做。

做完的样子（照用户的话）：在任意终端里敲一个命令就能开，能干一件小活。

### 9.2 第二期：接上 corral 这一套（paddock、pony 看得到状态，能派活）

按 §6 的原则，agent 那边只做通用功能，corral、fence 去适配。次序照 §6.4：做法一 → 二 → 三 → 四。

| 件 | 内容 | 在哪 | 推测 |
| --- | --- | --- | --- |
| 通用钩子和会话选项 | 做法一 agent 侧：按会话的钩子；会话名字和标签；退出就关会话、连接租约 | agent 仓库 | 500–800 行 |
| corral 适配器 | 做法一 corral 侧：`agents/colt.rs`、登记一行、事件翻译；「不支持暂停、套墙」的开关；`state::FILES` 若有新文件要登记 | ranch | 100–200 行加测试 |
| 技能、提问框 | 读 `~/.agents/skills`；模型问人的工具，题目进 `request.opened` | agent 仓库 | 500–700 行 |
| 分工表 | 加一家数据 | ranch 数据或本机表 | 几行 |
| 排空交接 | 服务端升级时在跑的轮先跑完再交给新版（§5.4）；派活前做好 | agent 仓库 | 500–900 行 |
| 会话暂停、继续、关闭 | 做法二：agent 的会话命令；corral 适配器能力「生命周期走命令」 | 两边各一件 | agent 300–500 行；corral 200–400 行加测试 |
| 执行器子进程和套墙前缀 | 做法三甲：agent 的执行器子进程和启动前缀；corral 适配器能力「套墙走参数」、fence 执行器数据、墙里实测 | 两边各一件 | agent 800–1,500 行；corral、fence 几百行加实测 |
| pen 订事件流 | 做法四：pen 当协议客户端；`corral approve`、`answer`；CONTRACT | ranch | 400–700 行加测试 |
| pony 跟进 | 结构化的批准按钮 | ranch（pony 自己的任务） | 另起 |

合起来约 8–10 件（不算 pony）。前两件做完就能派活、paddock 看得到状态；暂停、套墙在做法二、三之前对它报「不支持」。

### 9.3 之后几期（列要点）

- **多家模型：** Chat Completions 客户端，接本机、5090 的开源模型和 DeepSeek、Kimi 等；改文件加「按文字替换」并按模型选用。
- **系统服务：** `colt service install`，装成 launchd、systemd 的用户服务。
- **远程和认证：** 常驻服务端开 TCP 监听、令牌、配对码；可选中转。
- **网页客户端：** TypeScript，由常驻服务端发网页；同看一个会话、手机上批准。
- **桌面应用：** GPUI 或 Tauri；连常驻服务端，没有就用自带的那份起一个。
- **MCP：** 用 `rmcp`；fence 的查资料 MCP 接上。
- **子 agent、计划、网页搜索、图片。**
- **单机模式** `--standalone`（要的话）。
- **agent 自带系统沙箱**（照 Codex 的 Seatbelt、Landlock，§6.4 做法三乙），要的话；否则全靠执行器前缀加 fence。
- **corral 直接当客户端**（§6.4 做法五），要在 corral 里管网页、桌面开的会话时再做。
- **ACP 转接**（给 Zed 等编辑器）。

---

## 10. 没查清的事

- **ChatGPT 套餐：**
  - 只是公开仓库、不带开源许可证算不算「open-source partner」（§「要用户定的」第 8 条）。
  - 条款原文：openai.com 和 help.openai.com 对抓取返回 403，只看到搜索摘要。
  - 有一处搜索摘要提到帮助中心说 Go 套餐也能用，和开发者文档写的「Plus 和 Pro」不一致，没核对。
  - 预览期以后会不会变。
  - pony 从 Slack 触发算不算条款里的「用户自己的操作」。
  - 用户现在是 Plus 还是 Pro：留在「要用户定的」第 2 条。
- **思考内容跨轮带上：** `store:false` 时，要带 `include: ["reasoning.encrypted_content"]` 才能把思考带到下一次请求。这是我对 Responses 接口的理解，没核对 Codex 的具体写法，也不知道套餐这条路收不收。官方文档说 Codex app-server 走这条路「Local thread history and `thread/resume` still work」（W2），推测可以。
- **ureq 能不能直接走 unix socket：** 没查。不能的话，做法四的 pen 要自己写一小段 HTTP over unix socket（推测一两百行），或者走 127.0.0.1 加令牌。
- **fence 包执行器：** `fence run` 能不能直接包一个不是 agent 的执行器子进程，还是要小改 fence；srt 包一个长时间运行、反复起子进程的执行器有没有问题。没读 fence 的代码，没实测（§7.2）。
- **常驻服务端的几处做法：** 排空交接、会话用客户端的环境、连接租约、按会话暂停，都是我参照 Codex、opencode 推出来的，两家都没有现成的排空交接（Codex、opencode 都是直接重启或替换）。工作量和坑要等做的时候再看。
- **cairn 怎么接各家：** 没查，本轮范围外。
- **rig 的 coding agent：** 没细读（主控 10-23：先不细读）。
- **omp hashline 的提升数据：** 是 omp 自己的测试，没验证。
- **Codex 的 WebSocket 传输：** 官方标成实验性、不保证（W6），作为参照没问题，作为依据要小心。
- **工作量：** 全是推测。

---

## 出处

### 本地读的源码（2026-10-23 克隆到 `/tmp/r49-src/`，只读；本机装的程序只看了安装目录）

- **L1** Codex：<https://github.com/openai/codex>，标签 `rust-v0.163.0`（提交 `83086188`，2026-10-15）。本机 `/opt/homebrew/Caskroom/codex/0.163.0`。主要读了：
  - `codex-rs/app-server/README.md`
  - `app-server-protocol/src/protocol/common.rs`、`v2/{item,thread,turn,notification}.rs`
  - `app-server-transport/src/transport/{mod,websocket}.rs`、`websocket-auth/src/lib.rs`
  - `app-server-daemon/README.md`、`exec-server/README.md`、`exec-server-protocol/src`
  - `app-server/src/{outgoing_message.rs,request_processors/thread_lifecycle.rs}`
  - `tui/src/{lib.rs,insert_history.rs,custom_terminal.rs}`
  - `core/README.md`、`core/src/{compact.rs,tools/handlers/}`
  - `apply-patch/src/parser.rs`、`prompts/templates/compact/`、`rollout/src/list.rs`
  - `model-provider-info/src/lib.rs`、`protocol/src/{protocol.rs,config_types.rs}`
  - `Cargo.toml`、`LICENSE`、`NOTICE`
  - 行数用 `scc` 统计（不含 `vendor`）。
- **L2** opencode：<https://github.com/anomalyco/opencode>，标签 `v2.1.3`（提交 `bb25a2f4`，2026-10-22）。本机 `/opt/homebrew/Cellar/opencode-v2/2.1.3`。读了：
  - `V2_HTTP_API_AUDIT.md`
  - `packages/cli/src/{server-process.ts,commands/handlers/default.ts,services/{server-connection,standalone}.ts}`
  - `packages/client/src/effect/service.ts`
  - `packages/server/src/{auth,pairing,event-feed}.ts`
  - `packages/core/src/{tool/,session/,database/}`、`packages/ai/README.md`、`packages/tui/package.json`
- **L3** pi：<https://github.com/earendil-works/pi>，标签 `v1.4.0`（提交 `4a080252`，2026-10-22）。本机 `~/node_modules/@earendil-works/pi-coding-agent` 1.4.0。读了：
  - `packages/agent/{README.md,src/agent-loop.ts,src/agent.ts}`
  - `packages/coding-agent/docs/{how-pi-works,compaction,rpc,security,providers,usage}.md`
  - `src/core/tools/`
- **L4** omp：<https://github.com/can1357/oh-my-pi>，标签 `v18.12.6`（提交 `45c35e40`，2026-10-21）。本机 `/opt/homebrew/Cellar/omp/18.12.6`。读了：
  - `README.md`、`docs/{tools/edit,approval-mode}.md`
  - `packages/{wire,collab-web}/README.md`

### 网页（都是 2026-10-23 查看）

- **W1** OpenAI，Sign in with ChatGPT 开发者文档合集：<https://developers.openai.com/siwc/llms-full.txt>（Quickstart、ChatGPT plan usage overview、Registration and sign-in、Models and inference、Preview limitations、Codex app-server 各节）
- **W2** <https://developers.openai.com/siwc/token-sharing-open-source>、<https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations.md>、<https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference.md>、<https://developers.openai.com/siwc/token-sharing-open-source/sign-in.md>
- **W3** OpenAI，Sign in with ChatGPT Terms：<https://openai.com/policies/sign-in-with-chatgpt-terms/>；帮助中心「Using your ChatGPT plan in other apps and sites」：<https://help.openai.com/en/articles/20001542>。两页都返回 403，内容来自搜索摘要。
- **W4** OpenClaw PR #148567「preview ChatGPT token sharing through Responses」，2026-09-14 开、09-24 合并：<https://github.com/openclaw/openclaw/pull/148567>
- **W5** sakimura.org「OpenAI Launches Sign in with ChatGPT」（二手，机器翻译草稿）：<https://www.sakimura.org/en/2026/10/11148/>
- **W6** Codex app-server 文档：<https://learn.chatgpt.com/docs/app-server>（由 <https://developers.openai.com/codex/app-server> 308 跳转）
- **W7** ACP 介绍：<https://agentclientprotocol.com/overview/introduction>
- **W8** ACP 协议概览：<https://agentclientprotocol.com/protocol/overview>
- **W9** ACP RFD 进度：<https://agentclientprotocol.com/rfds/updates>；传输工作组：<https://agentclientprotocol.com/announcements/transports-working-group>
- **W10** Zed，「Bring Your Own Agent to Zed — Featuring Gemini CLI」，2025-08-27：<https://zed.dev/blog/bring-your-own-agent-to-zed>
- **W11** Ollama OpenAI 兼容：<https://docs.ollama.com/api/openai-compatibility>
- **W12** vLLM 在线服务：<https://docs.vllm.ai/en/latest/serving/online_serving/>
- **W13** llama.cpp server：<https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md>
- **W14** crates.io 接口：`https://crates.io/api/v1/crates/<名字>`（版本、总下载、近期下载、更新时间）
- **W15** GitHub 组织仓库列表：<https://api.github.com/orgs/openai/repos>、<https://api.github.com/orgs/anthropics/repos>（按更新时间取前 100 个）
- **W16** rig：<https://github.com/0xPlaygrounds/rig>（README、LICENSE）
- **W17** async-openai README：<https://github.com/64bit/async-openai>；genai README：<https://github.com/jeremychone/rust-genai>
- **W18** axum `Listener`：<https://docs.rs/axum/0.8.9/axum/serve/trait.Listener.html>
- **W19** GitHub 仓库接口（星数、许可证）：`https://api.github.com/repos/<仓库>`，含 openai/codex、anomalyco/opencode、earendil-works/pi、can1357/oh-my-pi、0xPlaygrounds/rig、ratatui/ratatui
