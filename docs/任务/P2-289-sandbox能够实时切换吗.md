# 任务：sandbox能够实时切换吗

2026-11-13，用户在看板上新建；同日主控和用户定了需求。
依据：DESIGN 第 8 条「启动时关闭沙箱（P2-198）」「沙箱状态（P2-166）」；`crates/canter-tui/README.md` 的 `/sandbox`
执行：派出

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

## 现在的样子

- 沙箱只能在会话打开那一刻定：按 `config.json` 的 `sandbox`，或打开时带 `--yolo` 强制关掉（P2-198，同时切到 Allow all）。
- 改 `config.json` 的 `sandbox` 能在下一轮生效，但会改到所有会话。
- `/sandbox` 只看不改（P2-166）。
- 核心里已有运行中改单个会话沙箱的入口：`crates/canter-core/src/configuration.rs` 的 `set_sandbox_disabled`（同一会话被带 `--yolo` 重新打开时用）。标记 `runtime.context.sandbox_disabled` 只有「强制关」和「按配置」两种；子 agent 起来时继承父会话的标记（`agents.rs`）。

## 在哪里干活

- worktree：`../canter-worktrees/p2-289`，分支 `p2-289`（已从 main 建好）。

## 要的结果

1. 会话开着的时候，用户在终端里输入 `/sandbox off` 关掉这个会话的沙箱，`/sandbox on` 打开它。不管 `config.json` 的 `sandbox` 是什么都能切：配置里关着沙箱时，`/sandbox on` 也能打开（现有标记要多一种「强制开」）。
2. 只改当前这个会话：别的会话、`config.json` 都不变。同一个会话开在几个终端里时，几个终端一起看到新状态。
3. `/sandbox off` 先弹一次确认，用户同意了才关；`/sandbox on` 直接打开，不用确认。
4. 重开这个会话（`/resume`、`-c`、`-r`）后，沙箱回到按配置走，和 `--yolo` 一样不存进会话。
5. `/sandbox` 不带参数照旧只看状态。
6. 从下一轮起生效，正在跑的这一轮不变。已经在跑的子 agent 保持原来的状态，切换后新起的子 agent 跟着新状态。

## 验收

照抄用户原话：

- 会话开着时能切换沙箱，不再「一旦开canter的时候，sandbox就固定了」。
- 只切当前会话；用 `/sandbox on|off`；关掉时确认一次；重开会话后回到按配置。
- 「如果我开了好几个canter，而且都在工作，如果我改了其中一个sandbox的状态，那么其他的正在运行的canter的sandbox状态会受影响吗？」：开两个会话、都在干活，在其中一个里 `/sandbox off`，另一个的沙箱状态和它跑的命令都不受影响（用户同意写进验收）。

## 不要做

- 不把沙箱放进 Shift+Tab 的权限档轮换，也不改权限档：`/sandbox off` 不像 `--yolo` 那样同时切 Allow all。
- 不改 `config.json`，不加配置项。
- 界面上的字用英文（确认框、提示、状态）。
- 协议改动写进 DESIGN 第 8 条和 Codex 结构的差别；不针对某一家模型。
