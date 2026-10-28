# HANDOFF

照着执行的事实。设计和理由看 `docs/DESIGN.md`，每件活的经过看 `docs/任务/`。

## 装的哪一版、怎么退回

- 10-28 第二十次装 main `ced920e`（`canter --version` 显示 `canter 0.1.0 · ced920e`；比上一版多 P2-115：计划模式里状态栏写 `Plan (then <档位>)`）。服务端要用户自己 `canter serve --replace`。
- 退回上一版 `1736cf5`：`cp -p ~/.canter/canter-1736cf5 ~/.cargo/bin/canter`，再 `canter serve --replace`。存盘格式没变。
- 10-28 第十九次装 main `1736cf5`（`canter --version` 显示 `canter 0.1.0 · 1736cf5`；比上一版多 P2-103 计划模式：Shift+Tab 第四档 Plan、`/plan`、`submit_plan` 批准框）。服务端要用户自己 `canter serve --replace`。
- 退回上一版 `c8d9451`：`cp -p ~/.canter/canter-c8d9451 ~/.cargo/bin/canter`，再 `canter serve --replace`。旧版打开在计划模式里的会话不坏，但按底下那一档照常干活、没有计划模式的限制，方案批准框看不到（P2-103 交叉审查第 9 条）。
- 10-28 第十八次装 main `c8d9451`（`canter --version` 显示 `canter 0.1.0 · c8d9451`；比上一版多 P2-114：状态栏 `context` 是以压缩线为满的进度条，分支后面 `(+加,-删)`）。服务端要用户自己 `canter serve --replace`。
- 退回上一版 `b19fb3a`：`cp -p ~/.canter/canter-b19fb3a ~/.cargo/bin/canter`，再 `canter serve --replace`。存盘格式没变。
- 10-28 第十七次装 main `b19fb3a`（`canter --version` 显示 `canter 0.1.0 · b19fb3a`；比上一版多 P2-113：状态栏 `xx% to compact`、各项上色，`config.json` 写 `"auto_compact_tokens": 400000` 可把压缩线调低，改了要重启服务端）。服务端要用户自己 `canter serve --replace`。
- 退回上一版 `4516c70`：`cp -p ~/.canter/canter-4516c70 ~/.cargo/bin/canter`，再 `canter serve --replace`。旧版不认 `auto_compact_tokens`（照旧忽略不认识的项，按窗口压缩），存盘格式没变。
- 10-28 第十六次装 main `4516c70`（`canter --version` 显示 `canter 0.1.0 · 4516c70`；P2-34 合并后的正式版：计划和待办默认开，`config.json` 写 `"plan_enabled": false` 可关）。服务端要用户自己 `canter serve --replace`。
- 退回到装 P2-34 之前的 `2302c00`：`cp -p ~/.canter/canter-2302c00 ~/.cargo/bin/canter`，再 `canter serve --replace`；旧版打开用过清单的会话不坏，清单不显示。上一次的分支版 `583ef2e` 备份在 `~/.canter/canter-583ef2e`。
- 10-28 第十五次装 **未合并的分支** `p2-34-todo` 的 `583ef2e`（`canter --version` 显示 `canter 0.1.0 · 583ef2e`；比上一版多清单拖着不勾时提醒模型、工具说明加「做完一步立刻更新」）：用户长程回测用。服务端要用户自己 `canter serve --replace`。
- 退回上一版 `12ffad2`：旧程序备份在 `~/.canter/canter-12ffad2`，`cp -p ~/.canter/canter-12ffad2 ~/.cargo/bin/canter`，再 `canter serve --replace`。存盘格式没变。
- 10-28 第十四次装 **未合并的分支** `p2-34-todo` 的 `12ffad2`（`canter --version` 显示 `canter 0.1.0 · 12ffad2`；比上一版多 P2-34 计划和待办，清单默认开）：用户要亲自开着清单测一个长程任务，测完定默认开关再合并。服务端已换新版（用户自己跑的 `canter serve --replace`，10-28 10:20 起）。
- 退回上一版 `2302c00`：旧程序备份在 `~/.canter/canter-2302c00`，`cp -p ~/.canter/canter-2302c00 ~/.cargo/bin/canter`，再 `canter serve --replace`。旧版打开用过清单的会话不坏，清单项目读成 Unknown 不显示（P2-34 交叉审查查过）。
- 10-28 第十三次装 main `2302c00`（`canter --version` 显示 `canter 0.1.0 · 2302c00`；比上一版多 P2-112 `!` 做成 shell 模式：品红、提示符换成 `!`、下边框 `! for shell mode`）。只改了终端，没重启服务端（还是 `9eb44dd` 起的那个）。
- 退回上一版 `d69e2f7`：旧程序备份在 `~/.canter/canter-d69e2f7`，`cp -p ~/.canter/canter-d69e2f7 ~/.cargo/bin/canter`，重开终端即可。只改了终端，新旧互相读没问题。
- 10-28 第十二次装 main `d69e2f7`（`canter --version` 显示 `canter 0.1.0 · d69e2f7`；比上一版多 P2-111 `!` 命令在输入框和对话卡片里标红）。只改了终端，服务端代码没变，没重启服务端（还是 `9eb44dd` 起的那个，不影响）。
- 退回上一版 `9eb44dd`：旧程序备份在 `~/.canter/canter-9eb44dd`，`cp -p ~/.canter/canter-9eb44dd ~/.cargo/bin/canter`，重开终端即可。只改了终端，新旧互相读没问题。
- 10-27 第十一次装 main `9eb44dd`（`canter --version` 显示 `canter 0.1.0 · 9eb44dd`；比上一版多 P2-100 全局说明改读 `~/.canter/AGENTS.md`，不再读 `~/.agents/AGENTS.md`）。服务端已换新版（`canter serve --replace`，nohup 起在后台）。`~/.canter/AGENTS.md` 是从 `~/.codex/AGENTS.md` 复制的一份（10-27，用户要用自己实际用的），两份各自独立，改一份另一份不跟着变。
- 退回上一版 `b78205d`：旧程序备份在 `~/.canter/canter-b78205d`，`cp -p ~/.canter/canter-b78205d ~/.cargo/bin/canter`，再 `canter serve --replace`。旧版读 `~/.agents/AGENTS.md`（现在没有这个文件），不读 `~/.canter/AGENTS.md`。
- 10-27 第十次装过 main `b78205d`（`canter --version` 显示 `canter 0.1.0 · b78205d`；比上一版多 P2-95 搜不到不画成失败和提示词加 rg、P2-25 输入历史、P2-31 外部编辑器 Ctrl+G、P2-27 做完提醒、P2-26 `!` 直接跑命令、P2-97 干活时 Tab 插队、P2-99 打断时排队的消息放回输入框）。服务端已换新版（`canter serve --replace`，nohup 起在后台）。
- 退回上一版 `a669963`：旧程序备份在 `~/.canter/canter-a669963`，`cp -p ~/.canter/canter-a669963 ~/.cargo/bin/canter`，再 `canter serve --replace`。P2-26 给 `commandExecution` 加了可选的 `source`，交叉审查查过旧版读新存盘不坏：旧版把 `!` 跑的命令显示成普通命令卡片、回退列表里没有命令轮。旧版不认 `tui.json` 里的通知设置，留着无害；`~/.canter/input_history.jsonl` 旧版不读。
- 10-27 第九次装过 main `a669963`（比再上一版多 P2-93 输入框鼠标单击定位）。
- 退回再上一版 `e4259d0`：旧程序备份在 `~/.canter/canter-e4259d0`，`cp -p ~/.canter/canter-e4259d0 ~/.cargo/bin/canter`，再 `canter serve --replace`。只改了终端，新旧互相读没问题。
- 10-27 第八次装过 main `e4259d0`（比再上一版多 P2-89 `/export` 导出 Markdown 和 `canter -c` / `--continue`）。
- 退回再上一版 `04db0a3`：旧程序备份在 `~/.canter/canter-04db0a3`，`cp -p ~/.canter/canter-04db0a3 ~/.cargo/bin/canter`，再 `canter serve --replace`。这一版没改存盘格式，新旧互相读没问题。
- 10-27 第七次装过 main `04db0a3`（比再上一版多 P2-86 handoff 后按两次 Esc 回原会话、P2-87 终端挂断时不再卡住、P2-77 `/theme` 代码配色和 `~/.canter/themes/`，`/statusline` 和 `/theme` 存 `tui.json` 互不覆盖）。服务端已换新版（`canter serve --replace`，nohup 起在后台）。
- 退回再上一版 `8c29af9`：旧程序备份在 `~/.canter/canter-8c29af9`，`cp -p ~/.canter/canter-8c29af9 ~/.cargo/bin/canter`，再 `canter serve --replace`。旧版不认 `tui.json` 里的 `theme`，照旧用 `ansi`；旧版的 `/statusline` 保存时会整个重写 `tui.json`，把 `theme` 冲掉。
- 10-27 第六次装过 main `8c29af9`（比再上一版多 P2-76 压缩和 `/handoff` 的语言规矩、P2-49 按词编辑、P2-50 大段粘贴收起、P2-51 拒绝并说一句、P2-52 标签页标题、P2-53 `?` 帮助弹框、P2-54 每轮改了哪些文件和 `/diff`、P2-55 路径中间省略、P2-82 文字发送失败放回草稿）。服务端已换新版（`canter serve --replace`，nohup 起在后台）。
- 退回再上一版 `011bdc9`：旧程序备份在 `~/.canter/canter-011bdc9`，`cp -p ~/.canter/canter-011bdc9 ~/.cargo/bin/canter`，再 `canter serve --replace`。用过「拒绝并说一句」（P2-51）的会话，P2-51 交叉审查查过新旧版本不会互相读坏，旧版不显示那句话。
- 10-27 第五次装过 main `011bdc9`（P2-74 `/context` 弹框、P2-75 `/files`、`/mcp`、`/skills` 弹框和排版）。再往前 `a6af4ae` 备份在 `~/.canter/canter-a6af4ae`。再往前的 `71c3507`（换提示词之前；不认 `tui.json`，留着无害；会话存盘多出的 `segments` 旧版读时忽略）、`f2c02d6`、`c5d2efe` 备份在 `~/.canter/canter-<提交号>`。
- 10-27 装过 main `a6af4ae`（含 P2-69～P2-71 新的系统提示词、工具说明、压缩和 /handoff 说明，P2-73 一次拖多张图片，P2-63 `/statusline` 选择框和 `~/.canter/tui.json`）。
- 10-27 装过 main `71c3507`（含 P2-66 跳到底部提示）。
- 10-27 装过 main `f2c02d6`（含 P2-65 命令卡片显示整条命令并上色、输出用粗竖线）。
- 10-27 装过 main `c5d2efe`（`canter --version` 显示 `canter 0.1.0 · c5d2efe`；比上一版多 P2-48 用户消息底色、P2-61 diff 上色和行号栏、P2-56 流光、P2-59 Esc 撤回刚发的消息、P2-62 压缩时显示 Compacting、P2-60 状态栏定制）。服务端已换新版（`canter serve --replace`，nohup 起在后台）。用户的 `~/.canter/config.json` 加了 `statusLine` 指向 `~/.canter/statusline.sh`（10-27 主控写的，用 `/usr/bin/jq`），改前的配置备份在 `~/.canter/config.json.bak-p2-60`（0600）。
- 退回上一版 `87259d5`：旧程序备份在 `~/.canter/canter-87259d5`，`cp -p ~/.canter/canter-87259d5 ~/.cargo/bin/canter`，再 `canter serve --replace`；旧版不认 `statusLine`，留着无害。
- 10-26 装过 main `87259d5`（0.1.0，`canter --version` 显示 `canter 0.1.0 · 87259d5`；含 P2-47 启动欢迎头和 `--version`、P2-46 可点的链接和路径、P2-44 对话搜索 Ctrl+F、P2-45 拖选复制和 `/copy`、P2-58 思考用时从发出请求算起和网页标题、P2-40 Ctrl+O 展开和思考、P2-41 工具卡片、P2-42 空行和状态行、P2-43 每轮用时、P2-38 markdown 表格、P2-39 代码块语法色和批准框命令上色、MCP 每个服务可配 `startup_timeout_sec` / `tool_timeout_sec`、P2-23 撤销和回退、P2-35 网页编码、P2-36 展示层颜色、P2-37 diff 新画法、P2-13 网页抓取、P2-24 @ 引用文件、P2-12 MCP、P2-29 服务端启动误报修复、P2-16 全屏界面、P2-1 斜杠命令、P2-15 缓存命中、P2-2 思考档位和切模型、P2-3 技能、P2-19 全局 AGENTS.md、P2-4 提问框、P2-6 两行状态栏、P2-20 Shift+Tab 轮换权限、P2-5 图片输入、P2-7 用量和花费、P2-8 /context 和 /files、P2-9 /btw、P2-10 /handoff、P2-11 /review；上一版是 `37489fe`）：`cargo install --locked --path crates/canter`，程序在 `~/.cargo/bin/canter`。不是系统服务，服务端由客户端在后台起，状态在 `~/.canter/`（DESIGN 第 15、16 条）。
- 换版本：在 main 上重新 `cargo install --locked --path crates/canter`，再 `canter serve --replace`。
- 退回：按 `~/.canter/server.json` 的 pid 停服务端（SIGTERM），`cargo uninstall canter`。`~/.canter/` 有会话和 key，删不删问用户。
- 用过 `/rewind` 退回对话的会话，不要用 `49216a5` 以前的版本打开：旧版不认退回记录，会显示退掉之前的完整对话、并把它发给模型；在旧版里接着聊过再升级回来，对话会乱（P2-23）。已退回的文件不受影响。

## 机器和环境

- 参考源码只读克隆在 `/tmp/r49-src/`：`codex`（`rust-v0.163.0`）、`pi`（`v1.4.0`）、`opencode`（`v2.1.3`）。`/tmp` 重启会清空，丢了照这三个标签重新 `git clone --depth 1 --branch <标签>`（地址见调研报告「本地读的源码」）。
- 编译目录：`../canter-worktrees/.target/<子目录>`，主仓库用 `main`。
- 测试用 cargo-nextest（10-26 用 Homebrew 装的 0.9.149，`/opt/homebrew/bin/cargo-nextest`）。
- 开 agent：`corral start` 单独一条运行，前后不用 `&&`、`;`、`|` 接别的命令，一次只开一个；建 worktree、`corral wait`、取结果都另起一条（用户的放行规则 `Bash(corral start:*)` 只认单独的 `corral start`，串起来会被自动权限检查以「Create Unsafe Agents」拦下；corral-dispatch 技能第 4 节，ranch f4a9a29，10-28）。单独一条仍被拦，再把命令给用户用 `!` 运行。

## 还没派的

第一期 P1-1 到 P1-14 都做完了（P1-10 第 1 到 3 条除外）。排着的：

- 第二期：总单是任务 P2-0（`docs/任务/P2-0-第二期需求单.md`）。 10-27 晚起剩下的次序以 P2-96 为准：第二期扩成 23 件（另加 P2-103 计划模式、104 沙箱、105 用久变慢、106 搜网页、107 安装部署、108 按命令放行的规则、109 花费上限、110 中断后接着跑），以 P2-81 收尾。做完了 P2-1～P2-13、P2-15、P2-16、P2-19、P2-20、P2-23、P2-24、P2-29、P2-35，P2-18 展示层分件 P2-36～P2-42 和补充 P2-43～P2-48、P2-58、P2-61 做完，P2-18、P2-33 出了方案（P2-17 并进 P2-18）。
  - 10-27 做完 P2-64（别家提示词调研）、P2-65（命令卡片），已装 `f2c02d6`。
  - P2-14、P2-22、P2-25～P2-28、P2-30～P2-34、P2-49～P2-55、P2-57、P2-63：各有一份入队的任务文件，要做时主控补全、给用户看过、说派才派。顺序见 P2-0「10-26 重排剩下的」：展示层一件接一件（10-26 改成 P2-58 → P2-45 → P2-44 → P2-46 → … → P2-56）（P2-43～P2-56 是 10-26 补的；P2-44～P2-48 等用户试完 P2-43 那一版再分解），然后是 P2-57 系统提示词（先出方案），之后是支持长任务的 P2-34 → P2-28（P2-27 可同时做），再往后 P2-25 等（P2-0 经过 10-26 最后一条）；P2-33 实现前要用户定方案 §7 的 4 个问题。
  - P2-21 插件系统 10-26 挪到第三期。
- P1-10 修第一期已知问题（`docs/任务/P1-10-第一期已知问题.md`）：第 4、5 条拆成 P1-13、P1-14 已派；第 1 到 3 条（性能）先不做。

## 已知没修的问题

- 计划模式选「清空上下文再做」后，作答的终端要等原会话这一轮结束才建新会话，等的时候没有超时、不收按键；原会话收尾写盘失败（比如磁盘满）时可能一直卡住，等待中出错时新会话不建、提示也没说方案没开工（P2-103 交叉审查复核建议改第 1 条，读代码推断）。另：新会话建好、切过去失败时提示写成「没能开始」，用户可能再选一次、两个会话做同一份方案（第 2 条，很少见）。
- `canter-model` 的 `cancellation_interrupts_retry_after_and_local_backoff`（`crates/canter-model/tests/streaming.rs`）在全套并行跑时偶发失败过一次（10-28 P2-34 交叉审查），之后连跑多次都过，原因未知；测试里有 50ms、250ms 两个时间窗口，怀疑高负载时超出。没查。
- 订阅落后时，`Store::events_after` 每次从头读整份事件文件（P1-4 交叉审查第 8 条）。结果对，长会话会慢。
- 服务端的会话登记表是一把锁，打开、关闭、从盘读大会话时会挡住别的会话的命令和新订阅（P1-5 交叉审查第 4 条）。结果对，历史大了会卡。
- `/v1/info` 每次重放所有会话的完整历史，客户端探活只等 2 秒；按实测，历史到 800 MB 左右会超时（P1-5 交叉审查第 8 条）。
- 终端连上新服务端时「先读快照、再 resume」是两次请求，另一个客户端恰好在中间关掉会话，本终端会把它重开（P1-13 交叉审查，只推演没复现，窗口一次本机往返）。要堵死得给 resume 加前置条件（如 `ifEventSeq`），是协议改动。
- 批准框：选项出现后 300 ms 保护期挡不住超过 300 ms 的终端卡顿（P1-7）。程序看不到终端真正显示的时刻，没有可靠修法；要更强保证得换成更明确的确认方式。换回输入框时的 Enter 已由 P1-14 修好。
- 测试 `canter::discovery discovery_recovers_from_a_reused_pid` 在全套高负载时偶发失败过一次（P2-37 开发时），没留下报错，单独跑 25 次复现不了；再失败时把报错留下来再查。
- 测试 `canter-model::streaming cancellation_interrupts_retry_after_and_local_backoff` 在全套并行跑时偶发失败过一次（P2-39 审查时），单独跑和再跑全套都过；这条测试等 50 毫秒后数请求，机器忙时可能没等到。
- 链接（P2-46 复核 1）：取消 `/resume` 会话列表、返回原会话的同步重绘期间，在仍显示的列表画面上按下鼠标，事件会被当成原对话那一帧，同格松开约 0.4 秒后可能打开原对话那一格的链接。时机很窄，用户 10-26：「先合并，记成建议改」。改法：会话列表占屏时结束对话帧的有效区间，查列表帧返回没有链接，补一条回归测试。
- 压缩没压成功的显示（P2-62）：终端按核心写进摘要的那句固定的话（`Compaction did not finish; the original context is kept.`）原样比对，核心改了措辞，失败会显示成「Context compacted」。彻底的改法是给压缩项加状态字段（协议改动）。
- stdin 被重定向时（`canter < /dev/null`、`… | canter`），macOS 上 canter 启动就卡在 crossterm 查询键盘增强，空屏、CPU 100%，只能从外面杀（P2-60 第二轮复核建议改 2，main 原来就有）。另外 P2-60 的读键盘线程对 `/dev/tty` 调 `poll()` 在 macOS 上得 POLLNVAL，会当成挂断（建议改 1），现在被前一条挡着碰不到。改法：stdin 不是终端时直接报错退出，或跳过查询并让读线程在 POLLNVAL 时退回原读法；走后一种要改两处，P2-87 起 `Input::recv` 也调 `terminal_input_open` 查挂断，`/dev/tty` 上第一次就会当成输入结束（可在 POLLNVAL 或 poll 报错时设 `self.tty = None`、只等通道）。卡住时 CPU 100% 可能就是下一条 crossterm 的空转循环（没核实）。
- crossterm 0.29.0 终端挂断时读线程会在 `try_read` 的 EOF/EIO 循环里空转、不返回（P2-87 查到）：canter 另外每 50 ms 查一次挂断、走原来的退出清理，空转的读线程只活到进程退出（约 150–200 ms）。启动时查询键盘增强正好碰上挂断时主线程会卡住，窗口只有一次终端往返，没修。
