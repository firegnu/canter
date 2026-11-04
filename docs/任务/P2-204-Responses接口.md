# 任务：第二期-28.3 - Responses 接口跑通

2026-11-04，主控建。canter/main 交给 canter/dev-responses（Codex，gpt-6-astra / xhigh）；交叉审查交 Claude Code（opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 1.0）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：P2-80 接 GPT 订阅按 `docs/调研/P2-33-第二个模型方案.md` §8 分件，第 4 件（Responses）11-04 用户选拆两件，本件是前一半：`config.json` 能配 GPT 这一家，服务端用 P2-202、P2-203 登录的账号发 Responses 请求，一轮对话加工具循环能用，加密思考在这一轮的工具循环里原样回传。后一半 P2-210 做跨轮次、跨重启、跨压缩的加密思考。`apply_patch`（自由格式工具）是 P2-205，模型目录和思考档位是 P2-206，和别家同会话切换是 P2-207。
依赖：P2-203
执行：派出
待用户：确认任务文件

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
