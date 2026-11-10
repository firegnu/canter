# 任务：第二期 - eval 接 Terminal-Bench 2.0

依据：P2-151「11-09 Terminal-Bench 排在第一版之后」。
依赖：P2-257、P2-258、P2-259、P2-160、P2-272
待用户：第一版跑通后再补全。

## 用户原话

- 11-09：「记进去，第一版跑通后接着做」

- 11-10（看完 benchmark 评估）：「我觉得可以先做160。」「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.」
- 11-10（问过能不能在 macOS 上跑、能不能接进现在的 eval 架构）：「还是走正常的bench吧。不要投机取巧了」

## 11-10 评估的结论（主控）

- 评的是 canter 外壳，模型固定 DeepSeek v4.1 flash。用 Terminal-Bench 2.1（89 题，官方运行框架 Harbor），不用 4.x（太难，flash 只有 20～31%，单题很长）和 SWE-bench Verified（OpenAI 26-02 起认定污染）。SWE-bench Pro 排第二。
- 和同一模型下的别家外壳并排跑：Harbor 自带 Terminus-2（官方中立外壳）、Claude Code（接 DeepSeek 的 Anthropic 兼容接口，不占 Claude 订阅）、opencode、pi；Codex 能不能接 DeepSeek 到时查。
- 外壳带来的差别往往只有几个百分点，89 题跑一次误差约 ±10 个百分点：只看得出明显差距，要跑几次、按题成对比。压缩、插话、子 agent、记忆、缓存照旧靠自己的 eval。
- canter 装进每道题的 Linux 容器，用 `canter exec "<题目>" --yolo` 跑（和别家的 `-p`、`codex exec` 一样，没人批准、没人回答）。要 Linux 静态版（P2-160），在 5090（`my-5090d-host`，x86_64 原生）上跑。
- 适配器是 Harbor 要的一小段 Python（`BaseInstalledAgent`），放 canter 仓库外。

## P2-160 留给本件的（11-10）

- 静态版从干净的已提交代码编（P2-160 验收时的 `--version` 显示的是开发自己同步仓库的 `3690057-dirty`）。编译命令见 DESIGN「Linux 支持（P2-160）」。
- 题目容器要先装 `ca-certificates`（Debian/Ubuntu `apt-get`，Alpine `apk`），缺了服务端报错退出。
- 看护进程杀进程组的写法在 Alpine 的 busybox ash 上没验过，在 Alpine 题目里跑时看一眼。

## 要做的

（占号。要解决：Harbor 在 Docker 里跑，canter 是常驻服务端 + 客户端；适配器是一小段 Python，放 canter 仓库外；约 90 件，先拿几件试出花费和时间；比外壳要让对方用同一个模型。）
