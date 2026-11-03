# P2-83：接 corral 和 paddock 调研

2026-11-03。只读本机源码与文档：canter `6b88db9`、ranch `7cbebe7`、paddock `7a41856`。ranch 工作树干净；paddock 有两份未跟踪任务文件，本报告没有使用或修改。下文「查到」指静态证据，「建议／推测」不是已经实现或实测通过；引用编号的文件、行号见末尾。

## 结论

- **还不能直接当作已接入。** corral 没有 canter 适配器；当普通程序启动时没有结构化状态，`--prompt` 也会被拒绝。paddock 的新建种类名单同样没有 canter。〔R1、R2、P2〕
- **环境已有一半以上：** 新建／恢复协议已经带客户端环境，模型命令已经使用；缺的是钩子仍继承常驻服务端的环境。应补钩子执行这一段，不必重做环境传输。〔C1、C4〕
- **P2-172 不等于停止能力。** `SessionEnd(client_exit)` 不关会话、不打断工作；corral 当前 stop 只确认 pen／客户端进程退出，可能留下仍在服务端执行的工作。〔C5、R5〕
- **建议先做 R49 做法四的最小范围：保留 TUI，corral 读公开快照＋SSE，停止走会话关闭接口。** 暂停、fence 档案先明确不支持；自带沙箱照实际配置工作。做法一仍可选，但今天要补的已不只是一个 adapter。〔C2～C7、R3～R6；方案判断〕

## 1. 两边要什么，canter 现在给什么

canter 当前是本机 Unix socket 上的 HTTP＋SSE：`<CANTER_HOME>/server.json` 记 `pid/version/protocolVersion/socketPath/instance`，发现后用 `GET /v1/info` 核验版本与能力；没有服务端时由 CLI 启动。接口已经有仓库文档，外部长期依赖的发现／绑定约定仍应在接入设计中明确。本轮只读这些定义，没有读取实际登记文件。〔C2、C7〕

| 功能 | corral／paddock 当前要求（查到） | canter 当前公开能力与是否够用（查到；接入判断另标） |
|---|---|---|
| 开、名字、标签 | `start NAME --cwd DIR --env K=V --label K=V --prompt TEXT -- COMMAND`；名字和 instance 属于 pen，labels 自己保存。没有统一的「免确认」开关，免确认参数属于 agent。〔R1、R2〕 | `canter [选项] -- <首句>`、`--tag K=V`、`--yolo`、`-m`、`--add-dir` 已有；HTTP 创建有 `name/tags/cwd/env/input/clientName`。CLI 没有 `--name`，TUI 用首句自动起名，`/rename`／PATCH 可改。**够启动；两边名字、标签不会自动同步，名字也不能代替会话 ID。**〔C1、C2、C8〕 |
| 首句、后续送话 | 默认把首句放 argv 的 `--` 后；`send` 经过 PTY，检查空闲、人类输入和接收确认。paddock 收件箱也用 send。〔R1、R2、P1〕 | 新建带首句可用；交互 `--resume ID` 不接受首句，恢复后需输入。HTTP `POST .../input` 支持 queue／steer；`canter exec` 是非交互单轮，不能替代长期 TUI。〔C1、C2、C9〕 |
| starting／working／idle／blocked | 事件状态机：SessionStart→idle、UserPromptSubmit→working、PermissionRequest→blocked、结束→idle；首句尚未被接收时保持 starting。还会有 unknown／exiting。〔R2、R3〕 | 协议有 starting／idle／working／waiting_approval／error／closed；有 turn.started/completed、request.opened/resolved、后台命令和目标事件、快照及 runningTurns 查询。**数据基本齐；需适配状态，不能只见一次 Stop 就把目标续跑、后台工作当做全部结束。**〔C2、C3、C10〕 |
| 中断、回复 | Interrupt 后仍 idle，但 `interrupted:true`；wait 返回 interrupted，reply 报 no_reply，不能拿上一轮回复充数。正常 reply 给 text／at。〔R2、R4〕 | turn.completed 带 completed／interrupted／failed；轮中保留 agentMessage；Stop 钩子有本轮最后助手消息，Interrupt 已有。协议足够；错误结束与正常回复须分别映射。〔C3、C4〕 |
| 提问、批准内容 | `questions` 为题目／选项，`command` 为 shell 原文，均最多 16 KiB 并标 truncated。实现仍有工具名和种类限定；不是任意 agent 都自动得到完整批准内容。现有操作主要通过终端／keys。〔R3、R4〕 | PendingRequest 有请求 ID、轮 ID；userInput 带 question/options，approval 带 tool/arguments/availableDecisions，sandboxApproval 带范围，planApproval 带计划；POST requests/{rid} 可回答。主流还转发子会话请求。**协议够用；直接接现有钩子不够，见 §2。**〔C2、C3、C4〕 |
| 停止 | pen 执行 adapter 的按键／信号序列，最后 KILL；stop 成功等 pen socket／锁释放，再回退出记录。〔R5〕 | `DELETE /v1/sessions/{id}` 已能取消轮、顺口问、建议请求，停止后台命令并收尾；TUI 收 HUP／TERM 是退出客户端。**缺外部实例与会话的绑定及可靠关闭流程；不能用杀 TUI 代替。**〔C5〕 |
| 暂停、继续 | pause 冻结 agent 进程树，paused 与 state 分开；send／keys 拒绝，stop 先解冻。paddock 调相同命令。〔R5、P3〕 | 只有目标的 pause/resume，不是整个会话的冻结；没有暂停所有模型请求、工具、后台进程的公开接口。**不够；不能冻结共享服务端，也不能只冻 TUI 后声称暂停成功。**〔C2、C10；架构推断〕 |
| 套墙 | `--sandbox PROFILE` 通过 fence check/run 包住整条启动命令；可从父实例继承 child-sandbox；错误不能退成无墙。〔R6〕 | 已有 macOS Seatbelt 模型 shell 沙箱及文件工具边界；`--yolo` 只免批准、仍保留沙箱。没有逐会话沙箱启动参数或 fence 档案映射。`!`、MCP、网页抓取和钩子不在该模型沙箱内。**不能把自带沙箱宣称为 fence dev。**〔C6〕 |
| 升级 | `corral upgrade` 换 pen／helper，保留 agent PID、PTY、连接、instance；不升级模型程序。〔R7〕 | `serve --replace`、upgrade／rollback、排空和 TUI 重连已有。**这两个升级边界可以分开；新增事件订阅和绑定必须随 pen 升级恢复，不能丢游标或误触发关闭。**〔C7；后半为接入要求〕 |
| 技能 | adapter 声明读取哪些安装位置；integration status 提供程序、版本、接法和技能状态。`codex` 这个 target 实际指共享 `.agents/skills`。〔R1、R8〕 | 读 `~/.agents/skills` 和项目根 `.agents/skills`，HTTP 可列技能。位置已匹配；只需外部登记，技能内容是否应增加 canter 示例另在对应仓库处理。〔C8〕 |
| paddock 另外要的 | 列表读 ls＋status；终端用 attach；收件箱读 reply／send；按 labels 的 role/effort 显示；新建名单和 shell 自动接管名单写死七家。Integration 页按报告动态列；暂停错误 unsupported 目前会显示 needs corral upgrade。〔P1～P4〕 | 只要 corral 保持这些公开格式，基本列表／终端不要求 canter 专用协议。新建入口、shell 接管、图标及暂停／沙箱说明仍须 paddock 改；**不能说加完 corral adapter，paddock 所有按钮就全好了。** |

## 2. hooks、环境与 R49 的变化

### 2.1 现有钩子能拿到什么

查到：钩子只从全局 config.json 读取，没有 CLI／创建请求的单次 hooks 覆盖；共同字段是 `session_id/cwd/hook_event_name/permission_mode`，无 transcript_path。受限会话禁用全部钩子。〔C4〕

| 事件 | 字段和时机（查到） | 接入边界 |
|---|---|---|
| SessionStart／SessionEnd | Start：新建 startup、从盘恢复 resume、离开已 End 后重连 resume。End：client_exit／client_request／server_shutdown。〔C4、C5〕 | 不是进程启动／退出的等价事件；恢复已经打开的会话不重新绑定环境或保证再发 Start。 |
| UserPromptSubmit | prompt；用户输入开始消费前，可拦；自动叫醒轮不触发。〔C4〕 | 不能作为所有轮的 working 信号；预检还可能拒绝输入，不能等同可靠的「已交付模型」。 |
| PreToolUse／PostToolUse | tool_name/tool_input；Post 另有 tool_response。前者规则检查后、批准框前；后者实际执行完成后。〔C4〕 | 工具名是 shell／ask_user 等；拒绝、预检失败没有 Post。 |
| PermissionRequest | tool_name/tool_input；普通工具批准与计划批准，通知、不代答。〔C4〕 | ask_user 实际等待直接发 request.opened＋waiting_approval，**没有这一钩子**；不能靠 PreToolUse 猜它已经打开框。〔C4：runner.rs:1530–1598〕 |
| Stop／StopFailure／Interrupt | 正常末条助手回复／错误／无额外字段。〔C4〕 | 通知异步启动，缺轮 ID、请求 ID、事件序号；静态看不保证写入外部事件文件的先后顺序。 |
| SubagentStart／SubagentStop | agent_id、agent_type、parent_session_id、task 或 status/last_assistant_message；子钩子另有 agent_name。〔C4〕 | 子会话没有普通 SessionStart/End/Stop/Interrupt；子工具钩子会运行。适配必须区分主／子，不能让子工具事件抢主状态。 |

**直接复用 helper 的三处问题（查到）：** 普通 `__hook EVENT` 用 transcript_path 是否非空生成 has_transcript，状态机用它和 cwd 收主会话；canter 没有该字段。带种类的 `__hook kind:EVENT` 还核验 hook 的进程祖先是 pen，常驻服务端发出的 hook 不满足这个关系。questions／command 的抽取认 AskUserQuestion／Bash，公开 command 路径还限制 Claude（Kimi 另走专用数据）。所以只补环境再登记名字，仍不能接好；应由外部适配公开会话身份与载荷，不在 canter 伪造 transcript。〔R1、R3、R4〕

### 2.2 「会话带上客户端的环境」具体还差什么

- **已有：** CLI 收集 UTF-8 环境；create 和 resume 的 env 到服务端私有内存；已经打开的会话保留原环境，从盘重开才采用本次 env；执行器清空继承环境后使用会话环境。环境不存盘、不进公开快照和事件。〔C1、C2〕
- **建议：** 钩子也使用同一份会话环境，以 `env_clear + 会话 env` 为基础，再由程序覆盖 `CANTER_SESSION_ID`、`CANTER_PROJECT_DIR`；cwd 仍用会话目录，不注入模型的临时目录。不按变量名前缀写第三方特例，不把整个服务端环境混回去。原来受限会话禁 hooks 的约束保留。〔现有差口：C4，方案建议〕
- **接入方注意：** CORRAL_NAME/INSTANCE/EVENTS/HOME 由 pen 构造，KIND/HELPER/PEN_PID 目前只给 Global adapter。选择 launch 接法时不能假定后两项一定存在；钩子命令可以用外部提供的稳定绝对 helper 路径。重新连接一个别人早已打开的会话不会替换环境，首版宜新建受管理会话；不要为绑定强改其环境。〔R1、R2、C2〕

### 2.3 P2-172 是否足够判断退出

**不够。** 最后一条会话 SSE 断开后等 10 秒；仍工作则继续等，等批准／回答却算停下；发 End 后会话仍可恢复。已经发过 client_exit，之后显式关闭或停机不再发第二次 End。只观察钩子既可能迟迟收不到，也可能在尚有待答工作时收到。若 pen 自己一直订阅 SSE，TUI 退出甚至不会满足“最后一个客户端离开”。〔C5〕

建议保持 P2-172 原义；另外提供可选的受管理会话生命周期：明确哪个客户端拥有关闭责任，显式关闭调用 DELETE 并确认结果，异常离开由独立于普通观察订阅的租约收尾。普通终端默认仍可退出后留会话。多客户端、会话切换、关闭范围需先定，不能直接把现有 10 秒通知改成自动杀会话。〔方案建议；现有关闭基础 C5〕

### 2.4 对照 R49 §6.1～6.6

| R49 的设想 | 现在的核对结果 |
|---|---|
| 一个 agent 是 PTY 中的进程树，靠 adapter／hooks 集成 | corral 仍如此；现在已有 Kimi，题目也不再只有 Claude 能报。以当前登记和 CONTRACT 为准。〔R1、R4〕 |
| 通用 hooks、名字、标签、客户端环境要建设 | 都已有基础；hooks 的环境、单次配置和观察完整性尚未齐，CLI 无名字参数。〔C1、C2、C4〕 |
| 退出即关闭＋租约 | 还没有；P2-172 只补通知，不能顶替。〔C5〕 |
| 状态／回复／提问可由公开事件提供 | 已实现；还增加了后台、目标、子会话请求等信息，适配不能只接最初几个事件。〔C2、C3、C10〕 |
| 做法三优先执行器前缀，自带沙箱留备选 | P2-123 已明确选择自带 Seatbelt。无需为接入倒回前缀方案；但 fence 的整体围栏、档案与当前自带沙箱仍是不同能力。〔C6〕 |
| 不做逐目录信任；升级要有排空 | 后续已定 P2-168 信任流程，worktree 共用主仓库信任；`--yolo` 不跳过它。排空／升级已有。这里是后续已定设计取代旧建议，不是待猜的设计意图。〔C7、C11〕 |

## 3. 差距清单

### 3.1 canter 自己的通用能力

大小为静态估计：小＝局部接线；中＝涉及 CLI／协议／核心协同；大＝执行与恢复生命周期。不沿用 R49 的行数估算。下面只列能力，不引入第三方名字或状态目录。

| 项 | 要做什么 | 为什么／优先级 | 估计 |
|---|---|---|---|
| A1 钩子会话环境 | 按 §2.2 补齐钩子的环境传入，覆盖程序保留变量，保持内存私有与现有恢复规则。 | 已明确留下的工作；外部管理器身份、PATH、用户工具配置须属于本会话。〔C1、C4〕 | 小 |
| A2 可机器读取的客户端绑定 | 明确可依赖的服务发现约定，公开客户端运行实例与当前会话 ID 的关联及切换通知，含 `/new`、恢复、分叉、交接后的变化；固定已知 ID 可先复用 POST 创建＋`--resume ID`，但不能据此声称已有完整切换通知。 | 外部管理器才能订对事件、关对会话；仅按名字、目录、继承标签搜会话有歧义。推荐接法的前置；形式待设计。〔C1～C3、C7、C8〕 | 中 |
| A3 可选的关闭责任和异常收尾 | 明确开启此模式的客户端所拥有的会话；正常退出确认关闭，崩溃／强杀按租约取消并收尾；普通观察连接不延长所有者租约，其他会话不受影响。复用现有 close，不重写执行器。 | 外部 stop 必须停实际工作；仅 HUP／TERM、P2-172 通知不够。需先定 §5 的关闭范围。〔C5〕 | 大 |
| A4 单次会话 hooks（备选路线才做） | 支持显式启动／建会话时附加 hooks，定义与全局配置的合并、重连和子会话继承；仍受信任约束。 | 让调用方只接自己开的会话，避免为一次运行改全局 JSON。当前只有全局 hooks。〔C4〕 | 中 |
| A5 完整的钩子观察契约（备选路线才做） | 若仅靠 hooks 判状态，补实际请求打开／解决、所有轮开始、主子来源及有序标识；不能把可拦输入事件当作最终接收确认。 | 提问、拒绝后继续、自动续跑和异步迟到事件都需正确显示。推荐路线直接用已有事件流，可不扩张 hooks。〔C3、C4〕 | 中 |
| A6 整会话暂停／继续（后续） | 管模型请求、工具调度、前后台命令、自动续跑、建议请求和子会话；定义恢复行为。 | 目标暂停不等于冻结；已有网络请求无法保证远端停止计算。首版不做。〔C2、C10；范围为建议〕 | 大 |
| A7 会话级沙箱选择（后续） | 若需每个启动单独要求沙箱，增加显式参数／协议设置与实际能力回报；保持起不来不降级，区分执行范围和平台。 | 当前全局开关无法承诺某次启动必开；不能为一个会话切全局配置。首版沿用已配置的自带沙箱，不实现外部档案。〔C6〕 | 中 |

### 3.2 交给 corral、paddock 的需求

| 归属 | 需求与理由 |
|---|---|
| corral：登记与启动 | 加 canter adapter、登记版本探测和共享技能 target；保留 name／instance／labels。把首句、权限、模型和标签按 canter 的真实参数／协议传递，取得明确会话 ID。已有适配器是 Claude 的 settings、Codex 的 -c、pi 的 extension 三种，不能照搬其中任一种参数。〔R1、R2、C1〕 |
| corral：状态来源 | 推荐让 pen 订 canter 快照＋SSE；按持久 seq 补发／去重，seq 为 0 的即时建议不推进游标；按 session／turn／request ID 隔离。事件 timestamp 是毫秒，要转成现有秒格式；Turn.startedAt/completedAt 已是秒，不能重复换算。处理真实 input 接收、自动轮、主子请求、结束／中断／错误、后台与目标续跑；unknown 不能当 idle。扩展 adapter 的通用状态来源能力，不能要求 canter 写事件私有文件。〔C2、C3、C10、R3〕 |
| corral：请求内容 | 把 userInput 映射成 questions、shell 批准映射成 command；明确 sandboxApproval／planApproval 如何显示，保持截断标志。去掉目前阻止 canter 输出这些字段的种类／工具名假设；缺完整信息时不得猜批准。首版仍在 TUI 作答，结构化 approve／answer 可后做。〔R3、R4、C3〕 |
| corral：生命周期 | stop 先关闭所拥有的实际会话并确认，再完成 TUI／pen 退出；失败不能仅凭 TUI 消失报“工作已停”。暂停和显式／继承 fence 档案先返回该能力不支持；不要把整个常驻服务端算进可杀／可冻进程树。A3 租约覆盖管理进程自身消失的情况。〔R5、R6、C5〕 |
| corral：hooks 备选 | 若用户选做法一，须有单次 hooks 接入、明确主会话绑定与主子过滤、工具字段翻译、时序处理；全局 helper 的父进程校验需按公开绑定另设计，不能简单取消校验。新增内部文件仍须进入 FILES。〔R1、R3〕 |
| corral：升级与恢复 | pen 升级快照保留会话绑定、租约责任、SSE 游标和待操作；服务端替换时重连，但不要把 server_shutdown 当用户停止，不自动重发首句或把历史回复报成新回复。〔R7、C5、C7〕 |
| corral：公开能力和使用入口 | 对外说明支持的状态来源、停止、暂停、沙箱；integration status 不能把不存在的 hooks 安装描述成已装。更新 corral／dispatch 的调用说明与分工数据；不把暂停不支持伪装成“升级 pen 即可”。〔R1、R8、P3〕 |
| paddock：新建／终端 | 新建 Tool、预设／命令识别、图标和 agent_shell KINDS 增加 canter；初期模型可沿用 canter 配置，若提供选择则读取公开模型接口。列表、reply、send、attach 继续走 corral，不直接读 canter 或 corral 状态目录。〔P1、P2、P4〕 |
| paddock：能力显示 | 暂停按钮按能力禁用，并修正 unsupported→needs upgrade 的误导；fence 选择器只列真正支持的档案。若展示自带沙箱，要区分“模型执行沙箱”和“整个进程的 fence 档案”，不能仅画同一把锁。〔P1～P3、C6〕 |
| paddock：已有功能边界 | Integration 页能动态多一行，但新的状态来源文案要跟进；当前列表模型没有 questions／command／interrupted 字段，若要专门显示需另接。基本终端可回答请求，不必为最小接入新增批准按钮。〔P1、P4〕 |

## 4. 推荐做法与次序

| R49 §6.4 做法 | 按现状的判断（建议，不是已定实现） |
|---|---|
| 一：adapter＋通用钩子 | 可作备选，但要 A1～A5 及 helper／状态适配。现有事件重名不代表语义、主会话识别、等待和顺序已兼容；不再按旧报告的“加一份短 adapter”估算。 |
| 二：生命周期走会话命令 | **停止部分现在就用**：已有 DELETE。不必先给每个 HTTP 接口包一条 CLI；暂停／继续 A6 后做。 |
| 三：执行器前缀／自带沙箱 | 已选自带沙箱，保持它；暂不建设 fence 执行器前缀，也不把 dev 档案静默翻译成另一套规则。 |
| 四：pen 订事件流 | **推荐先做最小版**：保留 PTY／TUI，只替换状态来源并接关闭；复用已有完整、有序、可重放的公开数据。代价是 corral 共用层要新建协议客户端及升级恢复，不能说完全不动核心。 |
| 五：完全不要终端 | 本轮不推荐；会牵动 attach／read／keys 和 paddock 终端，当前没有收益足以抵消这些改动。 |

推荐顺序：

1. 用户先定 §5 的路线和关闭范围，再拆实现任务；A1 是既定缺口，可先单独补。A2 的绑定形式、A3 的租约和切换语义写入 DESIGN 后实施。
2. corral 接 adapter＋明确绑定＋快照/SSE＋实际会话关闭，先在已有信任记录的仓库使用。保留原 TUI 信任流程；`--yolo` 不自动记录信任，未完成信任时不能虚报已经接收首句。〔C11〕
3. **到这里才具备日常派活的候选闭环：开、状态、回复、停。** 落地时还需隔离假服务／假模型验证上述四项，包括打断、不遗留工作、重连／升级不重发。此处是建议后续实现的验证范围，本调研没有运行这些检查，也没有追加本轮验收。
4. paddock 接新建与能力显示；基本列表／终端复用现有命令。后续需要整会话冻结、逐会话沙箱策略或按请求 ID 远程作答时，再分别做二的暂停部分、三的能力映射、四的 approve／answer。

## 5. 要用户／主控拿主意的事

这些是实现前的选择，本轮只提出，不替用户定、不开始实现。

| 问题 | 可选办法 | 建议 |
|---|---|---|
| 第一版状态来源 | A：补齐单次 hooks 与观察事件；B：保留 TUI，pen 读快照/SSE。 | **B**；现有协议比 hooks 完整，减少两套状态事件长期对齐。代价是外部管理层改动较大。 |
| stop 和异常退出的会话归属 | A：只显式 stop 时关闭，其他退出留会话；B：可选受管理模式，所有者退出／失联时也收尾。另需定切换会话后关“当前”还是“本次拥有的全部”。 | **B，关闭本次明确拥有的会话，不接管其他客户端已有会话**；观察订阅不算所有者。这样不会留下未被管理的工作，普通启动语义不变。具体租约宽限和升级交接另在设计任务定。 |
| 暂停是否挡住首版 | A：首版明确不支持，后做整会话暂停；B：本次就定义网络请求、后台、子会话的冻结和恢复。 | **A**；不能拿目标暂停或只冻 TUI 交差。 |
| `--sandbox` 如何处理 | A：首版拒绝 fence 档案，自带沙箱照配置，界面区分；B：现在增加会话级沙箱控制与公开映射，明确接受不等同 fence 的范围。 | **A**；显式与继承的档案都不能静默忽略，`--no-sandbox` 只能表示不套 fence，不能暗中关 canter 自带沙箱。 |

**尚未查证：** 实际安装版本是否等于这些源码、真实 PTY 的 send 确认及按键表现、未来 stop/lease 的耗时、pen 升级与服务端替换交叠时的行为。本轮未启动程序验证，因此不声称接入可用或给精确工期。R49 旧建议与后续已定设计的变化已在 §2.4 对照；未遇到需要猜测才能继续调查的设计意图。

## 依据索引与本轮验证

路径前缀：`C`＝canter 仓库，`R`＝同级 ranch 仓库，`P`＝同级 paddock 仓库。行号对应文首源码版本；只引用源码／仓库文档，没有读取真实状态目录。

| 编号 | 文件与行号／节名 |
|---|---|
| C1 | `C/crates/canter/src/lib.rs:57–95,166–241,300–345`；`C/crates/canter-tui/src/run.rs:431–479,506–540,660–708`（创建、恢复、切换） |
| C2 | `C/crates/canter-protocol/src/commands.rs:30–77,352–420,435–455,488–567`；`C/crates/canter-server/src/http.rs:836–905,978–1054,1401–1447`；`C/crates/canter-protocol/README.md:166–170,213–217`（接口、环境、SSE） |
| C3 | `C/crates/canter-protocol/src/types.rs:17–34,64–98,107–149,265–280,434–469,637–647,918–1013`；`C/crates/canter-protocol/src/event.rs:10–20,95–170`（状态、快照、轮、请求） |
| C4 | `C/docs/DESIGN.md:430–468,478–484`「钩子」；`C/crates/canter-core/src/hooks.rs:132–194,217–267,297–337,430–449`；`C/crates/canter-core/src/runner.rs:1431–1483,1530–1598`；`C/crates/canter-core/src/plan_mode.rs:140–145`；`C/docs/任务/P2-153-hooks.md:32–80,105–128`（要求、实现、完成及返工） |
| C5 | `C/crates/canter-core/src/clients.rs:7–118`；`C/crates/canter-core/src/session.rs:1068,1156,1864–1893,1952–2043`；`C/crates/canter-server/src/http.rs:1230–1257`；`C/crates/canter-tui/src/run.rs:615–630,2625–2641`；`C/docs/任务/P2-172-SessionEnd客户端离开.md:37–46,75–79,108–127` |
| C6 | `C/docs/DESIGN.md:403–428`「沙箱」；`C/docs/任务/P2-123-沙箱.md:33–65`（决定、拆分、用户验收）；`C/crates/canter-tools/src/executor.rs:124–133,218–243`；`C/crates/canter-protocol/src/commands.rs:371–390,542–555`（创建／更新无沙箱开关） |
| C7 | `C/docs/DESIGN.md:103–106,468–476`；`C/crates/canter/src/lib.rs:69–79`；`C/crates/canter-server/src/discovery.rs:17–97,107–115`；`C/crates/canter-server/README.md:8–30`；`C/crates/canter-tui/src/run.rs:2584–2616`（发现、升级、重连） |
| C8 | `C/crates/canter-core/src/skills.rs:10–54`；`C/crates/canter-protocol/README.md:156–160,190–196`（改名、技能）；`C/crates/canter-protocol/src/types.rs:265–280`（摘要无标签筛选契约） |
| C9 | `C/docs/DESIGN.md:279–295`「非交互模式」；`C/crates/canter/src/exec.rs:14–60,186–265`（参数、恢复、新建） |
| C10 | `C/docs/DESIGN.md:26,32`（即时建议、runningTurns 包括轮间续跑与后台）；`C/crates/canter-protocol/src/commands.rs:51–53,352–365`；`C/crates/canter-core/src/session.rs:2181–2238`（所有轮开始） |
| C11 | `C/docs/DESIGN.md:297–315`「文件夹信任」；`C/crates/canter-tui/src/run.rs:447–470,506–528`（信任与首句） |
| R1 | `R/crates/corral/src/agents/mod.rs:6–13,28–135,164–185,218–279`；`agents/claude.rs:12–43`、`agents/codex.rs:12–47`、`agents/pi.rs:12–31`（后三个相对同一 src 目录）；`R/docs/DESIGN.md` §7「加一家」 |
| R2 | `R/crates/corral/src/cli.rs:108–200,236–329,627–675,744–765`；`R/crates/corral/src/environment.rs:169–178`；`R/crates/corral/src/state.rs:19–35,293–309` |
| R3 | `R/crates/corral/src/hooks.rs:11–69,74–142,172–237`；`R/crates/corral/src/events.rs:20–112,128–195`；`R/crates/corral/src/cli.rs:185–196` |
| R4 | `R/docs/CONTRACT.md` §「被人打断的一轮」（114–126）、「提问框」（128–144）、「Kimi 提问、批准」（146–163）、「Bash 权限框」（165–177） |
| R5 | `R/docs/DESIGN.md:57–66`；`R/crates/corral/src/pen.rs:442–487,623–651`；`R/crates/corral/src/cli.rs:811–858`；`R/docs/CONTRACT.md:7–16` |
| R6 | `R/docs/CONTRACT.md:313–375`「sandbox 与 fence」；`R/crates/corral/src/sandbox.rs:42–114`；`R/crates/corral/src/cli.rs:627–662` |
| R7 | `R/docs/Corral核心Rust集成设计.md` §3～6；`R/docs/Corral通用升级设计.md:15–40,69–75`（旧核心文档的 helper 版本绑定以此后续设计为准）；`R/crates/corral/src/pen/upgrade.rs:119–132,270–282`（升级快照） |
| R8 | `R/crates/corral/src/skills.rs:9–25`；`R/docs/CONTRACT.md:179–206`「integration status」 |
| P1 | `P/README.md:3–6`；`P/app/src/corral.rs:16–68,87–103,194–237`；`P/app/src/agent_shell.rs:255`（attach） |
| P2 | `P/app/src/new_agent.rs:17–114,449–473,560–584,979–1001,1139–1146`；`P/app/src/agent_shell.rs:14–40` |
| P3 | `P/app/src/pause.rs:78–108`；`P/docs/DESIGN.md:392,821–828`（§13「agent 暂停（冻结）」「委派的 agent 放进 sandbox」） |
| P4 | `P/app/src/integrations.rs:1–28,54–68`；`P/docs/DESIGN.md:40–45,668`（§3「只共享公开约定」、§13「Integrations」）；§13「收件箱」 |

设计边界：`C/docs/DESIGN.md` 第 1、2、11、12 条；历史对照：`C/docs/调研/R49-自研agent调研.md:875–1020`（§6.1～6.6）。

本轮验证限定为引用文件／行号核对及 `git diff --check`。没有运行测试、构建、扫描器或真实模型，没有启动服务端／agent，没有操作用户会话或其它仓库文件。报告中的后续验证建议不代表本轮执行过。
