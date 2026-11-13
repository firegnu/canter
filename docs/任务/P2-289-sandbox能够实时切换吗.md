# 任务：sandbox能够实时切换吗

2026-11-13，用户在看板上新建；同日主控和用户定了需求。canter/main 交给 canter/dev-p2-289（Codex，`gpt-6-astra` / xhigh）。
路由：重 / 交叉审查要 / 影响面：碰要害（路由：重（置信 0.88）、要、碰要害）
类型：功能变更
依据：本轮让会话开着时能用 `/sandbox on|off` 切换这个会话的沙箱；不改权限档、配置和别的会话。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-13：「现在的需求是一旦开canter的时候，sandbox就固定了，在整个session阶段，我都切换不了sandbox」
- 11-13 主控问了四件事，用户都选了推荐：
  - 范围：「只当前会话」
  - 入口：「/sandbox on|off」
  - 关掉时：「关掉时确认一次」
  - 重开会话后：「回到按配置」
- 11-13：「所以我理解这个其实爆炸半径不大。对吧？」
- 11-13：「如果我开了好几个canter，而且都在工作，如果我改了其中一个sandbox的状态，那么其他的正在运行的canter的sandbox状态会受影响吗？」主控答不会（标记在各自会话的运行状态里），提议写进验收。
- 11-13：「同意，写进去提交吧」（同意下面第 1 条的「配置关着也能打开」、第 6 条「下一轮起生效」和验收里的多会话一条）

## 先读

- `AGENTS.md` 或 `CLAUDE.md`（仓库根）的「规矩」。
- `docs/DESIGN.md` 第 8 条里「启动时关闭沙箱（P2-198）」「沙箱状态（P2-166）」两段，第 440 行附近「配置自动重新读取」那段里沙箱的说法。
- `crates/canter-tui/README.md` 的 `/sandbox`（P2-166）。
- `crates/canter-core/src/configuration.rs` 的 `set_sandbox_disabled` 和按配置重算工具那段（`config.options.sandbox && !context.sandbox_disabled`）；`crates/canter-core/src/agents.rs` 里子 agent 继承标记的地方。

## 现在的样子

- 沙箱只能在会话打开那一刻定：按 `config.json` 的 `sandbox`，或打开时带 `--yolo` 强制关掉（P2-198，同时切到 Allow all）。
- 改 `config.json` 的 `sandbox` 能在下一轮生效，但会改到所有会话。
- `/sandbox` 只看不改（P2-166）。
- 核心里已有运行中改单个会话沙箱的入口：`set_sandbox_disabled`（同一会话被带 `--yolo` 重新打开时用）。标记 `runtime.context.sandbox_disabled` 只有「强制关」和「按配置」两种；子 agent 起来时继承父会话的标记。

## 在哪里干活

- worktree：`../canter-worktrees/p2-289`，分支 `p2-289`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-289`。

## 要做的

1. 会话开着的时候，用户在终端里输入 `/sandbox off` 关掉这个会话的沙箱，`/sandbox on` 打开它。不管 `config.json` 的 `sandbox` 是什么都能切：配置里关着沙箱时，`/sandbox on` 也能打开（现有标记要多一种「强制开」）。
2. 只改当前这个会话：别的会话、`config.json` 都不变。同一个会话开在几个终端里时，几个终端一起看到新状态。
3. `/sandbox off` 先弹一次确认，用户同意了才关；`/sandbox on` 直接打开，不用确认。
4. 重开这个会话（`/resume`、`-c`、`-r`）后，沙箱回到按配置走，和 `--yolo` 一样不存进会话。
5. `/sandbox` 不带参数照旧只看状态。
6. 从下一轮起生效，正在跑的这一轮不变。已经在跑的子 agent 保持原来的状态，切换后新起的子 agent 跟着新状态。
7. 协议改动（字段、请求、`features`）先写进 `docs/DESIGN.md` 第 8 条，写明和 Codex `rust-v0.163.0` 结构的差别和理由；`crates/canter-tui/README.md` 的 `/sandbox` 一节同步。

## 怎么算做完

- 验收（照抄用户原话）：
  - 会话开着时能切换沙箱，不再「一旦开canter的时候，sandbox就固定了」。
  - 只切当前会话；用 `/sandbox on|off`；关掉时确认一次；重开会话后回到按配置。
  - 「如果我开了好几个canter，而且都在工作，如果我改了其中一个sandbox的状态，那么其他的正在运行的canter的sandbox状态会受影响吗？」：开两个会话、都在干活，在其中一个里 `/sandbox off`，另一个的沙箱状态和它跑的命令都不受影响（用户同意写进验收）。
- 验证只做这些：
  - 先写针对这件活的测试，确认它在改之前失败：切换后下一轮的实际执行器状态、另一个会话不受影响、重开后回到按配置、配置关着时 `on` 能打开、切换后新起的子 agent 跟着新状态、`off` 不确认就不关。
  - 跑改到的包的测试：`cargo nextest run -p canter-core -p canter-protocol -p canter-server -p canter-tui`（改了 `crates/canter` 再加 `-p canter`）。
  - `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 不跑全套，不录屏。觉得不够，在回复里说，不要自己加。

## 不要做

- 不把沙箱放进 Shift+Tab 的权限档轮换，也不改权限档：`/sandbox off` 不像 `--yolo` 那样同时切 Allow all。
- 不改 `config.json`，不加配置项；不把切换结果存进会话。
- 界面上的字用英文（确认框、提示、状态）；改终端界面的 diff 里不要出现中文。
- 不针对某一家模型写特例。
- 不调真实的模型接口，测试用假服务端和临时 `CANTER_HOME`；不碰用户的 `~/.canter`。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务用起的时候记下的 PID。
- 碰到要拿主意的事（包括：现有 `set_sandbox_disabled` 是当场换工具，和「下一轮起生效」对不上时怎么处理；正在跑的后台命令怎么算），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-289` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
