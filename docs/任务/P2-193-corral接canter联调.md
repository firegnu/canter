# 任务：第二期-26.4 - corral、paddock、pony 接 canter 联调

2026-11-03，主控建（占号）。
依赖：P2-83

## 用户原话

- 见 `docs/任务/P2-83-全部第二期做完之后，接入corral和paddock.md`「用户决定（11-03）」。
- 11-03 canter 三件（P2-190、P2-191、P2-192）合并、用户装上 `16fb98c` 后，用户：「我觉得吧。现在paddock和ranch的主控都开着呢。你可以在他们里面建立任务，然后送话给他们，让他们做，你觉得如何？」主控说明这和 CLAUDE.md「不送话给用户的 agent、不改别的仓库」冲突，建议只这一件破例：需求单放 `/tmp`，不往别的仓库写文件，只送一句话让对方照自己的规矩建任务。用户：「同意，先写需求单。不要改claude.md」。
- 写需求单时用户补：「如果其他agent委派出canter，要使用--yolo开出。」主控问 paddock 新建要不要也默认 `--yolo`，用户：「paddock 也加，送出去吧！」

## 需求单（11-03 送出）

- `/tmp/canter-p2-193/ranch-需求单.md` → `ranch/main`（corral、pony、派活分工表，10 条）：开并跟住当前会话；状态从快照和事件流来，订阅放 corral 里 canter 的接入文件、公共代码只加通用挂点；reply、问题、待批准命令；送话、按键、attach、read 不变；`stop` 真正停服务端上的工作（可用 `--close-on-exit` 或关会话接口）；pen 升级和 canter 服务端升级不误判；暂停、fence 报不支持；`integration status`；pony「拒绝」「打断」按一次 Esc、认 `canter` 标题；dispatch 表照 Kimi 只点名时派，委派出的 canter 一律 `--yolo`。
- `/tmp/canter-p2-193/paddock-需求单.md` → `paddock/main`（5 条）：新建能选 canter、默认 `--yolo`；终端接管和 shell 自动接管认 canter；图标；暂停按能力如实显示不支持、fence 只列支持的、自带沙箱和 fence 分开写；Integrations 页。
- 两份都写明：这一版不做暂停、fence、pony「允许」、远程批准；canter 不反向依赖，要 canter 改的写成问题交用户；测试用隔离 `CANTER_HOME` 和假模型；最后用户在场联调。
- 送话前确认两个主控都 idle、没人 attach、没有人工输入；各送一句，只告诉需求单在哪和依赖顺序，不替它们拿主意。之后对它们照旧只读。

## 进展（11-03）

- ranch 部署 corral 接 canter（ranch `5f065af`，R56b + R56c）；`corral integration status` 列出 canter（`hooks:launch`，`unsupported:[pause,sandbox]`），派活表 canter 格是 `canter --yolo`。公开约定见 ranch `docs/CONTRACT.md`「canter（R56）」。
- paddock 做完需求单 1～5 条（P5-195、P5-196、P5-197），已合并安装。
- paddock 冒烟联调（用户在场，paddock/main 主持）全部通过：新建 canter（`canter --yolo`）、图标、点信任后 starting 转 idle、发话 working 再回 idle、reply 取得到、暂停按钮和菜单 Pause 灰掉、Integrations 显示 Built in、停止时 corral 报 exiting 后消失（这次没剩会话，没出提示）。
- ranch 托看的两件（`/fork` 之后 corral 报的状态、子 agent 批准框关掉后回到 idle）：用户决定这次不测，留给以后。
- ranch 指出 canter 协议 README 缺三处（`/v1/info` 字段、子 agent 请求的字段形状、`session.status` error 的含义），要不要开 P2-196 补，待用户定。
- paddock 补充：派活主流程用 canter 走通。`corral start` 开 `canter --yolo`（带 `--prompt`），在 worktree 里改文件、提交两轮；第二轮用 `corral send` 派，`send --after` 提醒和后台 `corral wait` 按时叫醒主控，reply 取到 DONE。`stop` 返回 `remaining_sessions:null`，paddock 按空处理（ranch CONTRACT 写的是全关了不带这个字段，两者不一致，属 ranch 那边，不挡）。
