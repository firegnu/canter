# 任务：Terminal-Bench 2.1 全套 89 题（第二步：oracle、canter、Terminus-2 各跑一遍）

依据：P2-260 已合并（Harbor 0.25.0 适配器、三题试跑、偷看答案检查）。本件在 5090 上用同一模型把全套 89 题跑三遍，出 canter 的绝对分数和与 Terminus-2 的对比。不改 canter 代码、不改适配器、不改题。
依赖：P2-260
执行：主控
待用户：看本文件；镜像拉完后说「开始跑」；定无人值守那条（见「待用户定」）

## 用户原话

- 11-10：「还是走正常的bench吧。不要投机取巧了」
- 11-10（主控推荐第一轮比 canter 和 Terminus-2、全套前先报估算、镜像跑完前留着）：「四个问题按你的推荐来」
- 11-10：「我想拿到真实的成绩」；偷看答案检查：「加吧，按你的建议来」
- 11-10（主控提流程：先 oracle 跑 89 题，再 canter，再 Terminus-2；4 并发；环境出错的题重跑一次、两个分数都报；每遍 $10 上限；主控执行，用户说「开始跑」再开始）：「其他没问题」
- 11-10（主控说三遍里只有 canter、Terminus-2 两遍花钱，$10 是各自的上限）：「明白了。」
- 11-10：「可以，现在就开始拉吧」（提前拉 89 个题目镜像）

## 在哪里干活

- 不开 worktree：本件不改代码。结果数据和本文件的完成记录直接提交到 main（按路径 `git add`）。
- 5090（`my-5090d-host`）：只用 `~/canter-linux/`，环境变量照 `bench/terminal-bench/README.md`「安装」一节。适配器从 5090 上一份干净的 main 克隆里取（`~/canter-linux/p2-274-src`，检出合并后的 main，`git status` 干净）。原始日志放 `~/canter-linux/p2-274-jobs/`，不进仓库。
- canter 静态版：沿用 P2-260 试跑那个（`64b80e3f`，`~/canter-linux/.target/p2-260-terminal-bench/musl/...`）。P2-260 之后 canter 的 Rust 代码没改过，和 main 是同一份；开跑前 `--version`、`sha256sum` 核对一次，写进结果数据。
- key：照 P2-260，从 5090 测试 HOME 的 `~/canter-linux/accept-home/.canter/config.json` 用 `jq` 读进环境变量，不打印、不写进任何文件和脚本。

## 要做的

0. **开跑前**：89 个镜像都拉完（`~/canter-linux/p2-273-pull.log` 有 DONE、没有 FAIL）。Terminus-2 先真跑 1 题（fix-git）确认三件事：凭证不落盘（`grep -rlF` 只命中既有授权源）；`reasoning_effort=high` 真的传到了 DeepSeek（日志里有思考 token）；Harbor 给的花费是不是空的（LiteLLM 可能不认识 `deepseek-flash` 的价格）。花费为空就按下面「花费口径」自己算。这一题算 Terminus-2 的检查，不算成绩。
1. **oracle 跑 89 题**：`--agent oracle`，4 并发，不调模型。没过的题单列，查是题目环境的问题还是网络、镜像的问题。
2. **canter 跑 89 题**：README「全套」一节的命令，4 并发，`--max-retries 0`，思考档 high，超时和资源用题目默认。
3. **Terminus-2 跑 89 题**：同一模型 `deepseek/deepseek-flash`、high、同一地址，4 并发，`--max-retries 0`。
4. **环境出错的题重跑一次**：每遍跑完，挑出因为环境出错没跑起来的题（Harbor 起不了容器、拉不了镜像、`docker compose` 失败这类，不含 `AgentTimeoutError` 和外壳自己报错退出），用单独的 job 重跑一次。报两个分数：只算第一次的，和用重跑结果替换后的。外壳自己出错、超时的照算 0 分，不重跑。
5. **$10 上限**：canter、Terminus-2 两遍各自盯：每完成一题累加花费，超过 $10 就按记下的 Harbor PID 发 TERM 停下（不按名字杀），已完成的照常出报告、没跑的列为缺题，然后停下来告诉用户。盯的办法写成 5090 上 `~/canter-linux/p2-274-run/` 里的 shell 脚本，不进仓库。
6. **每遍跑完的检查**：key 扫描（`grep -rlF`，pattern 从管道输入，只出文件名；只该命中既有授权源，有别的命中就停下）；`report.py` 带 `--tasks-dir` 出结果，canter 和 Terminus-2 都带 `--oracle-job` 比校验和；偷看答案检查有命中的逐题看，确认抄了答案的单列、按 0 分算（两个外壳同一套规则）；`metadata.errors` 不为空、`server_stopped_for_sub_agents` 为真的题逐题看并写进完成记录。
7. **结果进仓库**：`bench/terminal-bench/results/` 下每遍一个 JSON（oracle、canter、Terminus-2，有重跑的另加重跑的），加一份对比文件，写：两边总分（第一次 / 含重跑）、逐题谁过谁没过、花费、用时、token，以及只算 oracle 通过的那些题时的分数。

### 花费口径

- canter 用 `canter stats` 的美元（按请求时间分高峰、低谷价）。
- Terminus-2：Harbor 给了美元就用；给的是空或 0，就用同一张价表（`bench/terminal-bench/deepseek-flash.json`）按 token 算，按每题开始时间判断高峰、低谷价，并写明是自己算的。
- 对比时另给一列「全按高峰价」的花费，免得白天跑和夜里跑的价格差混进外壳之间的比较。

## 估算（照 P2-260 试跑）

- canter 一遍：乐观 $1.86；4 并发乐观约 1.1 小时。上限：89 题的超时加起来，4 并发约 10.6 小时，最长一题单独就要 3.3 小时。Terminus-2 一遍应在同一量级，没跑过，跑完 fix-git 那题再估。
- oracle 一遍不花钱，不需要模型思考，应比 canter 快很多。
- 三遍连着跑，大约 3～20 小时。DeepSeek 北京时间夜里是低谷价，大约便宜一半。

## 待用户定

- **无人值守**：项目规矩写「真调 DeepSeek 只在用户在场、同意时做」。三遍连着跑可能要跑一夜。主控建议：用户说「开始跑」就算同意，三遍写成一个后台脚本连着跑；$10 上限和 key 扫描由脚本自己做，key 扫描有新命中就停；主控不用一直开着，跑完由主控（或新会话）汇总。另一个办法是每遍跑完停下，等用户在场再跑下一遍。

## 怎么算做完

- oracle、canter、Terminus-2 三遍都跑完（或碰到 $10 上限、key 扫描命中按规矩停下并报告）。
- `bench/terminal-bench/results/` 有每一遍的结果数据和对比文件；本文件末尾的完成记录写清：分数、花费、用时、重跑了哪些题、偷看答案命中怎么判的、`metadata.errors` 和停服务端的题、和估算差多少。
- 推送前 gitleaks、trufflehog 扫过；推送等用户说。

## 不要做

- 不改 canter 代码、不改适配器、不改题、不放宽超时和资源、不按题调配置；跑出 canter 或适配器的问题，记下来另开任务，不边跑边改。
- 不跑 Terminus-2、canter 以外的外壳，不跑第二轮（多跑几轮看第一轮结果再说）。
- 不上传到 Harbor Hub，不登录 Harbor 账号。
- 5090 上不用 sudo，不碰别人的容器和镜像，不 `docker system prune`；题目镜像跑完先留着，问用户再清。
- 不按名字批量杀进程；停 Harbor、停盯花费的脚本都用记下的 PID。
