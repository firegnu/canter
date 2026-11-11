# Terminal-Bench 2.1 全套对比（P2-274）

5090，Harbor 0.25.0，89 题，4 并发，`--max-retries 0`。每遍跑完更新本文件。

## 总分

| 外壳 | 结果文件 | 总分（第一次） | 含重跑 | 只算 oracle 通过的 78 题 | 花费 | 全按高峰价 | 用时（墙钟） | token |
|---|---|---|---|---|---|---|---|---|
| oracle | `oracle-full-2026-11-10.json` | 78/89（87.6%） | 无重跑 | 78/78 | 不调模型 | — | 2 小时 28 分 | — |
| canter | `canter-full-2026-11-10.json` | 判分 70/89（78.7%）；扣掉看答案的 6 题 **64/89（71.9%）**；再扣存疑 1 题 63/89（70.8%） | 无重跑 | 判分 65/78（83.3%）；扣 5 题 **60/78（76.9%）**；再扣存疑 59/78（75.6%） | $2.88（全在低谷价） | $5.76 | 4 小时 55 分 | 输入 2.02 亿（缓存命中 1.99 亿，98.7%），输出 313 万 |
| Terminus-2 | 未跑 | | | | | | | |
| pi | 未跑 | | | | | | | |

## oracle 没过的 11 题

都不是外壳能决定的；没有一题属于「Harbor 起不了容器、拉不了镜像、compose 失败」，所以按规矩不重跑。

| 题 | 怎么没过 | 原因 |
|---|---|---|
| mteb-retrieve | 判分超时（1800 秒） | 判分脚本现装 mteb、torch，5090 到 PyPI 约 330 KB/s，装不完 |
| pytorch-model-recovery | 判分超时（900 秒） | 同上，现装 torch |
| torch-pipeline-parallelism | 判分超时（900 秒） | 同上，现装 torch、transformers |
| torch-tensor-parallelism | 判分超时（900 秒） | 同上，现装 torch |
| gcode-to-text | 标准答案超时（900 秒） | 标准答案 pip 装 opencv（67 MB）等，下载太慢 |
| qemu-alpine-ssh | 判分 0 分 | 判分脚本 `apt-get install curl`，Debian 安全源里那个版本已 404，curl 没装上，判分没跑起来 |
| qemu-startup | 判分 0 分 | 同上 |
| caffe-cifar-10 | 判分 0 分 | 标准答案装 Ubuntu 包时 404，caffe 没编出来 |
| mcmc-sampling-stan | 判分 0 分 | 标准答案装 rstan 时依赖 RcppParallel 的新版要 cmake，镜像里没有 |
| build-cython-ext | 判分 0 分（11 条挂 1 条） | `test_pyknotid_repository_tests` 没过，疑似依赖没钉版本 |
| protein-assembly | 判分 0 分 | dnachisel 求解器这次没解出来（`NoSolutionError`） |

前 5 题跟 5090 的网速有关，换外壳也一样；qemu 两题、caffe 是软件源删了旧版本；后 3 题是依赖漂移或题目本身不稳定。

## canter（11-10 21:11～11-11 02:06 北京时间）

`canter 0.1.0 · 64b80e3f`，`deepseek/deepseek-flash`，思考档 high。没有 `metadata.errors`、没有停服务端的题、没开子 agent；89 题的题目校验和都和 oracle 那遍一致；key 扫描只命中凭证源本身。

### 看答案（逐题看过）

canter 在 10 题里用 `web_search` 搜到了题库（GitHub 上的 `harbor-framework/terminal-bench-2` 和几个镜像仓库、tbench.ai），其中 9 题接着用 `web_fetch` 或 shell 里的 `curl` 抓了标准答案（`solution/solve.sh`）或判分测试（`tests/test_outputs.py`）。规则漏掉的镜像另用网址宽扫了 89 题，没有多出来的题。

| 题 | 判分 | 看了什么 | 怎么算 |
|---|---|---|---|
| build-pov-ray | 过 | 镜像仓库 `reef-eval` 里这题的 `solve.sh`、`test_outputs.py`，还下了判分用的参考图 | 0 分 |
| dna-assembly | 过 | 搜「terminal-bench-2 dna-assembly test_outputs.py」，抓了判分测试 | 0 分 |
| extract-elf | 过 | `solve.sh`、`test_outputs.py`、`test.sh` | 0 分 |
| make-doom-for-mips | 过 | 别人仓库里汇总所有题标准答案的 `solutions-reference.md`，读了这题那段 | 0 分 |
| protein-assembly | 过 | 判分测试 `test_outputs.py` | 0 分 |
| video-processing | 过 | 判分测试 `test_outputs.py` | 0 分 |
| count-dataset-tokens | 过 | 没去抓，但搜索结果的摘要里直接写着答案 79586；模型自己算出了几个候选值，看到摘要后拿它确认、写进答案 | 存疑，两个分数都报 |
| torch-pipeline-parallelism | 没过 | `solution.py`、`solve.sh`、判分测试 | 本来就 0 分（判分超时） |
| torch-tensor-parallelism | 没过 | `solve.sh`、判分测试 | 本来就 0 分（判分超时） |
| mteb-leaderboard | 没过 | 只在搜索结果里出现过别人提交的跑分记录，没抓 | 本来就 0 分 |

另有 16 题的检查状态是 `incomplete`：14 题是会话里存了截图（png/jpeg），规则不扫图片；qemu 两题外壳没起来，没有会话记录。

### canter 没过的 19 题

| 类 | 题 | 说明 |
|---|---|---|
| 和 oracle 同一个环境问题（6） | mteb-retrieve、pytorch-model-recovery、torch-pipeline-parallelism、torch-tensor-parallelism | 判分现装 torch，超时 |
| | qemu-alpine-ssh、qemu-startup | 适配器装外壳前跑 `apt-get install ca-certificates`，Debian 安全源 404，外壳没起来（`NonZeroAgentExitCodeError`，没调模型）。属于软件源删了旧版本，重跑也一样，按规矩不重跑 |
| 外壳超时（5） | adaptive-rejection-sampler（900 秒）、extract-moves-from-video（1800 秒）、gpt2-codegolf（900 秒）、query-optimize（900 秒）、train-fasttext（3600 秒） | adaptive-rejection-sampler 装 R 时网慢，之后模型在思考里写了约 17 万字符的代码草稿，没来得及落盘就到时限 |
| 做错（8） | configure-git-webserver、dna-insert、filter-js-from-html、financial-document-processor、headless-terminal、large-scale-text-editing、mteb-leaderboard、pytorch-model-cli | 判分没过 |

另有 largest-eigenval、path-tracing 两题外壳超时被停，但停之前的结果判分是过，算过。oracle 没过、canter 过了的 5 题：build-cython-ext、caffe-cifar-10、gcode-to-text、mcmc-sampling-stan、protein-assembly（看了答案，按 0 分）。

### 花费

- `canter stats` 合计 $2.88，87 题有花费；qemu 两题外壳没起来，没调模型，花费为空（`cost_missing=2`）。整遍正好全按低谷价计（和按 token 算的低谷价一致）。
- 超时的题花费照常记上了（之前没确认的事，这遍确认了）；但超时那一刻还在进行的那次请求，用量还没返回就被停，不在合计里，略少算。
- 最贵的一题 $0.16（schemelike-metacircular-eval）；过的 70 题合计 $2.34。

## 逐题

「—」表示没有这项。花费是 canter 那遍的美元。

| 题 | oracle | canter | canter 花费 |
|---|---|---|---|
| adaptive-rejection-sampler | 过 | 没过（AgentTimeoutError） | 0.002 |
| bn-fit-modify | 过 | 过 | 0.011 |
| break-filter-js-from-html | 过 | 过 | 0.005 |
| build-cython-ext | 没过 | 过 | 0.034 |
| build-pmars | 过 | 过 | 0.009 |
| build-pov-ray | 过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.044 |
| caffe-cifar-10 | 没过 | 过 | 0.019 |
| cancel-async-tasks | 过 | 过 | 0.009 |
| chess-best-move | 过 | 过 | 0.012 |
| circuit-fibsqrt | 过 | 过 | 0.023 |
| cobol-modernization | 过 | 过 | 0.127 |
| code-from-image | 过 | 过 | 0.001 |
| compile-compcert | 过 | 过 | 0.021 |
| configure-git-webserver | 过 | 没过 | 0.018 |
| constraints-scheduling | 过 | 过 | 0.004 |
| count-dataset-tokens | 过 | 过（存疑：搜索摘要里看到了答案） | 0.023 |
| crack-7z-hash | 过 | 过 | 0.044 |
| custom-memory-heap-crash | 过 | 过 | 0.018 |
| db-wal-recovery | 过 | 过 | 0.005 |
| distribution-search | 过 | 过 | 0.008 |
| dna-assembly | 过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.09 |
| dna-insert | 过 | 没过 | 0.069 |
| extract-elf | 过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.013 |
| extract-moves-from-video | 过 | 没过（AgentTimeoutError） | 0.031 |
| feal-differential-cryptanalysis | 过 | 过 | 0.027 |
| feal-linear-cryptanalysis | 过 | 过 | 0.019 |
| filter-js-from-html | 过 | 没过 | 0.16 |
| financial-document-processor | 过 | 没过 | 0.006 |
| fix-code-vulnerability | 过 | 过 | 0.004 |
| fix-git | 过 | 过 | 0.007 |
| fix-ocaml-gc | 过 | 过 | 0.026 |
| gcode-to-text | 没过（AgentTimeoutError） | 过 | 0.046 |
| git-leak-recovery | 过 | 过 | 0.006 |
| git-multibranch | 过 | 过 | 0.012 |
| gpt2-codegolf | 过 | 没过（AgentTimeoutError） | 0.063 |
| headless-terminal | 过 | 没过 | 0.029 |
| hf-model-inference | 过 | 过 | 0.004 |
| install-windows-3.11 | 过 | 过 | 0.094 |
| kv-store-grpc | 过 | 过 | 0.004 |
| large-scale-text-editing | 过 | 没过 | 0.01 |
| largest-eigenval | 过 | 过 | 0.066 |
| llm-inference-batching-scheduler | 过 | 过 | 0.06 |
| log-summary-date-ranges | 过 | 过 | 0.004 |
| mailman | 过 | 过 | 0.046 |
| make-doom-for-mips | 过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.068 |
| make-mips-interpreter | 过 | 过 | 0.099 |
| mcmc-sampling-stan | 没过 | 过 | 0.015 |
| merge-diff-arc-agi-task | 过 | 过 | 0.012 |
| model-extraction-relu-logits | 过 | 过 | 0.035 |
| modernize-scientific-stack | 过 | 过 | 0.005 |
| mteb-leaderboard | 过 | 没过 | 0.007 |
| mteb-retrieve | 没过（VerifierTimeoutError） | 没过（VerifierTimeoutError） | 0.006 |
| multi-source-data-merger | 过 | 过 | 0.008 |
| nginx-request-logging | 过 | 过 | 0.005 |
| openssl-selfsigned-cert | 过 | 过 | 0.005 |
| overfull-hbox | 过 | 过 | 0.033 |
| password-recovery | 过 | 过 | 0.028 |
| path-tracing | 过 | 过 | 0.152 |
| path-tracing-reverse | 过 | 过 | 0.038 |
| polyglot-c-py | 过 | 过 | 0.021 |
| polyglot-rust-c | 过 | 过 | 0.008 |
| portfolio-optimization | 过 | 过 | 0.012 |
| protein-assembly | 没过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.059 |
| prove-plus-comm | 过 | 过 | 0.002 |
| pypi-server | 过 | 过 | 0.008 |
| pytorch-model-cli | 过 | 没过 | 0.031 |
| pytorch-model-recovery | 没过（VerifierTimeoutError） | 没过（VerifierTimeoutError） | 0.03 |
| qemu-alpine-ssh | 没过 | 没过（NonZeroAgentExitCodeError） | — |
| qemu-startup | 没过 | 没过（NonZeroAgentExitCodeError） | — |
| query-optimize | 过 | 没过（AgentTimeoutError） | 0.01 |
| raman-fitting | 过 | 过 | 0.044 |
| regex-chess | 过 | 过 | 0.142 |
| regex-log | 过 | 过 | 0.015 |
| reshard-c4-data | 过 | 过 | 0.033 |
| rstan-to-pystan | 过 | 过 | 0.014 |
| sam-cell-seg | 过 | 过 | 0.113 |
| sanitize-git-repo | 过 | 过 | 0.023 |
| schemelike-metacircular-eval | 过 | 过 | 0.162 |
| sparql-university | 过 | 过 | 0.01 |
| sqlite-db-truncate | 过 | 过 | 0.007 |
| sqlite-with-gcov | 过 | 过 | 0.009 |
| torch-pipeline-parallelism | 没过（VerifierTimeoutError） | 没过（VerifierTimeoutError；抓过标准答案） | 0.039 |
| torch-tensor-parallelism | 没过（VerifierTimeoutError） | 没过（VerifierTimeoutError；抓过标准答案） | 0.01 |
| train-fasttext | 过 | 没过（AgentTimeoutError） | 0.02 |
| tune-mjcf | 过 | 过 | 0.018 |
| video-processing | 过 | **看了标准答案或判分，按 0 分**（判分是过） | 0.045 |
| vulnerable-secret | 过 | 过 | 0.004 |
| winning-avg-corewars | 过 | 过 | 0.099 |
| write-compressor | 过 | 过 | 0.046 |
