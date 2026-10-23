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
8. **协议用现成的、成熟的 coding agent 协议**：「协议我觉得做好使用成熟的现有的coding agent使用的协议」。选定 **Codex app-server 协议**（JSON-RPC；会话 → 一轮 → 一条条项目；批准、提问是服务端发给客户端的请求），走 WebSocket 让多个客户端同时连。不用 ACP（用户：「ACP我一直不是很看好」）。
   - **待讨论**：用户 10-23 提醒「我们选的 Codex app-server 协议是 OpenAI 自家的协议，不是公共标准，会跟着 Codex 版本一起变」。是以 Codex 某版为底拷成 canter 自己的协议、不跟上游（A），还是改回自定的 HTTP + SSE（C），见调研报告 §5.7，「我之后会在那个repo中讨论」。定了改这一条。
9. **桌面应用用 GPUI**（和 paddock 一样）。
10. **第一版范围**：照报告「要用户定的」第 6 条的推荐，另加 DeepSeek（第 6 条）。
11. **服务端怎么起、怎么升级**：照报告第 7 条的推荐：客户端找不到服务端就在后台起一个，另可装成用户服务；升级第一期等在跑的轮跑完（有上限）再重启、不自动换版本，第二期接 corral 派活前做排空交接；嵌在终端里的单机模式只作备选。
12. **接 corral 往后放**：「corral的接入等agent差不多可以工作了（tui版本的），再考虑接入。」

## 和报告不同的地方

- **协议**：报告 §5.2 推荐自定协议「HTTP 发命令 + SSE 收事件」（甲）；用户定了用现成协议，选 Codex app-server（报告里的乙）。报告 §5 里关于数据结构（会话 → 一轮 → 项目）、待处理请求、断线续传的分析照样有用；corral、pony 以后接入时要能连 WebSocket，这是选乙的代价（报告 §5.2）。
- **模型**：报告推荐第一期只接 Responses 接口；用户要第一期就接 DeepSeek，所以第一期同时有 Responses 和 Chat Completions 两种接口，模型接入层一开始就按多家来做。
- **接 corral**：报告 §6、§9.2 把接 corral 排在第二期；用户定为终端版本差不多能用之后再考虑，到时再定做法（报告「要用户定的」第 9 条的五种做法留作参考）。

## 还没定的

- 协议的来源：A 以 Codex 某版为底、自己管，还是 C 自定 HTTP + SSE（DESIGN 第 8 条、调研报告 §5.7）。两边的建议不同：ranch 主控推荐 A，canter 主控倾向 C，各自的理由见报告 §5.7 末尾。
- 第一期怎么拆成具体的活（报告 §9.1 推测 7 件左右）。
- 从 Codex 搬哪些代码（`apply-patch`、协议类型等），搬来的代码要保留 Apache-2.0 声明，按需加 NOTICE。
