# 任务：web_search 可以带回网页正文，工具说明写上搜索语法

2026-11-14 记下。2026-11-17 主控补成需求，用户：「接受你的建议，做吧」。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.96）、不要、改行为）
2026-11-17，canter/main 交给 canter/dev-p2-294（Codex，gpt-6-astra / high；middle-other）。
执行：派出
优先：低
类型：功能变更
依据：canter 的 `web_search`（P2-106）调 DeepSeek 搜索接口，现在只传 `query`、`count`。本轮让模型可以要网页正文（省掉搜完再 `web_fetch`），并在工具说明里写上接口支持的搜索语法；不加别的参数和返回字段。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-14：「deepseek昨天开放了很多api，你查一下咱们能用哪些集成到canter中？」
- 11-14 主控列了三件，用户：「记第1、2件吧」（这是第 2 件）。
- 11-17 主控查了本机会话统计（368 个会话共搜 22 次，10-30 以后 1 次），建议不做；用户问「加的话好加吗？爆炸半径大不大？」，主控答好加、只动 web_search，建议正文让模型自己选、搜索语法写进说明。用户：「接受你的建议，做吧」

## 已查到的（11-14）

- 接口文档：https://api-docs.deepseek.com/guides/search
  - `need_content`（缺省 false）：为 true 时每条结果带网页正文 `content`；
  - `query` 支持 `site:`、`-site:`、`lang:`、`after:`、`before:`，1～128 字符；
  - 价格不变：每千次成功请求 $0.75，失败和空结果不算钱。
- 开工时照文档再核一遍参数名和语法，和上面不一样的以文档为准，在完成记录里写。

## 先读

- 仓库根 `CLAUDE.md` 的「规矩」。
- `docs/DESIGN.md` 里 P2-106 搜网页那一段。
- `crates/canter-tools/src/web_search.rs`、`crates/canter-tools/src/web_search/deepseek.rs`，工具说明在 `crates/canter-tools/src/lib.rs`。
- 测试：`crates/canter-tools/tests/web_search.rs`、`crates/canter-core/tests/web_search.rs`。

## 在哪里干活

- worktree：`../canter-worktrees/p2-294`，分支 `p2-294`（派的时候从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-294`。

## 要做的

1. **正文由模型自己选**：`web_search` 加一个可选参数（比如 `content: bool`，缺省 false）。为 true 时向接口传 `need_content: true`，把每条结果的正文放进给模型的输出。工具说明写清什么时候开（要看网页内容、本来要接着 `web_fetch` 时），以及正文会多占上下文。
2. **正文只给模型**：不写进协议的搜索结果（`WebSearchResult` 不加字段）和 `events.jsonl` 的界面事件，终端显示照旧。（11-17 主控定：给模型的输出照所有工具的老规矩随 `model-history.jsonl` 存盘，和 `web_fetch` 一样，不改核心的存盘机制。）给模型的输出沿用现有的总上限；每条结果的正文分到差不多的份额，后面几条的标题和网址不能被前面的正文挤掉。
3. **搜索语法写进工具说明**：`site:`、`-site:`、`lang:`、`after:`、`before:` 各一句话怎么用。
4. **文档**：DESIGN 里 P2-106 那一段补一句本件做了什么。

## 怎么算做完

- 用户 11-17 同意的做法（主控建议原话）：「正文让模型自己选」「搜索语法写进说明」。
- 不开正文时，发给接口的请求、给模型的输出、终端显示都和现在一样。
- 验证只做这些：
  - 用本机起的假搜索服务端测：开正文时请求带 `need_content: true`、模型输出里有正文；不开时请求和输出和现在一样；正文很长时总输出不超上限、每条结果的标题和网址都在；协议里的搜索结果不带正文。
  - 工具说明变了，跟着更新请求快照（`crates/canter-core/tests/sandbox-disabled-request.json`、`crates/canter-core/tests/fixtures/p2-223-main-requests.jsonl`、`crates/canter/tests/fixtures/no_mcp_request.json`，以实际有变化的为准）。
  - 改到的每个包跑全部测试（`cargo nextest run -p <包>`）；工具说明变了，`canter-core`、`canter-server`、`canter` 也一起跑。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 不跑全套。觉得不够，在回复里说，不要自己加。

## 不要做

- 不调真的 DeepSeek 搜索接口，不花钱；测试一律用本机假服务端。
- 不加 `ori_question`、`snippet`、`site_name`、`authority`、`rerank_score` 这些（用户没要）；不改 `count` 的范围。
- 不改协议、事件和会话存盘的格式；不改终端显示。
- 不为 DeepSeek 在 agent 核心里写特例：搜索服务的参数只在搜索服务那一层（`web_search` 工具和它的服务实现），核心照旧不分模型和服务。
- 不改别的工具和它们的说明。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的服务用记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-294` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：新增可选 `content: bool`，缺省 false，只有开启时才向搜索接口增加 `need_content: true`。提供方结果将正文与协议 `WebSearchResult` 分开；正文仅加入模型输出，沿用凭证回显脱敏。开启时按结果均分 32 KiB 总预算，先给原有标题、网址、日期和摘要留位，再放正文，截断标记计入预算；关闭时保留原有请求和输出。工具说明补上正文用途、上下文开销及五种搜索语法，DESIGN 的 P2-106 段已补充本件决定。
- 文档核对：2026-11-17 阅读 [DeepSeek 官方搜索文档](https://api-docs.deepseek.com/guides/search/)，确认 `need_content` 缺省 false、正文在 `content`，`site:`、`-site:`、`lang:`、`after:`、`before:` 仍适用。官网当前 count 范围为 1～50；按本任务明确要求，canter 仍保持 1～20，未加入其它参数或返回字段。
- 验证了什么：先跑新增本机假服务用例，确认因不支持 `content` 失败，再实现并通过；长正文用例先因总长度超限失败，再按条分配预算后通过；工具定义检查先因缺少 content schema 失败，再补定义后通过。验证了缺省和显式 false 的请求及输出原样、true 请求带 need_content、模型收到正文、正文脱敏、20 条长正文总输出不超 32 KiB 且每条标题/网址及正文首尾都保留、各正文份额接近，以及协议结果和 events.jsonl 不带正文、model-history.jsonl 保留正文。相关 web_search 测试（canter-core 11 项、canter-tools 5 项）通过；`cargo nextest run -p canter-tools -p canter-core -p canter-server -p canter --no-fail-fast` 报告 1278 项通过、50 项跳过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。三个指定请求快照均未包含 web_search，其字节一致性测试全部通过，无需更新文件。所有 Cargo 命令均使用本任务独立编译目录，所有命令在前台等待结束。
- 拿主意的地方：主控明确「正文可以随模型历史（model-history.jsonl）存盘，和 web_fetch 的输出一样；任务文件里『不写进会话存盘』指的是不进协议的搜索结果（WebSearchResult）和 events.jsonl 里的界面事件，是主控写得不准，不改核心的存盘机制」。已照此执行，核心存盘机制、协议和终端显示均未改动；没有其它待主控决定的事。
- 没做的事：真实 DeepSeek 搜索和真实模型接口调用均为 0 次；未运行工作区全套测试，未改 count 范围、其它工具或依赖，未合并 main、未推送或部署。
- 踩过的坑：直接沿用整段输出的头尾截断时，长正文回归先得到 32794 字节（原有截断标记允许额外占字节），超过本件要求的 32768 字节；已改成开启正文时按条留预算，截断标记计入限额，避免前面正文占掉后面结果的位置。

## 主控审查

- 改动只在 `canter-tools` 的 web_search 两个文件、工具说明、测试和 DESIGN 一句；不开正文时请求和模型输出与原来逐字节一致（测试覆盖缺省和显式 false），正文不进 `WebSearchResult` 和 `events.jsonl`，随 `model-history.jsonl` 存盘（主控 11-17 定）。凭证回显脱敏也覆盖了正文。
- 三个请求快照不含 web_search（只在配了搜索 key 时出现），不用更新，开发说得对。
- 工具说明从 588 字节长到 1052 字节，每次请求约多 120 个 token；原说明已经一句话列过搜索语法，这次按任务要求逐条展开。已告诉用户，压不压由用户定。
- 合并前全套：`cargo nextest run --all-targets` 2230 过、50 跳过；`cargo test --doc --workspace` 通过。
- 结果：一次过
