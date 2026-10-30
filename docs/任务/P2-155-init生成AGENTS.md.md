# 任务：第二期-18 - /init 生成 AGENTS.md

2026-10-30，canter/main 交给 canter/dev-init（Codex，gpt-6-astra / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.79）、拿不准、拿不准；主控判：不碰要害，只加一条命令和一段提示词）
类型：功能变更
依据：照 Claude Code、Codex 的 `/init`：读一遍仓库，写出构建、测试命令和项目约定。新仓库开工省事，AGENTS.md 写得好，长任务和 eval 的成绩也会更好。Codex（`rust-v0.163.0`）的做法：`/init` 把一段固定的英文提示词（`tui/assets/prompt_for_init_command.md`）当成一条普通的用户消息发出去，模型照常用工具读仓库、写文件，没有别的特殊处理。canter 照这个做。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。
依赖：无
执行：派出

## 用户原话

- 10-29 主控对照 Claude Code、Codex 日常功能，提了改配置和升级会打断长任务、`/init`、`/permissions`、会话分叉四项；用户：「这四项我都需要排进来，还都是我日常都在使用的。」
- 10-30 用户：「能不能剩下的挑几个简单的，不碰核心的做做。例如141 143之类的」；主控建议 `/init` 发一段固定的话，让模型读一遍仓库再写，已经有 AGENTS.md 的就改进它、不覆盖。用户：「同意，从 /clear 开始」；`/clear` 做完后用户：「好，接着写 /init 的任务文件」。

## 先读

- `crates/canter-tui/src/commands.rs`：命令表 `COMMANDS`。
- `crates/canter-tui/src/app.rs`：普通消息怎么发出去（`Action::Send` 一带）。
- Codex 的提示词：`/tmp/r49-src/codex/codex-rs/tui/assets/prompt_for_init_command.md`（只读克隆；没有的话 `git clone --depth 1 --branch rust-v0.163.0 https://github.com/openai/codex /tmp/r49-src/codex`）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-155-init`，分支 `p2-155-init`（已从 main 建好）。只动 `crates/canter-tui/`，加上 `NOTICE`（如果搬了 Codex 的提示词原文）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-155-init`。

## 要做的

1. **加命令 `/init`**：不带参数（带了照别的不带参数命令的规矩拒绝），命令列表和 `?` 帮助里说明写 `Create or improve AGENTS.md for this project`。
2. **发出去的就是一条普通消息**：照 Codex，把一段固定的英文提示词当成用户消息发给模型，和用户自己打字发出去的走同一条路（正在干活时照普通消息的规矩排队）。对话里照普通消息显示这段提示词。权限照当前模式走，不为 `/init` 单独弹批准框（「只问执行」下模型直接写文件，用户事后看「Changed files」、`/diff`，不满意 `/rewind`）。
3. **提示词**：英文，放成单独的文件（比如 `crates/canter-tui/assets/init_prompt.md`），编译时读进来。照 Codex 那份的结构写：在会话的工作目录写 `AGENTS.md`，当成这个仓库的贡献者指南；先读仓库（结构、构建和测试命令、代码风格、测试、提交约定），内容简短、具体到这个仓库，给出命令和路径的例子。和 Codex 不同的一处：**已经有 `AGENTS.md` 时不跳过，改进它**——先读现有内容，保留用户写的，只补缺的、改过时的，不整份重写。
4. **搬来的要注明**：如果直接用了 Codex 提示词的原文（哪怕改了几句），照 CLAUDE.md「从别的项目搬来的代码」：文件开头注明来源（Codex、`rust-v0.163.0`、`codex-rs/tui/assets/prompt_for_init_command.md`），按 Apache-2.0 维护 `NOTICE`。只参考、自己重写的，在完成记录里写参考了哪里。

碰到要拿主意的事，停下来问主控。

## 怎么算做完

- （用户 10-30 同意的）`/init` 发一段固定的话，让模型读一遍仓库再写 AGENTS.md；已经有 AGENTS.md 的就改进它，不覆盖。
- 验证只做这些：一条测试，输入 `/init` 发出的是那段提示词、走的是普通消息那条路；带参数被拒绝；命令列表和帮助里有 `/init` 和说明。再跑 `cargo nextest run -p canter-tui`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。觉得不够，在回复里说，不要自己加。
- 真让模型跑一次 `/init` 由用户装版后自己试。

## 不要做

- 不动服务端、协议和权限规则；不为 `/init` 加专门的批准框、预览或确认步骤。
- 不让 `/init` 把对话里显示的提示词藏起来、换成一行字（那要动协议，不做）。
- 不改 canter 读 AGENTS.md 的规矩（打开会话时读，DESIGN 第 17 条）。
- 测试用隔离的 HOME 和 `CANTER_HOME`，不读用户真实的 `~/.canter`；不真调模型接口。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-155-init` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
