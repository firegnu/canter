# 任务：第二期-29.8 - 网页测试（Playwright MCP）computer use探索

2026-10-29，用户在看板上新建（原标题「第三期 - computer use」，内容空）。11-05 用户定：先做网页测试，移到第二期；桌面程序的 computer use 留第三期。
依据：用户要 canter 像 Claude Code 那样测网页，自己点、自己看。Claude Code 用的是 Playwright 插件（背后是 `npx @playwright/mcp`）；canter 已有 MCP（P2-12）和 MCP 返回截图给模型看（P2-130），只要在配置里加一项，不用写代码。
执行：主控（不派 agent，用户试用）

## 用户原话

- 11-05 用户：「另外评估一下computer use的难度有多大？这个在自动化测试里面我觉得还是比较重要的」
- 主控评估三种做法（Playwright MCP 测网页 / 先试现成的 macOS computer-use MCP / 做进 canter 内置工具）后，用户：「我觉得claude code的测试网页的还挺好的。自己点击自己测试。」
- 主控建议配置里加 Playwright MCP 试。用户：「那么改一下这个任务，把它列到第二期，然后把你上面的内容填进去。我找时间测试一下」

## 怎么试

1. 先备份 `~/.canter/config.json`，在 `mcp_servers` 里加一项，写法同现有的 `everything`：

   ```json
   "playwright": {
     "transport": "stdio",
     "command": "<本机 npx 的完整路径>",
     "args": ["-y", "@playwright/mcp@<固定版本>"]
   }
   ```

   版本写死，不用 `@latest`。11-05 查到最新是 `0.0.84`（11-03 发布），试的时候取一个发布满两周的版本。
2. 开新会话，`/mcp` 看到 `playwright connected` 和它的工具。
3. 让模型打开一个网页（本机起的开发服务器或公开网页），点几处、填个表，说出页面对不对。它会自己开一个 Chrome 窗口，能看着它点。
4. 分别用不能看图的模型（DeepSeek 等）和能看图的模型（GPT-6.1-sol）各试一次。

## 要知道的

- Playwright 主要读页面结构（按钮、输入框是文字），不靠看截图猜坐标，所以不能看图的模型也能用；要截图时，能看图的模型看得到。
- 它用单独的浏览器，没有用户平时的登录状态。Claude Code 的「Claude in Chrome」用用户自己的 Chrome（带登录），那是 Chrome 扩展，canter 接不了，也不建议接。
- 工具二十多个，占一些上下文；canter 有 MCP 工具按需加载（P2-162），影响不大。
- 试的时候真调模型，次数记进本文件。

## 试完以后

- 好用：本件收尾，写法写进使用说明（P2-228），不另做功能。
- 不好用：记下哪里不行（点不到、上下文太大、慢），再定要不要做别的。

## 第三期：桌面程序的 computer use（11-05 主控评估，留档）

测桌面程序（比如以后的 GPUI 版 canter）才需要。难处：

1. macOS 权限：截屏、控制鼠标键盘要给 canter 开「屏幕录制」「辅助功能」；canter 每次升级重新编译、签名变了，权限要重给，要固定证书签名才能免掉。
2. 模型点不准：看截图报坐标，普通看图模型常点偏，Retina 屏坐标要换算；OpenAI 专门训练过的 computer-use 能力在 ChatGPT 订阅线路给不给用没核实。
3. 安全：能点屏幕上任何东西，现有沙箱管不到，批准规则要重新设计（碰要害）。

建议到时候先用现成的 macOS computer-use MCP 加 GPT 试模型点不点得准，点得准再考虑做进 canter 内置工具。

## 进展

- 11-06 主控按用户选定（`0.0.83`、`--isolated` 每次空白浏览器、主控来加）在 `~/.canter/config.json` 加了 `playwright`，写法同 `everything`（同一个 npx 和 env）；改前备份为 `config.json.bak-20261106-before-playwright`。服务端热加载后 `/v1/mcp` 报 `playwright connected`，25 个工具。还没真调模型试。
- 11-06 用户按主控给的四个测试试完（example.com 打开和点链接、the-internet 登录退出、TodoMVC 加三条勾一条查计数、换 GPT 再跑一遍加截图看图），DeepSeek 和 GPT 各一轮，共四五次对话。用户：「全部都测试了，效果非常好。」

## 结论

好用，本件收尾，不另做功能。配置写法（固定版本、`--isolated`、`env` 里给 PATH、每个工具第一次要批准）由 P2-228 写进 `docs/usage.md`，主控在 P2-234 那条通知里一并交给 omp。桌面程序的 computer use 仍留第三期。
