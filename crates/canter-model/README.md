# canter-model

各家通用的模型消息和流式接口。第一期提供 DeepSeek 的 Chat Completions 适配器，不依赖工具包、协议包或存盘包。

## 第 4 件调用的接口

- `ChatCompletions::new(base_url, ApiKey::new(key))` 创建客户端。URL 可带 `/v1` 和尾部斜杠；适配器追加 `/chat/completions`。模型名在每次 `Request.model` 中传入。
- `Model::stream(Request, CancellationToken) -> ModelStream<'_>` 返回 `Send` 的拉取式流，可以通过 `dyn Model` 注入假模型。调用方持续 poll；丢弃流即丢弃请求，本包不开后台任务。
- `Request` 包含 `model`、`messages`、`tools`、`options`。`ToolDefinition.parameters` 是 `serde_json::Value`。`RequestOptions.thinking: Option<bool>` 控制思考开关，`reasoning_effort: Option<String>` 传思考档位，`max_output_tokens: Option<u32>` 控制输出上限；`None` 不发送对应字段，沿用提供方默认值。显式关闭思考时不发送 `reasoning_effort`。
- `ModelEvent` 包括带尝试内项目 ID 的 `TextDelta`、`ReasoningDelta`，参数已拼齐的 `ToolCall`、`Usage`、`Finished`，以及 `ItemCompleted { item_id, content }` 和 `Retrying { error, attempt, max_retries, delay }`。这些都是模型层内部事件，不是客户端协议。提供方可用 `ItemCompleted` 提前确认单项（完整内容为准）；`Finished` 确认这次响应剩余的全部项目。重试前发 `Retrying`，调用方把上次尝试的半截显示为中断，并清空未完成内容。用量包含输入、输出、总 token 数，以及提供方给出时的缓存命中、未命中和思考 token 数；缓存、思考计数是细分，不能再叠加到总量。
- `Finished` 是成功收完传输后的最后一个事件，原因是 `Stop`、`ToolCalls`、`Length` 或 `Other`。`Finished(Length)` 和 `Finished(Other)` 都表示回复不完整，核心需向用户说明。工具调用按序号拼接，id 和名字取第一个非空值、参数逐段追加，在 `[DONE]` 到达且结束原因为 `ToolCalls` 时交出。Chat Completions 调用方必须等到 `Finished(ToolCalls)` 后才执行工具；取消、错误或截断都不能执行该接口未确认的工具。其它提供方已用 `ItemCompleted` 确认的工具在可重试断流后仍照常执行一次，随后以完整项和工具结果继续请求。未完成内容不写进模型历史。
- 错误以流中的一次 `Err(ModelError)` 结束。用 `kind` 区分 `RateLimited`、`Server`、`Network`、`Timeout`、`ContextLength`、`Authentication`、`InvalidRequest` 和 `Other`；`ContextLength` 供核心触发压缩。可选的 `status`、`retry_after` 只保留安全的状态码与时长，不带提供方原始错误内容。
- 取消优先于读取和重试等待，之后直接结束流，不再返回事件或错误；调用方用自己持有的 token 区分取消与成功结束。正常成功必有 `Finished`。

## 模型历史

`Message` 可直接序列化到私有模型历史：`System { text }`、`User { text }`、`Assistant { content }`、`ToolResult { tool_call_id, text }`。`AssistantContent` 是按顺序保存的 `Text { text }`、`Reasoning { text }`、`ToolCall { id, name, arguments }`。

两种枚举使用 `type` 标签和 snake_case 值，均有 `#[serde(other)] Unknown`。旧版本读到未知种类不会整条失败，适配器跳过未知内容；未知字段本身不会被保留。工具参数始终是原样字符串，即使不是合法 JSON 也不解析、不重写，由工具层校验。

`request_body` 在转换后按助手消息和紧随其后的工具结果整理配对：删除没有对应调用的结果，保留匹配结果的原文，并在同一批工具结果末尾为缺失结果补上「没有结果：被打断或本版本无法识别」。占位结果位于下一条用户或助手消息之前；不会把隔着下一轮才出现的结果匹配回旧调用。整理仅作用于发出的请求，不改写原始模型历史。

存盘类型的演进遵守以下规则：

- 新字段一律用 `Option` 或 `#[serde(default)]`，使新版本仍能读取没有该字段的旧历史。
- 不修改已有种类字段的形状或含义；需要变更时新增消息或内容种类。
- 读出的 `Unknown` 不得写回。两个 Unknown 变体均标记 `#[serde(skip_serializing)]`，误写回会返回序列化错误，包含未知助手内容的消息也一样，避免悄悄把原内容写成 `{"type":"unknown"}`。

DeepSeek 要求带工具清单的后续请求回传所有历史助手消息的完整 `reasoning_content`，包括当时没有调用工具的消息。适配器把每条助手消息的 `Reasoning` 片段直接串接后回传，不插入空白或删除旧轮次；不带工具时也保留该字段，由 DeepSeek 忽略。调用方需保存收到的完整思考增量，不能只保存用于显示的摘要。依据：[DeepSeek 思考模式](https://api-docs.deepseek.com/guides/thinking_mode/)。

## 传输和凭证

`RetryPolicy` 是各提供方共用的重试模块；Chat Completions 用 `with_retry_policy` 注入，默认最多重试 10 次（含首次共 11 次请求）。本地退避为 0.5 秒起翻倍、最多 32 秒，向下加最多 10% 随机抖动。有效的 `Retry-After`（秒数或 HTTP 日期）优先，超过 60 秒按 60 秒，过去的日期立即重试。限流、5xx、网络和超时可重试，认证、无效请求、上下文过长不重试。没有任何已确认完整项时，无论是否出现过增量或用量，都可重发原请求；已有完整项则把错误交核心保留完整项并继续。错误的 `retries` 和错误文字报告实际重试次数。测试可注入 `RetryPolicy.sleep` 缩短等待，不改变报告给调用方的延迟。DeepSeek 的 `insufficient_system_resource`、`aborted` 仍归为 Server，`content_filter` 仍为 `Finished(Other)`；收到结束原因但没有 `[DONE]` 仍算断流。连接超时 30 秒、每次网络读取超时 300 秒不变。

本包不读取配置文件或环境变量，不使用环境代理，也不自动跟随重定向。`ApiKey` 的 `Debug` 和 `Display` 只输出遮罩；认证头标记为 sensitive，客户端 Debug 不暴露内部 HTTP 客户端。错误不保留 HTTP/SSE/JSON 底层错误链、响应正文、URL 或请求头，本包不写请求日志。

`tests/` 仅在 `127.0.0.1` 随机端口回放手写响应，凭证是固定的虚构值；不读取真实账号或用户状态，不访问模型服务。响应格式依据 [Chat Completions](https://api-docs.deepseek.com/api/create-chat-completion/)，状态码依据 [错误码](https://api-docs.deepseek.com/quick_start/error_codes/)。
