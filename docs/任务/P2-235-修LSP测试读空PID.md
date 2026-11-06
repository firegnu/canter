# 任务：修 LSP 测试偶尔读到空 PID 文件就失败

执行：主控
类型：Bug 修复

## 用户原话

- 11-06 用户（P2-233 时）：「现在修吧。这个是小问题的话，你直接修就行了」——几行能改好的小问题主控直接修。

## 要做的

`crates/canter-server/src/lsp_tests.rs` 的 `Fixture::ready`：只查 `instance` 文件存在就读进程号，而写的一边用 `fs::write` 先建文件再写内容，中间读到空文件时 `parse().unwrap()` 崩。11-06 P2-232 合并后全套里 `sigkill_of_server_removes_lsp_process_group` 因此失败一次（P2-223 开发时也失败过一次，同样是读空 PID 文件），单独复跑通过。改成等文件里有能解析的进程号再往下走。只改测试。

## 完成记录

- 做了什么：`ready` 的等待条件从「`instance` 存在」改成「`instance` 读得出且能解析成数字」。
- 验证：`cargo nextest run -p canter-server lsp_tests` 连跑 3 次，3 条全过；`cargo clippy -p canter-server --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。竞争偶发，没法稳定复现，靠读代码确认原因。
- 没做的事：同一处读 `{pid}.log` 里的 `CHILD` 行没改，没见过它失败。
