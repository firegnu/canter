# 任务：第三期 - 项目里没有 AGENTS.md 时读 CLAUDE.md

依据：canter 只读 `AGENTS.md`，用户的项目大多只有 `CLAUDE.md`（canter 仓库自己就是），在这些项目里用 canter 时看不到项目规矩。pi、omp 都会读 `CLAUDE.md`。
执行：派出
待用户：还没分解，派之前主控补全、给用户看过；先问下面「待用户定」那几条

## 用户原话

- 11-07 主控对比 pi、omp 时指出这个缺口，建议另开一件小活。用户：「好，写ast_grep任务文件给我看，CLAUDE.md也排上」

## 现状（主控查过的）

- 读说明文件在 `crates/canter-core/src/context.rs`（`dir.join("AGENTS.md")` 那两处）：全局读 `<CANTER_HOME>/AGENTS.md`；项目从 Git 仓库根到会话目录，每层读 `AGENTS.md`。每个文件最多 64 KiB，只在打开会话时读（DESIGN 第 17 条缓存规矩）。
- 用户文档：`docs/usage.md`「Instructions and AGENTS.md」。
- pi 的做法（`/tmp/pi-mono/packages/coding-agent/docs/configuration.md`）：每个目录按 `AGENTS.override.md`、`AGENTS.md`、`AGENTS.MD`、`CLAUDE.md`、`CLAUDE.MD` 找。

## 待用户定（主控建议）

1. 同一层两个都有：只读 `AGENTS.md`，不读 `CLAUDE.md`（两个常是同样内容或软链接，都读会重复）。建议这样。
2. 全局：不读 Claude Code 的 `~/.claude/CLAUDE.md`（那是别的工具的目录，里面是写给 Claude 的话）；全局仍只读 `<CANTER_HOME>/AGENTS.md`。建议这样。
3. `CLAUDE.md` 里 Claude Code 的 `@路径` 引入写法：当普通文字，不展开。建议这样。
4. `/context` 等显示照实写读的是哪个文件名。
