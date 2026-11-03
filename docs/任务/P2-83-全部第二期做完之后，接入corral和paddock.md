# 任务：第二期-26 - 接入corral和paddock(尤其注意hook的问题）

2026-10-27，用户在看板上新建。
2026-11-03，主控补成调研任务。
2026-11-03，canter/main 交给 canter/dev-corral-research（Codex，gpt-6-astra / xhigh）。
路由：高 / 交叉审查不要 / 影响面：看得见（路由：重（置信 0.16）、拿不准、看得见；本轮只调研不改代码，照规矩算看得见、不交叉审查）
类型：调研
你是被委派的 agent：照本文件做，不要再开别的 agent。
提示：区分已查证事实、推测和未知，给出直接依据；结论限定在本轮调查范围。
依据：11-03 用户把本件挪到 eval 前面。本件先查清 corral、paddock 要接 canter 需要什么、canter 现在有什么、差什么，出调研报告；本轮不改代码，实现等用户看过报告再拆。
执行：派出

## 用户原话

- 11-01 用户：「p2-83提到第二期后面」。排在 eval（第二期-25）后、P2-81 收尾（第二期-27）前。
- 11-03 用户：「p83提到前面」，挪到 eval 前面（现为第二期-26）。
- 11-03 主控建议先派一件调研：corral、paddock 接一个 agent 要什么，canter 现在有什么、差什么，分成 canter 自己要做的和交给 corral、paddock 的需求两份清单；用户：「好，写调研任务文件」。
- 照 CLAUDE.md：canter 只做自己这边的公开接口；corral、paddock 那边要改的写成需求交用户，不改它们的仓库。
- 11-01 P2-153 hooks 留下的两件，开工时要定：
  - SessionEnd 钩子只在会话被关掉时触发；会话常驻服务端，退出终端、`canter exec` 跑完都不关会话，所以现在基本只有服务端停掉时才触发。用户 11-01 定「合并后另开一件」，在 P2-172 做（P2-153 合并后）；本件开工时看 P2-172 定下的结果够不够 corral 用。
  - 钩子在常驻服务端上跑，拿不到终端那边的环境变量（corral 靠 `CORRAL_KIND`、`CORRAL_HELPER` 认自己开的 agent）；「会话带上客户端的环境」本件要做（R49 §6.4 做法一）。

## 先读

- 仓库根的 `CLAUDE.md`（「独立」「协议」「不改别的仓库」几条）。
- `docs/DESIGN.md` 第 1、2、11、12 条，以及写钩子（P2-153、P2-172）、沙箱（P2-123）、会话名字和标签（如果有）的段落。
- `docs/调研/R49-自研agent调研.md` 第 6 节（6.1～6.6）：当时对接 corral 的设想，五种做法和各自工作量。当时 canter 还叫 `colt`。本件要对照现在的代码核对这一节哪些已经有了、哪些变了。
- `docs/任务/P2-153-hooks.md`、`docs/任务/P2-172-SessionEnd客户端离开.md`、`docs/任务/P2-123-沙箱.md`的要做的和完成记录。
- corral（ranch 仓库，本机 `~/Developer/personal_projs/ranch`）：`docs/DESIGN.md`（尤其第 6～8 节和「加一家」）、`docs/CONTRACT.md`、`docs/Corral核心Rust集成设计.md`、`crates/corral/src/agents/`（`mod.rs` 的 `Adapter` 和已有几家，挑 `claude.rs`、`codex.rs`、`pi.rs` 对照）、`crates/corral/src/hooks.rs`、`events.rs`、`state.rs`。
- paddock（本机 `~/Developer/personal_projs/paddock`）：`README.md`、`docs/` 里写它怎么用 corral 的部分；只看它对 agent 有什么要求（看状态、显示终端、按钮等），不细看界面代码。
- 别的 ranch 文档按需读；`~/Developer/personal_projs/corral` 是旧版，不用读。

## 在哪里干活

- 在主仓库 `~/Developer/personal_projs/canter` 读代码，不建 worktree、不开分支。只新建报告 `docs/调研/P2-83-接corral和paddock调研.md`，再在本文件末尾写完成记录；两份文件在 main 上提交（按路径 `git add`），不动别的文件。
- ranch、paddock 两个仓库只读：不改文件、不提交、不切分支，也不在里面跑 `cargo` 或别的会生成文件的命令。

## 要做的

调研报告回答下面几件事，每条写明依据（文件和行号、文档节名），分清「查到的」「推测的」「不知道的」：

1. **corral 和 paddock 要从一个 agent 拿到什么**：开（名字、标签、首句、免确认）、状态（starting / working / idle / blocked、被打断）、回复、提问和批准的内容、停止、暂停、套墙、升级、技能位置，以及 paddock 另外要的。照现在的 corral、paddock 代码列，不照 R49 当时的记忆。
2. **canter 现在有什么**：对照第 1 条逐项写 canter 现有的公开接口（命令行、协议、钩子事件和字段、会话名字和标签、免确认、带首句启动、沙箱、SessionStart / SessionEnd 怎么触发、客户端退出后会话怎么样），哪些已经够用。R49 第 6 节写的设想，现在已经做了的、做法变了的，一并标出。
3. **差什么**：分两份清单。
   - canter 自己要做的：每条写要做什么、为什么（corral 哪个功能要用）、估计大小（小 / 中 / 大）。只写通用能力，不写 corral 的名字（DESIGN 第 1 条）。必须包含「会话带上客户端的环境」怎么做，以及 P2-172 的 SessionEnd 够不够 corral 判断 agent 退出。
   - 交给 corral、paddock 的需求：每条写要改什么、为什么；只写需求，不写它们的代码。
4. **推荐的做法和次序**：对照 R49 6.4 的五种做法，按现在的情况推荐先做哪种、做到哪一步就能日常用 corral 派活给 canter（开、看状态、取回复、停）；暂停、`--sandbox`（canter 已有自带沙箱，见 P2-123）这两样怎么处理。有实质的备选时才比较，不凑方案。
5. **要用户拿主意的事**：列出来，每个给选项和你的建议。

报告用中文写，简洁；表格、清单就够，不要大段铺陈。

## 怎么算做完

- 用户要的：「corral、paddock 接一个 agent 要什么，canter 现在有什么、差什么，分成 canter 自己要做的和交给 corral、paddock 的需求两份清单」。上面第 1～5 条在报告里都有回答。
- 验证只做这些：`git diff --check`；报告里引用的文件和行号自己核对过。觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 canter 的代码、测试、DESIGN 和别的文档；不改 ranch、paddock、cairn 的任何文件（CLAUDE.md「不改别的仓库」）。
- 不读 `~/.corral`、`~/.pony`、`~/.ranch` 等状态目录，不读用户真实的 `~/.canter`。
- 不用 `corral start` 开 agent 实测，也不对 `corral ls` 里现有的 agent 做 `stop`、`send`、`keys`、`attach`；觉得必须实测才答得上，停下来问。
- 不调真实模型接口，不起 canter 服务端。
- 不在报告里贴令牌、密钥、用户的私人路径以外的私人数据。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 辅助命令不用 Python 或别的脚本语言，只用 shell。
- 碰到要拿主意的事（包括第 1 条里 corral 的要求和 R49 记的明显矛盾、看不懂 corral 某块的设计意图），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不推送。

## 做完

在本文件末尾追加「## 完成记录」：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上报告的结论摘要（三五句）和有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
