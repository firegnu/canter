# 任务：用 DeepSeek OCR 读扫描版 PDF、图片里的文字

2026-11-14 记下，还没写需求，没派。
优先：低

## 用户原话

- 11-14：「deepseek昨天开放了很多api，你查一下咱们能用哪些集成到canter中？」
- 11-14 主控列了三件，用户：「记第1、2件吧」（这是第 1 件）。

## 已查到的（11-14）

- DeepSeek 11-13 开放独立 OCR：`POST https://api.deepseek.com/v1/tools/ocr`，只收 `multipart/form-data`，字段 `file` 和 `purpose`（`text_recognition` 或 `rich_layout_recognition`）。支持 PDF、Word、PowerPoint、PNG/JPEG/WebP/GIF/BMP/TIFF；单文件 100 MiB、1000 页；上传 120 秒内传完，整体 180 秒；每个账号同时 15 个。返回 `pages[]`（`index`、`text`、`finish_reason`），版面识别的文字里带 `<｜ref｜>`、`<｜box｜>` 坐标标记；识别中失败也回 HTTP 200，要看 `error` 字段。每千页 $0.225，只算成功的页。文件不进 Files API，也不交给对话模型。文档：https://api-docs.deepseek.com/guides/ocr
- canter 现在：`read` 读 PDF（P2-226）在本机抽文字，没有 OCR；扫描件只能用 `page` 参数转成图片给模型看，模型不支持图片就读不了。
- 主控提的方向：和 `web_search`（P2-106）一样，只在配了 DeepSeek 官方 API 时提供，算执行、要批准（文件会上传给 DeepSeek），核心不依赖它。具体做成新工具还是 `read` 的选项、哪些文件可以传，写需求时再和用户定。
