# 任务：Linux 支持（二）：Linux 沙箱

依据：11-10 Linux 支持拆两件，P2-160 做编译和运行，本件做 Linux 上的沙箱。两件都做完再做 P2-260。
依赖：P2-160
待用户：P2-160 做完后补全

## 用户原话

- 11-10：「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.你觉得如何？」

## 要做的

（占号。要解决：照 macOS Seatbelt 现在管的范围（P2-123），在 Linux 上用 bubblewrap 或 Landlock 做同样的隔离；5090 上已装 bubblewrap，内核 7.0 支持 Landlock。先看 Codex 在 Linux 上怎么做。）

## P2-160 交叉审查留给本件的（11-10）

- 测试夹具里按 `cfg!(target_os = "macos")` 写的沙箱默认值，有了 Linux 沙箱后改回两边都开：`crates/canter/tests/tui.rs` 的 World、`tests/server.rs` 的 Fixture、`canter-server` 的 `goal_tests`、`user_shell_tests`、`fork_tests`，`tests/exec.rs` 几处和 `--yolo`，两个记忆测试的过滤，doctor 测试的分支。
- `crates/canter-server/src/fork_tests.rs` 两个分叉测试（约 455、465、680、699 行）：P2-160 改成从模型历史读临时目录，macOS 上丢了「会话沙箱可写范围里有自己的临时目录」这条正向断言，补回（两边都有沙箱后两边都断言）。
- `crates/canter/tests/support/yolo.rs` 的 `yolo_reconnect_restores_the_terminals_sandbox_choice`：开头补 `world.set_sandbox(true)`，否则 Linux 上测不到 `--yolo` 重连后丢没丢。
- `crates/canter-server/src/doctor.rs` 约 470 行：Linux 上 doctor 的沙箱提示（`Seatbelt sandboxing is unavailable…` / `Run on macOS…`）和工具报错那句统一。
- 看护进程 `kill -KILL -<pgid>` 在 Alpine 的 busybox ash 上没验过（P2-260 在 Alpine 题目容器里跑时顺手看）。
