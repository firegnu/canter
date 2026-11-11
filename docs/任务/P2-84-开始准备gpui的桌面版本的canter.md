# 任务：第二期 - 开始准备gpui/web的桌面版本的canter client

2026-10-27，用户在看板上新建。

## 用户原话

## 要做的

## 怎么算做完

## 11-11

- 11-11：「先把eval三条规矩写进DESIGN。我的理解是，canter接下来真实任务我想让canter去做一个canter的gpui或者web的客户端（和tui功能对齐）以及我现在真实项目的任务。 RSI按照你的建议不单独做。另外贴身助理也按照你的建议先写一份方案附加到那个任务上。另外垂直领域那个暂时不做。界面这尤其是tui这块也按照你的建议来。」
- 用户打算把「GPUI 或网页客户端，和终端界面功能对齐」当作让 canter 自己做的真实任务。主控建议：若走 P2-98 贴身助理，网页版优先（手机能打开），GPUI 往后放。
- 11-11 用户：「其实我还有一个目的是我想在ipad上使用这个，但是我还没想好怎么规划这个。」用户已用 iPad 上的 Terminus SSH 到 5090 跑终端版试过，有 iPad 的 Magic Keyboard。
- 主控意见：GPUI 不支持 iPad，要在 iPad 上用就走网页版（Safari，可加到主屏幕）。网页版要先在服务端开本机端口加登录校验（安全相关，按碰要害派给 Codex 并交叉审查，不交给 canter），canter 再写网页。为 iPad 要多考虑：断线后重开原样、触屏能点批准和停止、横竖屏排版。远程入口和服务端放哪台（5090 一直开着）跟 P2-98 第二块一起定。用户还没想好怎么规划，等用户说再写任务单。
- 11-11 用户：「我觉得terminus ssh上去之后体验还好。」iPad 暂时用 SSH 加终端版就够，客户端不急。
- 11-11 用户：「我主要是想canter接一个独立的repo，这样我可以检验canter的能力。gpui我想请canter独立来做。……另外canter本身的开发，我还是会继续。」又定：「就叫canter-desktop，私有仓库，AGENTS.md你写吧」。
- 主控建了 `~/Developer/personal_projs/canter-desktop`（GitHub 私有仓库 `firegnu/canter-desktop`），只放了一份 `AGENTS.md`：目标（基础版到功能对齐）、协议来源（git 依赖固定提交引用 `canter-protocol`）、测试用临时 `CANTER_HOME` 和假模型、不改 canter 仓库（缺接口记进那边的 `docs/canter需求.md` 交用户）、会话环境变量怎么拿先问用户。具体怎么做由 canter 自己定；没提 paddock 的代码（canter 卡住来问再说）。
- 那边提来的接口需求，回到本仓库当一件活来做。用户用 canter 做那个项目时碰到的模型做事失败，按 DESIGN「用 eval 来改」第 2 条攒成 eval 题。
- 11-11 用户：不给 canter 接 cairn，「正好测试canter的记忆功能」；又说「记住这个不仅仅是测试哦，还是做真实任务的」。canter-desktop 首先是要交付能用的客户端，其次才是检验 canter 的能力和记忆。canter 卡住时可以帮（比如指给它 paddock 的 GPUI 输入框），但不替它做。
