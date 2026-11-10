# Terminal-Bench 2.1 全套对比（P2-274）

5090，Harbor 0.25.0，89 题，4 并发，`--max-retries 0`。每遍跑完更新本文件。

## 总分

| 外壳 | 结果文件 | 总分（第一次） | 含重跑 | 只算 oracle 通过的 78 题 | 花费 | 全按高峰价 | 用时（墙钟） | token |
|---|---|---|---|---|---|---|---|---|
| oracle | `oracle-full-2026-11-10.json` | 78/89（87.6%） | 无重跑 | 78/78 | 不调模型 | — | 2 小时 28 分 | — |
| canter | 未跑 | | | | | | | |
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

## 逐题

逐题结果见各遍的结果文件 `trials[]`（`task`、`reward`、`exception_type`）。canter 跑完后在这里补逐题对照。
