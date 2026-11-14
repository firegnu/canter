# 任务：web_search 用上 DeepSeek 搜索接口的全文和搜索语法

2026-11-14 记下，还没写需求，没派。
优先：低

## 用户原话

- 11-14：「deepseek昨天开放了很多api，你查一下咱们能用哪些集成到canter中？」
- 11-14 主控列了三件，用户：「记第1、2件吧」（这是第 2 件）。

## 已查到的（11-14）

- canter 的 `web_search`（P2-106）调 `POST https://api.deepseek.com/v1/tools/search`，现在只传 `query`、`count`，模型拿到标题、网址、日期、summary。
- 接口文档（https://api-docs.deepseek.com/guides/search）里还有：
  - `need_content`（缺省 false）：为 true 时每条结果带网页正文 `content`，可能省掉一次 `web_fetch`；
  - `ori_question`：用户原问题，帮助理解查询；
  - `query` 支持 `site:`、`-site:`、`lang:`、`after:`、`before:` 这些语法，1～128 字符；`count` 1～50；
  - 结果还有 `snippet`、`site_name`、`authority`（0～4）、`rerank_score`。
  - 价格不变：每千次成功请求 $0.75，失败和空结果不算钱。
- 写需求时要定：`need_content` 是模型自己选还是固定开（正文会撑大上下文，现在有 32 KiB 头尾截断）；搜索语法写不写进工具说明；改工具说明会冷一次缓存。
