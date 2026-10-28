# P2-34 plancompare

前台监督本分支的 canter，通过 `canter_tui::client::Client` 开会话、发消息、批准和中断；
关闭会话使用公开 DELETE 接口。进程监督、隔离环境和计数口径沿用 P2-72 promptcompare，
其私有辅助函数按本实验需要改写在 `support.rs`，不修改原驱动或产品代码，不增加依赖。

```text
plancompare prepare /absolute/path/to/config.json
plancompare smoke /absolute/path/to/canter
plancompare run /tmp/p2-34-plancompare-XXXXXX /absolute/path/to/canter
plancompare report /tmp/p2-34-plancompare-XXXXXX
```

`prepare` 只读取指定配置文件，创建仓库外的临时根目录，将配置副本存成 0600；不打印内容，
也不启动服务端或调用模型。保留接口、key 和单价，设 `deepseek-flash`、131072 上下文窗口、
默认开启清单，并清空 MCP 配置。运行使用 high 思考档位、隔离 HOME / CANTER_HOME、
socket、Cargo home/target，直接调用已安装工具链并启用 Cargo offline，不读真实用户说明文件。

`smoke` 另建隔离目录，仅用 loopback 假模型和假 key；完成 A/B/C 开关各一次、D，
再用一笔模拟大用量触发硬阈值中断。假模型返回的金额只是计数演练，不是真实费用。
断言放在服务端停止并回收之后；验证批准、实际 Rust 测试先失败再修好、四文件改名、
只读解释、D 的历史调用仍在而工具定义已关闭，以及 D 不会重复计入 A 的统计。

**只有主控回复「开始真调」后才运行 `run`。** A/B/C 的项目和会话互相独立，开关在配置里
切换后新开会话；D 显式关闭 A-on 会话、设置关闭、再恢复该会话。场景原话不提醒模型列清单。
若真实 A-on 没用过 update_plan，D 仍执行，但不能据此认定有历史调用时的兼容性通过。
权限为 AskForExecution，批准请求自动允许；遇到模型提问不猜答案，中断并停止交主控处理。
已有运行记录时拒绝重跑整套，不自动重试。服务端在正常、错误或用户中断路径均按自身 PID 停止并回收。

合计达到 50 次可见请求或 $0.8 不开新场景，当前轮继续；达到 60 次或 $1 立即发协议 interrupt。
按 P2-72：每个会话取 `max(普通用量事件数, assistant 历史条数 + compaction_request 条数)`，
另加私有用量事件；费用只累加逐次 `request.costUsd`，缺费用或历史多于用量时标记 unknown。
每个事件及 250ms 定时检查；接受已在途请求，无法统计没有记录的内部重试，不把它称为账单硬上限。

产物：`state/sessions/<id>/events.jsonl` 和 `model-history.jsonl` 是原始依据；`runs.jsonl`
记录场景和会话，`runs/<case>/record.json` 固定事件及历史起止位置，D 与 A-on 分开计数；
`results.jsonl`、`totals.json` 保存请求、token、已报告费用、计划调用/成功次数、连续同内容更新、
命令退出码和轮错误。A/B 结束后另跑本地 `cargo test`，结果在 `verify.*`；这些检查不调用模型。
`report` 从原生记录及固定区间重新计算，不解析配置。第二段分析完成后由执行者删除整个
临时 `state` 和 `home`（其中含 key），确认清理，再提交任务记录；第一段保留配置等待授权。
