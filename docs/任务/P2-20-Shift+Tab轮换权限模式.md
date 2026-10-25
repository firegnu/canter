# 任务：Shift+Tab 轮换权限模式

2026-10-25，canter/main 交给 canter/dev-shifttab（Claude Code，opus[1m] / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：常规（置信 0.97）、要、碰要害）
类型：功能变更
依据：第二期加塞的第 20 件（`docs/任务/P2-0-第二期需求单.md`）。本轮在终端输入框里加 Shift+Tab，按顺序轮换三种权限模式，像 Claude Code 那样随手切；`/mode` 保持不变。不加新的权限模式，不做 Claude Code `/permissions` 那种允许／拒绝规则。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-1
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-25：「现在加塞一个任务，就是现在我想有类似claude code的bypass或者codex的yolo那种功能。这样权限默认全过，理解我意思吗？」
- 主控说明已有 `--yolo` 和 `/mode allow-all`，用户：「我要的是类似claude code的permission斜杠命令，这样我可以随时改。permission命令带出一个各种权限的选择。理解我意思吧？」
- 主控说明 `/mode` 就是这个选择框，并问是只换叫法／按法，还是要 Claude Code `/permissions` 那种规则；用户：「1，加 Shift+Tab 轮换，写任务文件给我看」。

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/任务/P2-1-斜杠命令.md`「要做的」和「主控追加」：`/mode` 怎么换模式、选择框和按键为什么要按「按键发生时屏幕上那一帧」判断。
- `docs/任务/P1-14-批准框换回输入框时的Enter.md`「要做的」（保护期）。
- `crates/canter-tui/src/commands.rs`（`MODES`、`mode`、`help`）、`app.rs`（按键分发的先后：批准框、选择框、命令列表、输入框；`info_line`）。
- `crates/canter-tools/README.md`「权限与执行器边界」那张表（三种模式各管什么）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-20-shift-tab`，分支 `p2-20-shift-tab`（已从 main 建好）。
- 只动 `crates/canter-tui/`、`crates/canter/tests/tui.rs`（如需真服务端测试）、本任务文件末尾的完成记录。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-20-shift-tab`。

## 要做的

- 输入框里按 Shift+Tab（crossterm 的 `BackTab`），按这个顺序换到下一个模式：Always ask → Ask for execution → Allow all → Always ask。换模式和 `/mode <名字>` 走同一条路（同样的改设置请求），回答中途也能换，下一个动作按新模式走。
- 往下轮的起点是**按键时屏幕上显示的模式**：连按两下、而服务端还没回话、屏幕还没变时，两下都换到同一个模式，不会一下跳两格。
- 只在输入框能用时生效：批准框、提问框、选择框（包括 `/mode` 的三选一）、命令列表开着时，Shift+Tab 不换模式；批准框、提问框换回输入框的 300 ms 保护期里也不换（这时就是 P1-14 防误按的窗口）。
- 换完以后信息行显示新模式（现在就有）；不另弹提示。
- `/help` 里加一行说明这个快捷键（比如 `Shift+Tab   Cycle permission mode`），样式和命令行一致。

## 怎么算做完

- 输入框里按 Shift+Tab，权限模式按 Always ask → Ask for execution → Allow all → Always ask 轮换，信息行显示新模式；`/mode` 照旧能用。（主控按用户 10-25「1，加 Shift+Tab 轮换」拟，用户看过任务文件后：「验收照你拟的，派吧」）
- 验证只做这些：
  - 针对验收写测试，先写、确认因为还没有快捷键而失败：按三下 Shift+Tab，每一下发出的改设置请求和信息行依次是三种模式，第四下回到 Always ask。
  - 边角用例：屏幕没变时连按两下只换一格；批准框、提问框、选择框、命令列表开着时不换；换回输入框的 300 ms 内不换；回答中途能换。
  - 做完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的 `CARGO_TARGET_DIR`）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `/mode` 的行为、三种模式的含义和权限规则；不加新模式、不做允许／拒绝规则；不改协议、核心、服务端。
- 不改默认权限模式，不加配置项。
- 不调真实模型；测试用隔离的 HOME、`CANTER_HOME` 和临时 socket，不碰用户正在跑的服务端（别读、别停它的 pid）。
- 不升级依赖；要加新依赖的，停下来问。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事（包括有的终端收不到 Shift+Tab、要动上面范围以外的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-20-shift-tab` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-10-25，canter/dev-shifttab。

- **做了什么**（只动 `crates/canter-tui/src/app.rs`、`commands.rs`、`crates/canter-tui/tests/slash.rs` 和 `crates/canter/tests/tui.rs`）：
  - 输入框里按 Shift+Tab（crossterm 的 `BackTab`，不看修饰键），按 `MODES` 的顺序换到下一个模式：always-ask → ask-for-execution → allow-all → always-ask。交出的是和 `/mode <名字>` 同一个 `Action::Update`：`commands.rs` 新加 `set_mode`，`/mode` 和 Shift+Tab 共用；`next_mode` 算下一个。主循环照旧发 `PATCH`，信息行等 `session.updated` 回来再变。
  - 起点按按键那一帧：`Shown` 多记这一帧信息行显示的模式，Shift+Tab 从它往下轮。屏幕还没变时连按几下，都换到同一个模式。一帧都还没画出来时不换。服务端存的是 Unknown 时，信息行显示 Always ask，就从 Always ask 往下轮。
  - 只在输入框能用时生效：批准框、提问框照原来的分派先走自己的按键处理，不认 Shift+Tab；选择框开着时只认上下、Enter、Esc 等，本来就不认；命令列表开着时新加一条，Shift+Tab 什么都不做。批准框、提问框换回输入框的 300 ms 保护期里（P1-14 的 `send_guard_until`）也不换；和别的键一样，这一下会把保护期往后推。
  - `/help` 最后加一行 `Shift+Tab   Cycle permission mode`，和命令行一样是青色名字、灰色说明，列宽按最长的 `Shift+Tab` 算。
  - 没改 `/mode`、三种模式的含义、协议、核心和服务端。服务端本来就允许回答中途改设置，每次调工具都读当时的模式，所以「回答中途换，下一个动作按新模式走」不用改服务端。
- **验证了什么**：
  - 先写测试，改代码前跑过都失败，原因都是没有这个快捷键（Shift+Tab 交出 `None`；真服务端那条信息行一直是 Allow all，等到超时）：
    - 验收：`tui.rs` 的 `shift_tab_cycles_the_permission_mode`（真服务端、假模型）：`--yolo` 开会话，按四下 Shift+Tab，每一下等信息行变，再查服务端存的模式，依次是 Always ask、Ask for execution、Allow all、Always ask。
    - 边角（`slash.rs`，TestBackend）：`shift_tab_on_an_unchanged_screen_moves_one_mode`（同一帧连按两下、重画后服务端还没回话、回话到了还没画出来，都只换到 Ask for execution；画出来以后再按才到 Allow all）；`shift_tab_does_nothing_while_a_box_a_choice_or_the_list_is_open`（批准框、提问框、`/mode` 三选一、命令列表四种，每种先确认打开前 Shift+Tab 能换，打开后不换）；`shift_tab_right_after_a_box_closes_does_nothing`（批准框、提问框被别的终端答掉后 300 ms 内不换，过后能换）；`shift_tab_works_while_the_model_answers`。
  - 反向检查：分别把「从屏幕上的模式轮」改成「从最新收到的模式轮」、去掉命令列表那条、去掉 300 ms 保护，每次只有对应的那个测试失败，改回后全过。
  - `/help` 那行没写测试（验证清单里没有），用一个临时测试画出来看过：在命令后面，列宽、颜色和命令行一致；看完删了，没提交。
  - `cargo test --all-targets` 217 个全过；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过，都带本任务的 `CARGO_TARGET_DIR`。没调真实模型；测试的服务端用隔离目录，测完没有残留进程。
- **拿主意的地方**：
  - 验收测试从 Allow all 起按四下。任务里写「按三下……依次是三种模式，第四下回到 Always ask」：从默认的 Always ask 起按，第三下就回到 Always ask，第四下是 Ask for execution；从 Allow all 起，三下正好依次是 Always ask、Ask for execution、Allow all，第四下回到 Always ask，和字面一致，整圈和绕回都走到了。
  - 批准框、提问框开没开按按键那一帧算，和输入框里别的键一样：框已经来了、还没画出来时，按在输入框那一帧上的 Shift+Tab 照样换模式。命令列表按当前输入算（和 Tab、上下键一样）；选择框按当前状态算（和现在一样，开着就吃掉按键）。
  - 起点按帧上的模式，所以连按两下时，就算第一下的回话已经到了、还没画出来，第二下也换到同一个模式，等于发两次相同的改设置请求。
  - `/help` 列宽按 `Shift+Tab`（9 个字）算，命令的说明比原来往右挪了一格，好让这一行和命令对齐。
  - Shift+Tab 不看修饰键。看过 crossterm 0.29 的解析：老式的 `ESC [ Z` 和 kitty 键盘协议里的 Shift+Tab 都报成 `BackTab`，没发现要另外处理的终端。
- **没做的事**：没在真终端里手动按过，只有 TestBackend 和真服务端测试；`canter --help` 没提这个快捷键（在 `crates/canter/src/`，不在本任务范围）；DESIGN 没改（不涉及按键）。
- **踩过的坑**：
  - 「屏幕没变时只换一格」的测试第一版分不出「从屏幕上的模式轮」和「从最新收到的模式轮」：第一版里回话一到就马上重画，按键时屏幕上的和最新收到的总是同一个模式。补了「回话到了、还没画出来时再按」那一步，反向检查确认它能抓到。
  - clippy 把测试里 `[(&str, fn(&mut Screen)); 4]` 这样的数组报成类型太复杂（`type_complexity`），加了一个类型别名 `Step`。

## 交叉审查后修改

2026-10-25，照 `P2-20-交叉审查.md` 的 2 条必须改（主控都同意）改，canter/dev-shifttab。只动 `crates/canter-tui/src/app.rs` 和 `crates/canter-tui/tests/slash.rs`。每条先加测试，改代码前跑过都失败。

1. **模式被截掉时不轮换**：`Shown.mode` 只在这一帧的信息行把模式文字完整画出来时才记。`info_line` 多交出一个「模式整个在不在」：截断只留开头，所以截完的文字以「模型 · think 档位 · 模式」开头就算在。信息行是最下面一行，只有对话区下面的几行都放得下（`View::below() <= 高度`，和 `render` 共用同一个算法）时才画得出来，放不下也不记。没记时 Shift+Tab 不发更新。
   - 测试 `shift_tab_does_nothing_while_the_mode_is_cut_off`（24×12，信息行只剩 ` test-model · think hig…`）：Shift+Tab 不发更新；回话（`askForExecution`）后重画，模式仍看不见，再按也不发。改前第一下就交出 Ask for execution；去掉第一下的断言再跑，第二下交出 Allow all，就是审查里说的跳过没看到的模式。
2. **按键那一帧有选择框、命令列表时不轮换**：`Shown.mode` 还要求这一帧是空的输入框：没有批准框、提问框，选择框和命令列表都没开（按画这一帧时的状态算，没地方显示的也算开着）。所以那一帧上前一个排队的键（比如 Esc）已经把框关掉，这个 Shift+Tab 也拿不到模式，丢掉。原来按当前状态拦截的两处（选择框先走自己的按键处理、命令列表开着时吞掉 Shift+Tab）都留着。别的键不看 `Shown.mode`，行为不变。
   - `/resume` 的会话列表由 `run.rs` 的 `pick` 另画，本界面不知道那些帧。`App` 多记本界面最后一帧的编号（`latest_frame`）。按键所属的帧比它新，就是画在本界面位置上的别的东西（会话列表），这时 Shift+Tab 拿不到模式。别的键照旧按列表之前那一帧处理，和原来一样。没改 `run.rs`。
   - 测试 `shift_tab_on_a_frame_with_a_choice_or_the_list_does_nothing`：画出 `/think` 的选择框后不重画，依次按 Esc、Shift+Tab，不发更新；命令列表（输入 `/`）同样。之后重画、框不在了，Shift+Tab 照常换到 Ask for execution。改前 Esc 之后的 Shift+Tab 交出 Ask for execution。
- **验证**：
  - 新加的 2 个测试；`slash.rs` 全部 27 个，`approval_box.rs` 13 个，`fullscreen.rs` 7 个；`tui.rs` 里 Shift+Tab、`/mode`、`/resume`、`/think` 和 `/model` 选择框、批准框按帧判断、换回输入框保护期相关的 8 个。都过。照主控说的没重跑全套。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过。
  - 反向检查：分别去掉「模式整个在」「这一帧没有选择框和命令列表」「信息行放得下」「按键帧比本界面最后一帧新」四个条件，每次都只有对应的检查失败，改回后过。
  - 有两项用临时测试查过、没提交（主控只要求上面两个测试）：
    - `/resume`：真服务端，输入 `/resume `（带空格，没有命令列表）回车，会话列表画出后一口气交入 Esc、Shift+Tab；改前服务端的模式被改成 Ask for execution，改后不变，回到输入框后 Shift+Tab 照常能换。
    - 信息行放不下：80×4 的窗口，信息行画不出来，Shift+Tab 不发更新。
- **拿主意的地方**：
  - 选择框、命令列表开着却没地方显示时，也算「那一帧上有」，Shift+Tab 丢掉。这样宁可少换一次，不会在看不到框的时候换模式。
  - `/resume` 没有在 `run.rs` 里另记会话列表的帧，而是在 `App` 里用帧编号判断，好让以后别的画在本界面位置上的东西也自动算进来。
- **踩过的坑**：没有。

## 合并 main（P2-6）后修改

2026-10-25，canter/dev-shifttab，照主控要求。

- **做了什么**：
  - `git merge main`（没有 rebase），把 P2-6 状态栏合进来。冲突只在 `app.rs`，共五处：`view()` 末尾、`info_line` 改名 `info_rows` 的签名和内容（三处）、`render` 里的行数。都按 P2-6 的两行布局写，再把本任务的判断接上。
  - 「模式文字确实画出来了」改按两行判断。`info_rows` 多交出第一行有没有被截：模式在第一行末尾，第一行没被截，模式就整个画出来了。第一行是两行里上面那行；对话区下面的行放不下时从最下面截，所以从提示行到第一行信息都放得下（`View::below() - (INFO_ROWS - 1) <= 高度`）才算画出来。第二行（目录那行）被截或画不出来不影响。`View::below()` 改用 `INFO_ROWS`，`render` 和这里共用。
  - 截断那条测试跟着改：原来取最后一行，现在最后一行是目录行，不改的话断言会白白通过。改成取倒数第二行（新加小帮手 `mode_row`），并先确认这一行确实是「模型 · think」那行。
  - 两个临时检查留成正式测试：
    - `slash.rs` 的 `shift_tab_needs_the_first_info_row_drawn`：80×5 只放得下第一行信息（模式看得见、目录那行被截），Shift+Tab 照常换；80×4 连第一行也画不出来，不换。80×5 那一半是新加的：没有它，「两行都画出来才算」的写法也能通过，验不到按第一行判断。
    - `tui.rs` 的 `shift_tab_pressed_on_the_session_list_changes_nothing`（真服务端）：输入 `/resume `（带空格，没有命令列表）回车，会话列表上屏后按 Esc，紧接着在同一帧上按 Shift+Tab。回到输入框后，服务端的模式还是 Always ask；再按一次 Shift+Tab 才换到 Ask for execution。
- **验证**：
  - 反向检查（在合并后的代码上）：改成两行都画出来才算，新测试在 80×5 那步失败；不看第一行画没画出来，80×4 那步失败；不看模式有没有被截，截断那条失败；去掉会话列表帧的判断，`/resume` 那条失败。改回后都过。
  - 合并后跑一次 `cargo test --all-targets`，229 个全过；`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都过。测完没有残留的测试服务端。
- **拿主意的地方**：
  - 第一行画出来、第二行被截时，Shift+Tab 照常能用：模式就在第一行，用户看得见。
  - `/resume` 那条测试靠测试跑在单线程运行时里：`press(Esc)` 等会话列表那一帧上屏后发出 Esc，紧接着同步发 Shift+Tab，中间客户端插不进来。所以两个键都落在会话列表那一帧上，不会因为时序变成落在新画的输入框上。
- **踩过的坑**：没有。

## 主控审查

2026-10-25，canter/main。

- diff 只动了 `crates/canter-tui/` 和测试。`/mode` 和 Shift+Tab 共用 `set_mode`；起点是按键那一帧屏幕上的模式。在 `470e496` 上重跑 `cargo test --all-targets`：217 个通过；clippy、`fmt --check`、`git diff --check` 通过。
- 交叉审查（canter/review-shifttab，Codex gpt-6-astra / xhigh，见 `P2-20-交叉审查.md`）：必须改 2 条，主控同意：模式文字被信息行截掉时仍会往下轮、可能跳过没看到的模式进 Allow all；选择框／命令列表那一帧上排队的 Shift+Tab 在 Esc 关框后会漏到输入框。`8187b8e` 改完。期间 P2-6 先合并（权限模式挪到状态栏第一行），主控让开发者合并 main 并按第一行判断（`7fe487f`），把临时检查留成正式测试。复核可以合并。
- 合并后在 main 上 `cargo test --all-targets` 229 个通过，clippy、`fmt --check` 通过。
- 没验到的：各家真终端是否都把 Shift+Tab 发成 `BackTab`（crossterm 0.29 认传统 `ESC [ Z` 和 CSI-u），用户装好后试。

结果：返工 1 次
