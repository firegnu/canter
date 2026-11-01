# 任务：第二期-10 - 在新文件夹工作的时候第一次 trust

2026-10-31，用户在看板上新建；11-01 主控补全。
2026-11-01，canter/main 交给 canter/dev-trust（Codex，gpt-6-astra / xhigh）。
路由：碰要害 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.98）、要、碰要害）
类型：功能变更
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依据：照 Codex、Claude Code，第一次在某个文件夹里打开时问用户信不信任这个文件夹，信任才加载项目里会自动生效的设置。目的是防别人仓库里的配置、hooks 在打开 agent 时自动跑代码，不是防模型。canter 现在打开文件夹只有 LSP 会自动跑项目里的东西（rust-analyzer 跑 build.rs、语言服务器读项目配置），以后的 P2-159 项目级配置、P2-153 hooks 都要看本件定下的信任状态。
依赖：P2-154
执行：派出

## 用户原话

- 10-31 用户：「在做遥测之前有一个任务，就是P2-168.这个你能评估一下吗？」「因为codex和claude code第一次在某一个文件夹下工作的时候就会弹出一个trust的选项。」
- 主控评估：Codex 不信任时以受限方式打开（项目配置、hooks、命令规则不加载，技能照常），信任按 git 仓库根存；canter 现在没有项目级配置和 hooks，弹框暂时没东西可挡。用户：「我不明白codex和claude code谈这个文件夹的目的在哪？」主控解释是防别人仓库里的设置在打开时自动跑代码，建议挪到项目级配置前面一起做。用户：「可以，挪到项目级配置前面」
- 11-01 主控问了六件事，用户都选了主控的建议：
  1. 不信任时：「受限打开」——照 Codex，照常能用，但不加载项目里会自动跑代码的东西（以后的项目级配置、hooks；现在是 LSP 不启动）。
  2. 信任记在哪一级：「git 仓库根」——在仓库里任何子文件夹打开都算同一个仓库，只问一次；不在 git 仓库里的按文件夹本身记。
  3. 以前用过的文件夹：「不算，第一次照样问」。
  4. 不信任时项目的 AGENTS.md 和技能：「照常加载」——它们只是给模型看的文字，本身不会在打开时跑代码。
  5. 选了不信任以后：「不记，下次再问」——只记「信任」，选错了下次打开就能改过来，不另做撤销命令。
  6. `canter exec` 碰到没信任过的仓库：「不问，按受限跑」——信任过的照常；没信任过的按受限模式跑，开头打一行提示，不加新参数。

## 先读

- `docs/DESIGN.md`：「LSP 起停和配置（P2-132）」「非交互模式（P2-30）」「改配置不用重启（P2-154）」几节，第 8 条协议。
- `crates/canter-protocol/src/commands.rs` 的 `SessionCreateParams`（`cwd`、`additional_directories`）；`crates/canter-protocol/src/types.rs` 的会话设置。
- `crates/canter-server/src/http.rs`：`App::configuration`、`session_options`、新建和 `resume` 会话的路由。
- `crates/canter-core/src/session.rs` 里 LSP 怎么挂到会话上（`tools.lsp()`、`crate::lsp::watch`）；`crates/canter-tools/src/lsp/service.rs` 起语言服务器的入口。
- 终端界面开会话、`canter -r` 打开存盘会话的流程（`crates/canter-tui/src/`），和 `crates/canter/src/exec.rs`。
- 参考（只读，放 `/tmp` 下的克隆）：Codex 的 trust 弹框和 `trust_level` 存法，Claude Code 的 trust 弹框文字。只参考不搬，在完成记录里写参考了哪里。

## 在哪里干活

- worktree：`../canter-worktrees/p2-168-trust`，分支 `p2-168-trust`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-168-trust`。

## 要做的

1. **信任记录**：服务端在 canter 自己的状态目录里记哪些地方信任过。按 git 仓库根记（工作目录在仓库里任何子文件夹都算这个仓库）；不在 git 仓库里的按工作目录本身记。只记「信任」，不记「不信任」。以前用过的文件夹不自动算信任。
2. **第一次打开时问**：终端界面在一个没信任过的地方开会话（新开或 `canter -r` 打开存盘会话）时，先弹框问信不信任，两个选项：信任、受限打开。选信任就记下、照常打开；选受限就这次按受限模式打开，下次还问。信任过的不再问。弹框和提示文字用英文。
3. **受限模式挡什么**：现在只挡 LSP——受限的会话不启动语言服务器（即使全局配置开了 LSP），模型拿不到诊断。项目的 AGENTS.md、技能照常加载；全局 `config.json` 里的 MCP、放行规则、沙箱照常（它们是用户自己的设置，不是项目带来的）。受限会话里把"这是受限模式、LSP 没开"用一行英文提示告诉用户。
4. **`canter exec`**：不弹框。信任过的照常；没信任过的按受限模式跑，在 stderr 打一行英文提示。不加新参数。
5. **服务端定、协议传**：信任状态的查询、记录和会话是否受限都在服务端，经协议给客户端，以后 GPUI、网页客户端照样能用。协议里加的东西和 Codex app-server v2 结构不同的地方写进 `docs/DESIGN.md`，说明为什么。
6. **DESIGN**：写一节「文件夹信任（P2-168）」，写清上面 1～5 条，并写明以后 P2-159 项目级配置、P2-153 hooks 在受限模式下不加载。

## 怎么算做完

- 用户 11-01 选的六条（见「用户原话」）都做到：没信任过的地方第一次打开时弹框；选信任以后在这个仓库任何子文件夹打开都不再问；选受限就照常能用但 LSP 不启动、AGENTS.md 和技能照常加载，下次再问；以前用过的文件夹也问一次；`canter exec` 不问，没信任过的按受限跑并提示一行。
- 验证只做这些：先写针对性的测试，确认因为现在没有信任检查而失败：没信任过的工作目录开会话时，全局开了 LSP，受限会话不起语言服务器（用测试里已有的假语言服务器）；记了信任以后，同一仓库的子文件夹查出来是已信任。边角再补几条：不在 git 仓库里的文件夹按自身记；选受限后不留记录；`canter exec` 在没信任过的目录跑不等弹框、stderr 有提示；终端界面弹框的两个选项各走一遍（用现有的终端界面测试办法）。再跑你改到的包的相关测试，加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。不跑全套，不调真实模型。

## 不要做

- 不做 P2-159 项目级配置、P2-153 hooks，不为它们预先加配置项；本件只把信任状态做好、让 LSP 照它走。
- 不做撤销信任、列出信任的命令或界面。
- 不改全局 `config.json` 的配置项名字、默认值和校验规则；不加新配置项（为了本件必须加的除外，先停下问主控）。
- 不读、不改用户真实的 `~/.canter`；测试用隔离的 HOME 和 canter 状态目录、临时目录建测试用的 git 仓库、临时 socket，不碰用户的会话和配置。测试不调真实模型接口。
- 界面上的文字用英文。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-168-trust` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

2026-11-01，canter/dev-trust 完成，提交在 `p2-168-trust`。

- **做了什么**：服务端新增正向信任记录 `trusted_folders.json`（状态目录内，0600、加锁、临时文件原子替换），按规范化的最近 git 仓库根或非 Git 目录自身查找，不从以前的会话推断信任。新增协议查询/记录端点、`folderTrust` 能力和可选会话设置 `restrictedMode`，同步生成 JSON Schema / TypeScript。终端在新建、`-r` / `-c`、会话列表和 `/new` 等入口先查询并显示英文信任选择；受限不留记录，AGENTS.md、技能、全局 MCP / 放行规则 / 沙箱保留原行为。exec 不询问，受限时 stderr 提示一行。核心同时保护首次打开和配置热重载；受限会话不挂 LSP，已经打开的会话在记录信任并恢复后可以正常使用 LSP。DESIGN 第 8 条和「文件夹信任（P2-168）」已说明协议差异与未来项目配置、hooks 的约束。
- **主控决定**：本轮提出额外目录的边界问题后，主控选择中间办法：加目录不弹框、不加询问流程；工作目录受限时整个会话不启 LSP；工作目录已信任时，对文件按其 git 仓库根判断，非 Git 文件按覆盖它的已添加目录本身判断。A 的信任不放行 B；B 已被单独信任才允许其文件启动 LSP。原因是 B 的构建脚本同样可能被运行，信任仍按用户确定的仓库范围，而加目录的额外交互不在本件范围。已经写入 DESIGN，并用 Git / 非 Git 两种 B（含子目录文件）验证了未信任拦截、记录信任后恢复。没有新的待主控决定事项。
- **验证了什么**：先写并运行针对性测试，确认未信任目录会启动假 LSP、缺少信任查询端点、A 的信任会放行 B、终端没有信任框、exec 没有受限提示这五种目标失败，再分别实现到通过。补验同仓库子目录共用记录、非 Git 目录不继承父目录记录、受限不写记录、AGENTS.md / 技能仍进模型上下文、热重载不解除受限、保存会话重启后恢复仍询问、终端两个选项和已信任子目录免询问、exec 无交互完成及同一已打开会话的信任升级。所有模型请求均走本地假模型，语言服务器用已有 Rust fixture。
- **回归与检查**：`canter-server` 的 `reload_tests` 19 项、HTTP 相关测试 82 项通过；`canter-core` LSP 22 项、prompts 3 项、session 串行复跑 36 项通过；`canter-tools --lib lsp::` 37 项通过、1 项既有忽略；`canter-protocol` 与 `canter-store` 共 48 项通过；`canter --test exec` 16 项、最终 `canter --test tui` 69 项通过。额外目录测试扩为 Git / 非 Git 后，4 项 trust 测试再次通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。编译均使用指定的独立 `CARGO_TARGET_DIR`，运行测试时隔离 HOME / CANTER_HOME；测试服务和 PTY 子进程按各 fixture 持有的 PID 结束。未跑工作区全套测试。
- **参考了哪里**：只读 `/tmp/codex-ro` 的 Codex `rust-v0.163.0`：`codex-rs/tui/src/onboarding/trust_directory.rs` 的目录范围说明和显式确认，`codex-rs/config/src/config_toml.rs` 的 `ProjectConfig.trust_level`、`config/src/project_trust.rs` 的路径查询及 `core/src/config/edit.rs` 的信任写入入口。Claude Code 官方仓库克隆在 `/tmp/p2-168-claude-code-reference`，提交 `80c6959d28c4c30ecd14fc1cdf172f19d9adad81`，参考 `CHANGELOG.md` 对仓库范围和嵌套仓库信任的说明；此克隆不含终端信任框实现，文字另核对官方仓库 [issue #9256](https://github.com/anthropics/claude-code/issues/9256) 所附 2.0.13 对话框（询问是否信任目录、明确继续或退出）。仅参考，没有复制实现。
- **没做的事**：未实现 P2-159 项目级配置或 P2-153 hooks，未增加全局配置项、exec 参数、撤销/列出信任命令或加目录询问。未读改真实 `~/.canter`，未调真实模型，未启动其他 agent，未推送或合并到 main；交叉审查交回主控。
- **踩过的坑**：假 LSP 是异步启动，写完文件立刻看进程记录会误判，改为让假模型轮保持打开并有界等待后再断言；reqwest 没启用 query builder，测试改用已有 URL 类型编码查询参数。恢复 LSP 订阅时一度复用了完整 `allowed` 检查，macOS 路径别名导致已停止订阅未恢复，既有重试测试抓到后改成仅检查信任，回归通过。旧 PTY 用例没有预设信任而停在新框，已在它们的隔离 fixture 中显式记信任。核心 session 并行回归有一次 `session already open` 存盘锁冲突，该用例单独重跑及整个文件串行复跑都通过，没有改动存盘锁实现。


## 返工记录

2026-11-01，第 1 次，按主控及交叉审查意见完成，提交在 `p2-168-trust`。

- **坏记录与警告**：查询遇到 `trusted_folders.json` 解析／读取错误，返回未信任并受限打开；创建和恢复会话不因此失败。英文警告包含记录文件完整路径和具体错误，先写入会话事件再返回，客户端从警告之前的游标订阅。POST 仍报错，不覆盖坏记录。终端查询失败直接请求受限打开并显示警告，不退出；协议新增可选请求字段 `restrictedMode`，只能要求更严格的打开方式，不能放行未信任目录。exec 恢复保留打开响应的游标，避免后续读快照漏掉警告；没有删减原有恢复读取。
- **目录删除**：信任查询遇到不存在的 cwd 返回未信任；已打开会话和存盘会话都可以受限恢复，覆盖 `-r` 和 `/resume`。新建会话仍拒绝不存在的目录，错误明确为 `Directory does not exist`。恢复时原本已信任的会话也会降为受限，卸载 LSP、清除待交付诊断并把公开状态改为 Stopped；正在运行的轮后续写文件也不能重新起 LSP。
- **worktree 与信任边界**：落实用户 11-01 的新决定：有效 Git worktree 共用主仓库信任。检查 `.git` → gitdir、gitdir 反向指回当前 checkout、commondir 指向正确公共目录，以及主仓库 `.git` 对该目录的所有权；校验不过按当前 worktree 自身判断。查询、exec 服务端打开和 LSP 按文件检查使用同一解析。只参考 `/tmp/codex-ro/codex-rs/git-utils/src/trust.rs`（Codex `rust-v0.163.0` 的 `resolve_root_git_project_for_trust`），以本地同步文件操作独立实现，没有搬代码。DESIGN 第 8 条和文件夹信任一节已更新，并明确子模块／嵌套仓库单独判断、未信任时不起 LSP 不弹框，需要在那里单独开 canter 来信任；加目录不弹框的既定决定保留。
- **假服务端及指定 reload 测试**：给 `canter-tui::fake::FakeServer` 补齐 info 的 `folderTrust` 能力和查询端点。先复现 notifications 8 项、tab_title 2 项及 editor 1 项失败，再修到全部通过，未改断言和超时。核对指定 `disabling_then_enabling_lsp_still_waits_for_the_original_turn` 的原改动，只有创建会话前已经 await 完成的信任写入，原等待、模型请求门控和断言均未修改；本轮在两个相关并行批次中都通过，未复现主控报告的一次失败，没有证据把它归因于新增的测试时序，因此保留原测试，并把未能复现的情况如实记录。
- **回归与检查**：坏记录查询／创建／恢复、POST 保留坏文件、删除目录后恢复、worktree 共享和 gitdir／commondir 反向链接错误等目标检查均先确认旧行为失败，再实现通过。最终相关服务端 10 项通过（含指定 reload 测试、受限 LSP、额外目录隔离）；`cargo nextest run -p canter-tui` 整包 582 项通过；终端及 exec 相关 17 项通过（含 `/new`、`/handoff`、计划新会话、`-r`、`/resume`）；协议 37 项通过，生成物一致性检查通过；核心 LSP 串行复跑 22 项通过。`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。所有命令前台等待完成，测试使用指定独立 target、隔离 HOME / CANTER_HOME 和本地假模型；未跑工作区全套。
- **踩过的坑与验证限制**：删除目录的终端测试起初把 fixture 的服务端启动目录一起删了，改成单独的待删会话目录；跨目录 `/resume` 测试必须先 Tab 显示全部目录。补测发现 exec 恢复会跳过打开后发出的警告，保留恢复游标后创建／恢复均能显示。核心 LSP 与服务端合跑的 32 项中有 6 项核心 LSP 诊断时序失败，主要是未在既有 500 ms 窗口内收到诊断；这组核心测试不调用本轮新增的信任更新／警告方法，nextest 每个测试进程各自 rustc 编译假 LSP。保留原代码、断言和超时，串行 22 项全部通过，证据指向并行负载／启动抖动，未扩大本件去改这批测试。失败日志在 `/tmp/canter-p2-168-check/core-server-rework-final.log`，串行结果在 `core-lsp-rework-serial.log`，供主控合并前全套验证参考。
- **没做的事与待决事项**：按主控要求未改“家目录本身是仓库”、LSP 每次读取信任记录、恢复多次读取三项建议，HANDOFF 由主控记；未加全局配置、CLI 参数或加目录询问，未修改本任务之外的测试断言／超时。未读改真实 `~/.canter`、未调用真实模型、未开其它 agent、未推送或合并 main。没有新增需要主控决定的事项。
