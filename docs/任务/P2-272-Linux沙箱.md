# 任务：Linux 支持（二）：Linux 沙箱

依据：11-10 Linux 支持拆两件，P2-160 做编译和运行，本件做 Linux 上的沙箱。两件都做完再做 P2-260。
依赖：P2-160
待用户：P2-160 做完后补全

## 用户原话

- 11-10：「我同意1，但是我的想法也是拆2件，但是要两件都做完，再跑260.你觉得如何？」

## 要做的

（占号。要解决：照 macOS Seatbelt 现在管的范围（P2-123），在 Linux 上用 bubblewrap 或 Landlock 做同样的隔离；5090 上已装 bubblewrap，内核 7.0 支持 Landlock。先看 Codex 在 Linux 上怎么做。）
