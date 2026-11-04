# 任务：第二期-29 - 接入 OpenRouter

依据：P2-200、P2-201 做完后，OpenRouter（Chat Completions，`https://openrouter.ai/api/v1`，API key，按量预充值）在 `config.json` 里加一家就能用，模型名照 `openrouter/<上游名>` 写、在第一个 `/` 处分开。可能要补的是思考内容的字段：canter 现在只认、只回传 DeepSeek 式的 `reasoning_content`（`crates/canter-model/src/wire.rs`），OpenRouter 用 `reasoning`（还可能有 `reasoning_details`）。主控看代码判断，没实测。
依赖：P2-80（排在它后面）
执行：派出
待用户：开工前定要接哪几个模型，用户自己准备 OpenRouter 账号和 key

## 用户原话

- 11-04 用户：「等一下，那么我能不能理解，现在openrouter是不是也能快速接上了？」主控答能接、不用改代码，思考字段可能有毛病；用户：「加一个任务，接入openrouter也排到第二期，gpt订阅接入的后面」。
- 11-04 用户（P2-200 时）：「我既想订阅opencode go或者openrouter这种，也想使用gpt，kimi这种coding plan的。而且可以切换这种。」

## 要做的

1. 主控在用户本机 `config.json` 里加 `openrouter` 这一家和用户选的模型（key 用户自己填）。
2. 让 OpenRouter 的思考内容能正常显示，多轮和调工具时该回传的照规矩回传；只按通用的 Chat Completions 字段处理，不按 OpenRouter 的名字或地址分支（DESIGN 第 23 条）。
3. 开工时主控补全「怎么算做完」和「不要做」，给用户看过再派。
