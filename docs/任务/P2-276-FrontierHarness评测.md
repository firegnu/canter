# 任务：FrontierHarness Eval 评测 canter（占号）

依据：11-11 用户给了 https://frontierharness.org/evaluate-your-own-harness/ ，说「我觉得这个比较专业」，让主控先探测，探测完记成一件。走哪条路还没定，用户定了再写全。
依赖：P2-274（canter 在 Terminal-Bench 2.1 上的成绩）；可能和 P2-275 合并
待用户：选下面哪条路

## 用户原话

- 11-11：「现在有一个专门评测harness的eval，你看一下。https://frontierharness.org/evaluate-your-own-harness/ 我觉得这个比较专业。你先探测一下」
- 11-11：「把这个记成一个task，https://frontierharness.org/evaluate-your-own-harness/」

## 探测结果（11-11）

来源：网站和它的数据接口（`/api/summary`、`/api/live`），源码仓库 `github.com/frontier-harness-eval/eval`（`e837a70`，2026-09-08，只读克隆在 `/tmp/fhe-eval`）。

- **是什么**：Runta 主办（Runta 出沙箱，Baseten 出模型），固定模型、换外壳，比通过率、花费和速度。只算判分通过的；模型失败和超时算 0；基础设施失败不计分、单列。
- **v1.0（已冻结，仓库里就是这一版）**：只用 Kimi K3（Fireworks），30 题（Terminal-Bench 2.1 的 21 题 + DeepSWE 9 题），9 个外壳、12 种配置，每题 1 次。Codex 66.7%、Claude Code 63.3%、pi 60.0%；每过一题的花费中位数从 $1.05 到 $18.34。
- **v1.1（11-10～11-14 正在跑）**：21 个外壳（含 Claude Code、Codex、pi、oh-my-pi、OpenCode、DeepSeek 自家外壳 4 种、mini-SWE-agent 当基线），5 个模型（GLM-5.3、GLM-5.3-Flash、DeepSeek-V4.1-Flash-1017、Muse Spark Open、Kimi K3），62 题（Terminal-Bench 2.1 的 45 题 + Terminal-Bench 4.x 的 17 题），每题 4 次，思考档 max，1～4 核、2～8 GB 内存，外壳时限 900 秒～8 小时。
- **和 canter 的关系**：v1.1 的 45 道 2.1 题都在 P2-274 跑过的 89 题里。canter 在这 45 题上判分 38/45，扣掉看答案的 3 题（dna-assembly、make-doom-for-mips、video-processing）是 35/45（78%）。DeepSeek-V4.1-Flash 的价格（输入 $0.30、输出 $1.20）和 canter 的 `deepseek-flash` 一样，很可能是同一个模型，版本要核对。
- **「评测你自己的外壳」的公开流程是 v1.0 的**，要：Runta 账号（商业云，有试用）；Kimi K3 的 key（Fireworks、Moonshot、OpenRouter、Together 之一，按 $3 / $15 每百万 token 计价）；把 key 存进 Runta 的云端保管（和「凭证只放本机」冲突）；算花费的脚本要 Python（超出现在只给 `bench/terminal-bench/` 适配器开的例外）；把 canter 注册进 Harbor 0.22 和 Pier（DeepSWE 用，要新适配器）。新提交的结果默认 `methodology_comparable: false`、不进排名，要在同样环境里再跑一个对照外壳才能说可比。
- **网络**：他们的放行名单里有 GitHub、PyPI、npm、apt 源等，外壳能上 GitHub；文档里没看到偷看答案的检查。

## 可选的路（待用户选）

1. **不花钱粗比**：等 v1.1 跑完（11-14），拿 canter 在 P2-274 里那 45 题的成绩，和各外壳用 DeepSeek-V4.1-Flash 的成绩比。不严格：他们每题 4 次、max 档、Runta 沙箱；我们 1 次、high 档、5090。
2. **走官方 v1.0 流程**：Runta + Kimi K3 + 对照外壳。要花钱、要交出 key、要 Python 例外，结果仍不进排名。主控不推荐。
3. **在 5090 上照 v1.1 的设置跑那 62 题**：DeepSeek Flash、max 档、每题 4 次。和 P2-275（4.1）重叠，可以合到一起；先试跑几题再估花费。

主控推荐先走 1，要严格对比再考虑 3。

## 不要做

- 不改 canter 代码、不为这套题调配置。
- 没有用户同意不注册 Runta、不买 Kimi K3、不把 key 交给第三方保管。
- 不在 `bench/terminal-bench/` 以外写 Python。
