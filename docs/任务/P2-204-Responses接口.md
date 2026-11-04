# 任务：第二期-28.3 - Responses 接口跑通

2026-11-04，主控建。canter/main 交给 canter/dev-responses（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 1.0）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：P2-80 接 GPT 订阅按 `docs/调研/P2-33-第二个模型方案.md` §8 分件，第 4 件（Responses）11-04 用户选拆两件，本件是前一半：`config.json` 能配 GPT 这一家，服务端用 P2-202、P2-203 登录的账号发 Responses 请求，一轮对话加工具循环能用，加密思考在这一轮的工具循环里原样回传。后一半 P2-210 做跨轮次、跨重启、跨压缩的加密思考。`apply_patch`（自由格式工具）是 P2-205，模型目录和思考档位是 P2-206，和别家同会话切换是 P2-207。
依赖：P2-203
执行：派出

## 用户原话

- 11-04 用户：「但是我想先做gpt，这样就齐全了。可以安心做eval」。§7 四个问题都选推荐（DESIGN 第 6 条 11-04 那一条）。
- 11-04 用户问「我理解是不是这个思考过程加密导致的204比较难做？」；用户：「203做完再详细讨论」。
- 11-04 用户答三个问题，都选推荐：拆两件（本件新接口跑通，P2-210 存盘、压缩、恢复）；配置照推荐写（见「要做成什么」第 1 条）；本件合并后用户在场真调 2 次。

## 要做成什么

照方案 §3「最小模块边界和旧配置兼容」「SIWC 请求契约」「流式状态机与私有历史」「专用错误不应都变成限流重试」做。字段名以官方文档为准（见「先读」）。

1. **配置和路由**：`config.json` 的 `providers` 里能写 GPT 这一家：
   ```json
   "chatgpt": {
     "api": "responses",
     "auth": "chatgpt",
     "models": { "<模型名>": { "context_window": 0, "image": true } }
   }
   ```
   - 不写 `api_key`；令牌从 `canter auth use chatgpt` 选中的当前账号取（P2-202 的取令牌接口），401 时照 P2-202 的 `recover_unauthorized` 恢复一次。
   - 不能写 `base_url`：地址写死成官方的 `https://api.openai.com/v1`，写了 `base_url` 或 `api_key` 是配置错误（照 P2-154 沿用旧配置并提示）。`auth` 只认 `chatgpt`。
   - 模型先手写，和别家一样；`/model` 照 P2-201 能选到。
   - 没登录、要重新登录、没授权套餐时，请求失败给明确的英文提示（比如让用户跑 `canter auth login chatgpt`），不回落到别家。
   - 没有价格，花费照现在没价格的模型处理，不显示 $0。
2. **请求怎么拼**（只发明确允许的字段，不把通用请求选项整个序列化过去）：
   - 固定 `store: false`、`stream: true`；每次带完整的 input，不带 `previous_response_id`。
   - 系统提示（固定段和各段）原样放进最前面的 developer 消息（DESIGN 第 23 条那段已定），不改提示词的内容和顺序；用户文字和图片按 input_text、input_image 发。
   - 工具放稳定的 namespace（canter 内置一组、MCP 一组），`strict: false`，参数照旧由工具层校验；只分派目录里有的工具。
   - Preview limitations 页列的参数一个都不发；也不发 DeepSeek 的 `thinking`、`max_tokens`、`stream_options`。
   - 思考档位这次不发（用模型默认），P2-206 做档位；思考摘要按文档开（用于显示）。
3. **流怎么读**：
   - 按 `output_index`、`item_id` 分开收文字、思考摘要、工具调用；工具参数保持原样字符串，item id 和 `call_id` 分开存。
   - 文字增量立即显示；思考摘要按现有的思考显示；密文不显示、不进公开事件和 SSE。
   - 收到 `response.completed` 才算成功、才把本批工具交给现有的批准和执行流程。`response.failed`、顶层 error、`response.incomplete`、EOF、取消都不执行工具，已显示的文字保留并标失败或打断。
   - 只在还没输出任何内容和用量时有限重试暂时错误（沿用 P2-110 的边界）；流里出现额度错误照样处理。
   - 认不出的非关键事件跳过；认不出的输出项不执行、不当成成功的空回复，报当前版本不支持。
4. **GPT 专有的报错**：照方案 §3 那张表映射，至少：额度用完（`subscription_sharing_usage_limit_exceeded`）不自动重试、给 Manage usage 链接 `https://chatgpt.com/settings/usage`、不换别家；账号不符合条件、不支持的参数或工具、路由不对、授权问题各给明确的英文提示；只回 `detail` 的 401/403/503 按状态码分类，不回显正文。错误照现有脱敏办法，不留服务器原文。
5. **用量**：`response.completed` 的 usage 映射到现有计数：`input_tokens_details.cached_tokens` 算缓存命中，`output_tokens_details.reasoning_tokens` 是输出的一部分，不重复相加；缺 usage 记成不知道，不当成 0。
6. **一轮里加密思考原样回传**：GPT 在一轮里调了工具，下一次请求要把上一次的输出项（含 reasoning 的 `encrypted_content`、message 的 phase、工具调用）按原顺序原样带上。
   - 照方案在助手历史里加一个可省略的新字段，存 Responses 原样的输出项（只存需要回传的字段，不存 HTTP 头、认证、错误正文），密文只进 0600 的模型历史。
   - **本件只回传当前这一轮工具循环里的输出项**；更早的轮次、压缩之后、重启之后，发给 GPT 的历史只用普通内容（文字和工具调用、结果），不带密文。跨轮次、跨重启、跨压缩的回传是 P2-210。
   - 新字段对 DeepSeek、Kimi 等 Chat Completions 的家不存在，它们的请求一个字节都不变。
7. **后台运行同意**：用 P2-203 的 `background_consent`。会话用的 GPT 账号没同意时，最后一个连着这个会话的界面离开后，不再发新的 GPT 请求；正在跑的工具照常结束、结果照常保存，界面回来后能接着做。同意了就照现在一样接着跑。

## 主控补的（用户没说到的地方）

- 档位：P2-206 之前不发 `reasoning.effort`，`/model` 里 GPT 模型的档位选项先不动；会话里存着 DeepSeek 的档位换到 GPT 时，也不把它翻译成 GPT 的档位发出去。
- 压缩、顺口问、交接、子 agent 走 GPT 时，照现有流程发（只用普通内容）；保证不出错、不泄露，质量问题留 P2-210。
- 合并后用户真调 2 次用的模型名和窗口，主控到时按官方或 Codex 的资料写进用户的 `config.json`，本件不预置任何模型。

## 先读

- `docs/DESIGN.md` 第 6 条（11-04 GPT 那一条、P2-200、P2-202、P2-203 的几条）、第 8 条、第 13 条（会话存两份）、第 17 条（缓存只往后加）、第 23 条（不绑定 DeepSeek，含 Responses 转换器那段）。
- `docs/调研/P2-33-第二个模型方案.md` §3（整节）、§5（会话切换和缓存，本件只看边界）、§6「未来必须有的本机假服务测试」里请求编码、完整流、失败流三组，§8 第 4 件。
- 官方：Models and inference `https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference`、Errors and recovery `.../errors-and-recovery`、Preview limitations `.../preview-limitations`、Migrate to Responses `https://developers.openai.com/api/docs/guides/migrate-to-responses`、Function calling `https://developers.openai.com/api/docs/guides/function-calling`。开工时读当天的版本，和方案对不上的以文档为准，写进完成记录。
- 代码：`crates/canter-model/src/`（`types.rs` 的 `Message`、`AssistantContent`、`ToolDefinition`、`ModelEvent`，`chat_completions.rs`、`wire.rs`、`retry.rs`、`error.rs`）、`crates/canter-server/src/config.rs`（`Provider`、`Router`、`model_client`）、`crates/canter-server/src/chatgpt_auth/`（`access_token`、`recover_unauthorized`、`background_consent`、`current_record`）、`crates/canter-core/src/runner.rs`、`usage.rs`、`context.rs`。
- 参考 Codex `rust-v0.163.0` 的 `codex-api` Responses 请求和 SSE 解析（`/tmp` 下只读克隆）。只参考，不搬；完成记录里写参考了哪里。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-204-responses`，分支 `p2-204-responses`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-204-responses`。

## 怎么算做完

- 上面 1～7 条都做到。
- DESIGN：第 6 条补 GPT 这一家的配置写法；第 8 条记协议里新加的东西（如果有）；第 13 条补助手历史的新字段和「本件只回传当前一轮」。`crates/canter-server/README.md` 的配置例子加 GPT 这一家。
- 验证只做这些：
  - 为本件加针对性测试，用测试进程里起的假 Responses 服务端和 P2-202 测试里那种假授权服务（隔离的 HOME 和 `CANTER_HOME`、临时端口和 socket）。
  - 至少覆盖：请求只带允许的字段、developer 消息原样、工具在 namespace 里；文字、思考摘要、工具调用交错的流；SSE 分片和多行 data；一轮里两次工具调用后第三次请求原样带上前两次的输出项（含密文）；`completed` 之前不执行工具；failed、incomplete、EOF、取消、先 200 再报错；额度用完不重试；401 恢复一次；没登录、要重新登录、没授权套餐的提示；用量映射；没同意后台时最后一个界面离开后不再发请求。
  - 断言密文、令牌不出现在公开事件、SSE、日志里。
  - **单家和几家 Chat Completions 的配置，发给模型的请求逐字节和现在一样**（`canter-server`、`canter` 里有逐字节比对的测试，要跑）。
  - 改到哪几个 crate 就跑哪几个的全部测试（`cargo nextest run -p <crate>`）。再加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。全套由主控合并后跑，你不跑。

## 不要做

- 不连真的 `api.openai.com`、`auth.openai.com`，不做真实请求；真调 2 次在合并后由用户在场做。
- 不借 Codex 的 client id、`chatgpt.com/backend-api`，不冒充 Codex 的请求头。
- 跨轮次、跨重启、跨压缩回传密文，压缩时密文的处理，上下文估算算密文（P2-210）。
- 自由格式工具、`apply_patch`（P2-205）；模型目录、思考档位、套餐用量显示、`Using ChatGPT plan` 标记（P2-206）；GPT 和别家之间的历史转换（P2-207）。现在 `/model` 空闲时照 P2-200 能换家，换到 GPT 时历史只按普通内容发，不做专门的转换。
- WebSocket、官方的服务端压缩、`previous_response_id`、OpenAI 自带的搜网页、托管工具。
- 不改 prompt 的拼接（系统提示词分段、上下文顺序和内容、工具列表的顺序和内容）、压缩提示词和摘要、压缩后带回文件（P2-142）和待办（P2-176）的逻辑；developer 消息只是把现有的系统提示换个外壳。觉得非改不可，停下来问。
- 核心逻辑里不按 OpenAI、DeepSeek 的名字或地址分支（DESIGN 第 23 条）；接口差异只在适配器里。
- 不自动换家、不在额度用完时改用别家。
- 不碰用户真实的 `~/.canter`、会话、配置和登录记录，不碰用户正在跑的 canter 服务端。
- 不改 ranch、paddock、cairn 等别的仓库。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的服务用起的时候记下的 PID。
- 不用 Python。
- 屏幕上的文字用英文。
- 碰到要拿主意的事（包括官方文档和方案对不上、要改现有测试的断言才能过、本件和 P2-210 的边界划不开），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-204-responses` 上提交。

## 合并以后（用户和主控做）

- 11-04 用户选：本件合并、用户装好后，用户在场真调 2 次：一次普通问答、一次带工具调用（比如让它读一个文件）。主控先在用户的 `config.json` 里加 GPT 这一家和一个模型，用户 `canter auth login chatgpt` 登录。确认真服务器认请求格式、流能正常解析、工具调用后能接着回答。会占一点套餐额度。对不上的先修，再派 P2-210。主控把结果和真调次数记进本文件。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-11-04，canter/dev-responses。

**做了什么**：增加 Responses 配置、固定官方地址、请求白名单、developer/图片转换、canter 与 MCP namespace、交错 SSE 状态机和专用安全错误。只有完整、验证通过的 completed 批次能进入原来的工具批准/执行流程；输出或用量出现后不重试，401 最多恢复一次，额度错误给 Manage usage 链接。用量映射保持 reasoning 是 output 子集，缺 usage 和缺价格仍是未知。服务端登录命令、请求和配置重载共用一个惰性账号管理器。

助手私有历史增加可省略的 replay，保存过滤后的有序输出项；当前轮的两次工具循环会原样回传密文、phase、item id、call_id 和参数。跨轮、重启、压缩、顺口问、交接只发送普通内容，估算不计密文长度。逐账号后台同意控制每次请求及重试；最后一个界面离开后等待重连，已运行工具完成并保存。DESIGN 第 6、13 条和服务端 README 已更新；没有新增客户端 HTTP/SSE 协议字段。

**验证了什么**：所有命令在前台运行并等待结束；Cargo 均使用 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-204-responses`。

- `cargo nextest run -p canter-model -p canter-core -p canter-server --no-fail-fast --status-level fail --final-status-level fail`：602 项通过，27 项为仓库原有忽略项（含手工基准和 P2-146 跟踪的测试），本件没有新增忽略项。
- `cargo nextest run -p canter -E 'test(no_mcp_request_matches_pre_search_bytes) | test(text_only_read_keeps_the_original_model_messages_byte_for_byte) | test(exec_model_takes_a_provider_and_an_unknown_one_points_to_model) | test(model_choice_crosses_providers_and_the_status_bar_follows)' --no-fail-fast --status-level fail --final-status-level fail`：4 项通过。服务端完整测试同时包含 `legacy_requests_keep_the_same_bytes_with_and_without_named_providers`、`legacy_restored_unlisted_slash_model_keeps_the_original_request_bytes`，单家、混合多家及恢复会话的旧请求字节不变。
- clippy 提示条件分支写法后作了保持行为不变的调整，再运行 model 全部测试及 Responses、replay、估算、旧请求字节相关筛选，共 47 项通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。
- 假 OAuth 与假 Responses 都在测试进程内用 loopback；会话测试使用隔离 HOME、CANTER_HOME、临时端口和 Unix socket。覆盖请求白名单、原样提示、namespace、UTF-8 分片和多行 data、交错文字/摘要/函数参数、所有失败终态及取消、未知输出、拒答、缺用量、额度错误、401 一次刷新和第二次拒绝、三种登录提示、界面断开阻止后续请求与重试。真实两次 shell 工具循环验证第三次请求包含前两批输出，以及结果存盘、重连恢复、0600 历史、公开事件/SSE/日志不含密文与令牌。
- 开发中先取得目标行为失败证据，再补实现；包括不支持 Responses 配置、请求字段/增量不符、缺少 replay、密文影响估算、null 被当作字段缺省、路由丢失界面状态，以及 added 已含内容后仍重试等。已有测试断言未被放宽。

**拿主意的地方**：保留原 `Model::stream`，增加默认委托的 `stream_with_context` 传递本机界面状态和当前轮账号绑定；不把这些信息加入模型 payload。账号在该轮首次请求时选定，工具循环保持绑定，子 agent 使用根会话的界面状态；每次尝试重新检查同意，令牌刷新后再检查一次。replay 用当前轮标识限定回传，接口与模型校验留在适配器，核心不按厂商名或地址分支。Chat Completions 的序列化及重试策略保持原样。

主控本轮明确决定：将 `crates/canter-server/src/reload_tests/providers.rs` 的 `provider_reload_is_atomic_and_frozen_until_the_next_turn` 中非法接口值由 `responses` 改成 `unsupported_api`。原因是 Responses 现在是受支持接口，原样例不再代表未知接口；报错带家名、说明不支持接口、脱敏、沿用旧配置等原断言一条未改。另加 `responses_requires_chatgpt_auth_with_a_provider_specific_error`，独立覆盖缺少 auth、null 和错误 auth；没有用它替换原测试。其余旧测试只有新可选字段及函数签名所需的机械适配。没有仍需主控决定的问题。

开工按 2026-11-04 官方文档核对：[Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)、[Errors and recovery](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery)、[Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)、[Migrate to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)、[Function calling](https://developers.openai.com/api/docs/guides/function-calling)、[Reasoning](https://developers.openai.com/api/docs/guides/reasoning)。没有发现需要改变本件约定的冲突；按当前说明，密文默认返回，不额外发送 include，摘要用 summary:auto，本件不发送 effort。只读参考 `/tmp/canter-p2-202-codex-reference` 的 `rust-v0.163.0` 标签下 `codex-rs/codex-api/src/common.rs`、`requests/responses.rs`、`sse/responses.rs`，没有搬代码。

**没做的事**：真实 OpenAI 授权/推理请求均为 0 次，没有动用户真实账号、配置、会话或常驻服务。合并后用户在场的 2 次真调仍由主控安排。没有做 P2-210 的跨轮/重启/压缩密文回传及密文估算，也没有做 P2-205/206/207、WebSocket、托管工具或服务端压缩。未跑全 workspace 测试；未安装、合并或推送，只在本任务分支提交。

**踩过的坑**：假授权存储直接使用已有临时目录时，目录权限不符合私有存储要求；改用其中独立的 state 目录。`Option<String>` 会把显式 null 当成缺省，故对禁用字段使用只接受字符串的反序列化，区分未写与显式写入。async-stream 宏中的 let-chain 曾触发 edition 解析错误，改成先 match 提取 usage 再判断，兼容编译和 clippy。第一次整组回归被旧测试的“不支持 responses”假设挡住，按任务停止、取得上述主控决定后复跑通过。

### 返工

2026-11-04，已只读主仓库 `docs/任务/P2-204-交叉审查.md` 的「审查意见」和「主控判断」，落实两条必须改：canter、mcp namespace 分别发送固定的 `Tools in the canter namespace.`、`Tools in the mcp namespace.`；普通助手文字发送 `type: message`、`role: assistant` 和 `output_text`，developer、user 仍用 `input_text`。

**请求编码测试**：更新 `request_preserves_prompt_images_and_tools_using_only_allowed_fields` 的完整请求期望值，逐字段要求两个 namespace 的 description；新增 `ordinary_history_uses_output_text_and_keeps_function_call_shapes`，覆盖上一轮助手文字、canter/MCP 工具调用及结果、后续助手文字和新用户消息，完整断言每个 input 项的字段、顺序、call_id 和原始参数/结果字符串。两项均先确认因目标缺陷失败，再修实现后通过。此次暴露的坑是本机假服务不会替官方验证 schema，原测试期望值也遗漏了必填字段；本次按官方契约修正期望，而不是只确认请求成功。

**其余编码核对**：重新对照官方 [Function calling](https://developers.openai.com/api/docs/guides/function-calling)、[Migrate to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)、[Images and vision](https://developers.openai.com/api/docs/guides/images-vision)、[SIWC Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)，并只读本机 OpenAI SDK 的 Responses 类型和 Codex `rust-v0.163.0` 的 `codex-rs/tools/src/responses_api.rs`、`codex-rs/protocol/src/models.rs`（没有搬代码）。核对了顶层白名单、developer/user、图片、namespace 内函数、普通 function_call/function_call_output，以及 replay 的 message/reasoning/function_call；除上述两处外，没有发现其他已发送字段或形状与契约不符。图片 `detail` 官方明确允许省略并默认 `auto`，保持省略。两条「可以不改」均未改：message/function_call 的密文白名单问题留 P2-210；自定义 headers 继续忽略。

**验证**：命令全部在前台跑完，Cargo 使用本分支独立编译目录。两项请求编码测试及 `two_tool_rounds_replay_order_phase_ciphertext_and_raw_arguments` 共 3 项通过；`cargo nextest run -p canter-model` 全部 33 项通过、0 跳过（含原请求逐字节测试）；`cargo nextest run -p canter-server -E 'test(two_tool_loops_pause_at_last_disconnect_replay_privately_and_resume)'` 1 项通过；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 均通过。未跑 workspace 全套，真实授权/模型调用仍为 0 次，未改主仓库审查文件或用户状态。没有新的待主控决定事项；只在 `p2-204-responses` 提交，未合并或推送。

## 主控审查

- 第一轮看 diff：核心只加了接线（压缩写 `replay: None`、忽略 `ModelEvent::Replay`，`message_tokens` 估算时去掉 replay，runner 只在本轮成功结束时按 turn id 挂 replay），没有按厂商分支；不用 GPT 时请求逐字节不变由旧请求字节测试覆盖。开发问的 `responses` 旧测试值，主控定改成 `unsupported_api`、原断言不动、另加缺 auth 的测试。
- 交叉审查（Claude Code，`opus[1m]` xhigh）挑出 2 条必须改：namespace 缺必填 `description`、助手历史用了 `input_text`。两条都是真服务器会拒、假服务端测不出的格式问题，交回返工；`b860653` 修好并补了逐字段请求测试，复核「可以合并」。可以不改 1 记进 P2-210，可以不改 2（`headers` 被忽略不报错）记 HANDOFF。
- 合并后在 main 跑全套：`cargo nextest run --all-targets --no-fail-fast` 1729 过、49 跳过；`cargo test --doc --workspace` 通过；clippy、fmt、diff check 通过。
- 还没真调：请求格式要等合并以后用户在场的 2 次真调确认。
- 结果：返工 1 次

## 用户真调（11-04 合并以后）

- 用户 `canter upgrade` 装 `88bf2d8`、`canter auth login chatgpt` 登录，在 canter-test-rust 用 `chatgpt/gpt-6.1-sol`（配置里窗口写 272000）真调 2 次，都失败：
  - 普通问答：文字正常流出、用量记到（↑8.7k ↓42），最后报 `Model request failed: unexpected error`。
  - 读 README.md：没出文字（↓19，像是工具调用），同样报 `unexpected error`，工具没执行。
- 服务端日志没有细节（错误只留种类，不留原文）。看代码：用量来自 `response.completed`，之后 `sse.rs` 拿 `response.output` 和流里 `output_item.added/done` 记下的条目按位置核对 id、类型，对不上就是 `ErrorKind::Other`。Codex `rust-v0.163.0` `codex-rs/codex-api/src/sse/responses.rs` 只用 `output_item.done` 拼结果，不读 `response.completed` 的 output（它的测试里 output 是 `[]`）。主控推测订阅这条线路的 `response.completed` 不带完整 output，没有原文证实。
- 真调次数：2 次（`observed` 已为真，没有自动重试）。
