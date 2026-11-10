# Terminal-Bench 2.1 / canter

用官方 **Harbor 0.25.0** 在 x86_64 Linux Docker 中运行
`terminal-bench/terminal-bench-2-1`（89 题）。不改题目、资源、超时、提示词或评分。
本目录不属于 Cargo workspace。Python 仅用于适配 Harbor、采集结果和检查答案来源。

## 安装（在仓库根目录，Bash）

宿主机需要可直接使用的 Docker（含 Compose）、Git、curl、jq；无需 sudo。
下列工具、临时目录、数据和结果放在 `BENCH_ROOT`，不修改 shell 配置。

```bash
export BENCH_ROOT="$HOME/canter-linux"
export UV_INSTALL_DIR="$BENCH_ROOT/uv-bin"
export UV_TOOL_DIR="$BENCH_ROOT/uv-tools"
export UV_TOOL_BIN_DIR="$BENCH_ROOT/uv-bin"
export UV_CACHE_DIR="$BENCH_ROOT/uv-cache"
export UV_PYTHON_INSTALL_DIR="$BENCH_ROOT/python"
export UV_PYTHON_BIN_DIR="$BENCH_ROOT/uv-bin"
export TMPDIR="$BENCH_ROOT/tmp"
export XDG_CONFIG_HOME="$BENCH_ROOT/harbor-home/.config"
export XDG_CACHE_HOME="$BENCH_ROOT/harbor-home/.cache"
export HARBOR_TELEMETRY=0
mkdir -p "$TMPDIR" "$UV_INSTALL_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
curl -fsSL https://astral.sh/uv/0.13.7/install.sh -o "$BENCH_ROOT/uv-install.sh"
UV_NO_MODIFY_PATH=1 sh "$BENCH_ROOT/uv-install.sh"
"$UV_INSTALL_DIR/uv" python install 3.12.16
"$UV_INSTALL_DIR/uv" tool install --managed-python --python 3.12.16 'harbor==0.25.0'
export PATH="$UV_INSTALL_DIR:$PATH"
harbor --version
export PYTHONPATH="$PWD/bench/terminal-bench${PYTHONPATH:+:$PYTHONPATH}"
export BENCH_PYTHON="$UV_TOOL_DIR/harbor/bin/python"
```

不登录 Harbor，不使用 upload/push。Harbor 的 Docker 镜像保存在 Docker 自己的目录。
Harbor 0.25.0 的部分缓存常量固定在 `~/.cache/harbor`，不遵循 XDG；这里下载时明确
指定导出目录，运行本地路径，避免依赖默认题目缓存。若其它 Harbor 功能产生默认缓存，
应单独记录，不能用清理整个 Docker 或用户缓存的方式收尾。

## 编译干净提交的 musl 静态版

本次基线为 `64b80e3f963f0005f69f078e68a4af71a611ca28`。编译目录与适配器工作区
分开；完整 clone 保证 Docker 中能读取 `.git` 并正确生成版本号。

```bash
export CANTER_COMMIT=64b80e3f963f0005f69f078e68a4af71a611ca28
git clone --no-hardlinks "$PWD" "$BENCH_ROOT/canter-build-source"
git -C "$BENCH_ROOT/canter-build-source" checkout --detach "$CANTER_COMMIT"
test -z "$(git -C "$BENCH_ROOT/canter-build-source" status --porcelain)"
docker build -t canter-terminal-bench-musl - <<'DOCKERFILE'
FROM rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900
RUN apk add --no-cache build-base cmake perl linux-headers git
DOCKERFILE
mkdir -p "$BENCH_ROOT/musl/cargo" "$BENCH_ROOT/musl/home" \
  "$BENCH_ROOT/.target/p2-260-terminal-bench/musl"
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$BENCH_ROOT/canter-build-source:/src:ro" \
  -v "$BENCH_ROOT/musl:/build" \
  -v "$BENCH_ROOT/.target/p2-260-terminal-bench/musl:/target" \
  -w /src -e HOME=/build/home -e CARGO_HOME=/build/cargo -e CARGO_TARGET_DIR=/target \
  canter-terminal-bench-musl cargo build --locked --release -p canter \
  --target x86_64-unknown-linux-musl
export CANTER_BINARY="$BENCH_ROOT/.target/p2-260-terminal-bench/musl/x86_64-unknown-linux-musl/release/canter"
"$CANTER_BINARY" --version
file "$CANTER_BINARY"
sha256sum "$CANTER_BINARY"
```

## 数据与 oracle

先下载官方数据，再以本地路径运行。导出物不改动、不放进仓库，也不上传到 agent
容器。Harbor 只在验证阶段提供测试；oracle 的日志和答案不会交给 canter。

```bash
harbor datasets download terminal-bench/terminal-bench-2-1 \
  --output-dir "$BENCH_ROOT/p2-260-data" --export
export TB_DATA="$BENCH_ROOT/p2-260-data/terminal-bench-2-1"
export TB_JOBS="$BENCH_ROOT/p2-260-jobs"
for task in fix-git openssl-selfsigned-cert polyglot-c-py; do
  harbor run --path "$TB_DATA/$task" --agent oracle \
    --n-concurrent 1 --max-retries 0 --jobs-dir "$TB_JOBS" --job-name "oracle-$task"
done
jq '{task_name, verifier_result, exception_info}' "$TB_JOBS"/oracle-*/*/result.json
```

这三题分别是 Git 恢复、证书生成和 C/Python 文件，预计较短。官方 2.1 导出物的
Dockerfile 中没有 Alpine 基础镜像；`qemu-alpine-ssh` 和 `qemu-startup` 是 Debian
容器里的 Alpine 虚拟机，不能算 canter 运行在 busybox ash 上。

## canter 参数、凭证与试跑

`--model` 的 `provider/model` 决定 canter 配置中的服务商和模型。`endpoint`、
`key_env`、`model_settings`、`reasoning_effort` 均由参数提供，适配器没有 DeepSeek 分支。
`deepseek-flash.json` 是本轮模型能力和价格：上下文 1,048,576，图片能力，以及 canter
基线价表的美元价格。没有设置压缩阈值、最大轮数或工具策略；其它行为沿用 canter 默认。
换模型时提供它自己的模型能力/价表；没有价格时用量中的美元是未知，不当作零。

key 只从宿主机环境变量读取，**不要用 `--ae KEY=value` 或 `--ak api_key=...`**。
可以交互读入（不进 shell 历史），或从已获授权的本机凭证源读取；不能把值写进脚本。

```bash
read -rs -p 'API key: ' CANTER_BENCH_API_KEY; printf '\n'
export CANTER_BENCH_API_KEY
export TB_MODEL=deepseek/deepseek-flash
export TB_ENDPOINT=https://api.deepseek.com
export TB_EFFORT=high
export TB_MODEL_SETTINGS="$(jq -c . bench/terminal-bench/deepseek-flash.json)"
```

每次只跑一题，跑完检查凭证，再继续下一题。首次完成三题即可；适配器返工重跑总数
不得超过三次，真实模型题目运行总数不得超过六次。`--max-retries 0` 关闭 Harbor 的
整题重试，canter 自身默认请求重试不变。

```bash
task=fix-git # 然后依次换成 openssl-selfsigned-cert、polyglot-c-py
harbor run --path "$TB_DATA/$task" --agent-import-path canter_agent:Canter \
  --model "$TB_MODEL" --ak "binary_path=$CANTER_BINARY" \
  --ak "endpoint=$TB_ENDPOINT" --ak key_env=CANTER_BENCH_API_KEY \
  --ak "model_settings=$TB_MODEL_SETTINGS" --ak "reasoning_effort=$TB_EFFORT" \
  --n-concurrent 1 --max-retries 0 --jobs-dir "$TB_JOBS" --job-name "canter-$task"

# 只输出文件名；grep 的 1 表示无命中，2 表示扫描出错。
# pattern 从管道输入，不把真实 key 放入 grep 参数或任何文件。
grep -rlF -f <(printf '%s\n' "$CANTER_BENCH_API_KEY") "$TB_JOBS/canter-$task"
grep -rlF -f <(printf '%s\n' "$CANTER_BENCH_API_KEY") "$BENCH_ROOT"
```

检查命中必须停下交主控处理。若 key 来自原已授权的本机配置，完整扫描当然也会命中
该源文件：应把它作为既有凭证源明确报告，不能把它混同于新产生的日志泄露，也不能
隐瞒其它命中。运行结束 `unset CANTER_BENCH_API_KEY`，不要删除用户的原始凭证源。

适配器在 `setup()` 中先调用 Harbor 0.25.0 父类安装，再生成容器配置。该正式版没有
主分支的 `configure()` 钩子。配置用 Docker `exec -T` 的 stdin 内存管道传输，权限
0600；命令行没有 key，因此 Docker 的异常不会包含 `-e KEY=value`。该通道使用钉住
版本的 Docker 私有接口，所以升级 Harbor 必须重新核对。

`canter exec --yolo` 的 stdout、`canter stats --last --json` 和 `sessions/` 先留在
容器内，经不带流式日志回调的 stdin/stdout 通道采集，在内存中替换意外出现的 key
后才写宿主机。不会下载 `config.json` 或整个 `.canter`。finally 中删除配置；Harbor
负责删容器。`agent/collection.json` 记录替换次数，非零必须人工查看。
Harbor 的 `n_input_tokens` 包含缓存 token；美元直接采用 canter stats，不重算。

## 结果与偷看答案检查

```bash
"$BENCH_PYTHON" bench/terminal-bench/report.py \
  --dataset terminal-bench/terminal-bench-2-1 \
  --oracle-job "$TB_JOBS/oracle-fix-git" \
  --oracle-job "$TB_JOBS/oracle-openssl-selfsigned-cert" \
  --oracle-job "$TB_JOBS/oracle-polyglot-c-py" \
  "$TB_JOBS/canter-fix-git" "$TB_JOBS/canter-openssl-selfsigned-cert" \
  "$TB_JOBS/canter-polyglot-c-py" > bench/terminal-bench/results/pilot.json
jq '.trials[] | {task, reward, cost_usd, answer_source_check}' \
  bench/terminal-bench/results/pilot.json
```

`answer-source-rules.json` 是共用规则，覆盖 Harbor 的 GitHub、Terminal-Bench 相关仓库、
tbench.ai、Harbor Hub 和 Hugging Face 的 benchmark 数据集组织。`report.py` 扫描每个
trial 的 `agent/` 下所有 JSON、JSONL、txt：canter 的原始会话、模型历史、工具调用、
工具返回和 stdout；Terminus-2 的 `trajectory.json` 及文本日志也走同一套规则。
JSON 会递归检查字符串，JSONL 按记录检查，包含网页、搜索、shell 命令和输出里的来源。

报告保存每题是否命中、规则 ID、匹配来源、文件名、行号（整份 JSON 用 1）和规则文件
SHA256。日志缺失或 JSON 损坏会标为 `incomplete`，不会报告为“检查通过”。命中只表示
需要主控人工判断，可能只是提到一个 URL；不自动改分。无命中不证明绝对没有偷看，
不能识别未记录的访问、混淆地址或模型训练中已有的答案。本件只实际检查 canter 试跑。

## 全套与 Terminus-2（本件不执行，先报估算并获同意）

沿用上述环境变量，保留题目默认超时和资源：

```bash
harbor run --path "$TB_DATA" --agent-import-path canter_agent:Canter \
  --model "$TB_MODEL" --ak "binary_path=$CANTER_BINARY" \
  --ak "endpoint=$TB_ENDPOINT" --ak key_env=CANTER_BENCH_API_KEY \
  --ak "model_settings=$TB_MODEL_SETTINGS" --ak "reasoning_effort=$TB_EFFORT" \
  --n-concurrent 4 --max-retries 0 --jobs-dir "$TB_JOBS" --job-name canter-full

# Terminus-2 的 LiteLLM 模型名使用同一个 DeepSeek 官方模型。
# 只传环境变量名，不传值；也不使用 --ae。
harbor run --path "$TB_DATA" --agent terminus-2 --model deepseek/deepseek-flash \
  --api-key-name CANTER_BENCH_API_KEY --base-url https://api.deepseek.com \
  --ak reasoning_effort=high --n-concurrent 4 --max-retries 0 \
  --jobs-dir "$TB_JOBS" --job-name terminus2-full

"$BENCH_PYTHON" bench/terminal-bench/report.py \
  --dataset terminal-bench/terminal-bench-2-1 "$TB_JOBS/canter-full" > canter-full.json
"$BENCH_PYTHON" bench/terminal-bench/report.py \
  --dataset terminal-bench/terminal-bench-2-1 "$TB_JOBS/terminus2-full" > terminus2-full.json
```

Terminus-2 命令只按正式版接口写出，尚未真跑或验证其凭证日志路径；第二步同样需要
每轮凭证检查。原始日志留在运行机器，不进仓库。镜像保留，不执行 prune。

比较两个外壳或两轮的汇总、逐题差异和答案来源命中：

```bash
jq -s 'map({agent, model, score, totals})' canter-full.json terminus2-full.json
jq -s '.[0].trials as $a | .[1].trials as $b |
  $a[] as $x | $b[] | select(.task == $x.task) |
  {task, canter: $x.reward, other: .reward,
   canter_usd: $x.cost_usd, other_usd: .cost_usd,
   canter_seconds: $x.agent_seconds, other_seconds: .agent_seconds}' \
  canter-full.json terminus2-full.json
jq '.trials[] | select(.answer_source_check.matched != false) |
  {task, answer_source_check}' canter-full.json terminus2-full.json
```

参考：Harbor 0.25.0 的 `BaseInstalledAgent`、`Pi`、Docker 后端的 stdin 上传和
`AgentContext`；只参考接口，没有复制实现。依赖和源码：[Harbor PyPI](https://pypi.org/project/harbor/0.25.0/)、
[官方仓库](https://github.com/harbor-framework/harbor)。
