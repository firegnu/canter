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
7. **名字和仓库**：`canter`（马的「慢跑」步法），仓库 `github.com/firegnu/canter`，公开，Apache-2.0。用户：「开源，repo就放在github上」。
8. **协议：结构照 Codex，传输和类型 canter 自己定（报告 §5.7 的 C）**。2026-10-24 在 canter 仓库定的，用户：「好，就用 C，改 DESIGN」。
   - 经过：用户先说「协议我觉得做好使用成熟的现有的coding agent使用的协议」，选了 Codex app-server 协议，不用 ACP（「ACP我一直不是很看好」）。10-23 用户又提醒「我们选的 Codex app-server 协议是 OpenAI 自家的协议，不是公共标准，会跟着 Codex 版本一起变」。可选的有两条：A 是以 Codex 某版为底拷一份、自己管；C 是自定 HTTP + SSE（报告 §5.7）。最后定了 C。
   - 传输：客户端用普通 HTTP 请求下命令；服务端用一条 SSE 长连接按顺序推事件，每条带递增序号，断线后从序号接着收（报告 §5.2 的甲，传输学 opencode）。
   - 结构：照 Codex app-server v2，会话 → 一轮 → 一条条项目；批准、提问是服务端等客户端回答的请求；事件分开始、增量、完成。字段名尽量和 Codex 一样。只参考，不拷 Codex 的协议代码；参考的版本写进任务文件。
   - 协议类型放单独的 Rust 包，从它生成 JSON Schema 和 TypeScript 类型；agent 核心和协议之间隔一层转换。
   - 理由：A 拷过来、不跟上游以后，同样是 canter 自己维护的协议，和 C 只差传输方式和拷不拷类型代码。传输上 HTTP + SSE 更贴合 canter：一台机器多个客户端，断线能接着收，以后 corral、pony 这类同步程序直接接，curl 就能排错。
   - 代价：离用户原来要的「使用成熟的现有的coding agent使用的协议」远一步，用的是 Codex 的结构，不是它的协议本身；协议包要自己写，估 1,000–1,500 行（报告 §9.1）。
9. **桌面应用用 GPUI**（和 paddock 一样）。
10. **第一版范围**：照报告「要用户定的」第 6 条的推荐，另加 DeepSeek（第 6 条）。
11. **服务端怎么起、怎么升级**：照报告第 7 条的推荐：客户端找不到服务端就在后台起一个，另可装成用户服务；升级第一期等在跑的轮跑完（有上限）再重启、不自动换版本，第二期接 corral 派活前做排空交接；嵌在终端里的单机模式只作备选。
12. **接 corral 往后放**：「corral的接入等agent差不多可以工作了（tui版本的），再考虑接入。」

## 和报告不同的地方

- **协议**：最后和报告 §5.2 的推荐一样，自定「HTTP 发命令 + SSE 收事件」（甲）。中间用户一度定了 Codex app-server（乙），10-24 改成 C，即甲的传输加 Codex 的结构（第 8 条）。报告 §5 里讲乙、讲 WebSocket 的部分不再适用；讲数据结构（会话 → 一轮 → 项目）、待处理请求、断线续传的部分照用。
- **模型**：报告推荐第一期只接 Responses 接口；用户要第一期就接 DeepSeek，所以第一期同时有 Responses 和 Chat Completions 两种接口，模型接入层一开始就按多家来做。
- **接 corral**：报告 §6、§9.2 把接 corral 排在第二期；用户定为终端版本差不多能用之后再考虑，到时再定做法（报告「要用户定的」第 9 条的五种做法留作参考）。

## 还没定的

- 第一期怎么拆成具体的活（报告 §9.1 推测 7 件左右）。
- 从 Codex 搬哪些代码（比如 `apply-patch`；协议类型不搬，见第 8 条），搬来的代码要保留 Apache-2.0 声明，按需加 NOTICE。
