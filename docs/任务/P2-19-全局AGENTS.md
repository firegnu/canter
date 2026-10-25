# 任务：全局 AGENTS.md

2026-10-25，canter/main 交给 canter/dev-agentsmd（Codex，gpt-6-astra / high）。
路由：中 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.85）、拿不准、拿不准；后两项主控定：只改拼系统提示词的一处，读的是用户自己的说明文件，规则照项目 `AGENTS.md`，不碰权限和存盘）
类型：功能变更
依据：第二期第 19 件（`docs/任务/P2-0-第二期需求单.md`）。项目里的 `AGENTS.md` 第一期已支持；本轮再读一份全局的 `~/.agents/AGENTS.md`，排在项目的前面。不读别家 agent 的配置。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
依赖：P2-15
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 10-25 试 P2-3 时：「还有支持AGETNS.md(系统目录的和项目目录的）也要做吧？」主控说明项目的已支持、全局的没有，建议放 `~/.agents/AGENTS.md`、排在项目前面、不读 `~/.codex/AGENTS.md`；用户：「这个先标记为P2-0的一个任务。」
- 10-25：「都照你的建议，写 P2-4 任务文件给我看。P2-19一块」；看过任务文件后：「验收照你拟的，两件一起派吧」。

## 先读

- `CLAUDE.md`：「测试不依赖用户状态」「验证」「独立编译目录」。
- `docs/DESIGN.md` 第 17 条。
- `crates/canter-core/src/context.rs` 的 `prefix()`（项目 `AGENTS.md` 怎么读、64 KiB 上限、警告怎么出、P2-3 的 HOME 从哪来）；`session.rs` 的 `initialize_prefix()`；`crates/canter-core/README.md` 讲上下文的地方。
- `crates/canter/tests/server.rs` 里 P2-3 技能的测试（临时 HOME 怎么放文件、怎么拿到发给模型的系统提示词）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-19-global-agents`，分支 `p2-19-global-agents`（已从 main 建好）。
- 只动 `crates/canter-core/src/context.rs`、`crates/canter-core/README.md`、测试（`crates/canter-core/tests/` 或 `crates/canter/tests/server.rs`）、本任务文件末尾的完成记录。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-19-global-agents`。
- 并行：P2-4（提问框）同时在分支 `p2-4-ask` 上做，动协议、工具、核心的会话循环和终端。你不要动 `context.rs` 以外的核心代码。

## 要做的

- 打开会话（建会话、打开已有会话）时，除了项目根目录到工作目录每层的 `AGENTS.md`，再读 `<HOME>/.agents/AGENTS.md`；HOME 照 P2-3 技能的办法从会话环境取，没有 HOME 就不读。
- 拼进系统提示词时全局的排在所有项目 `AGENTS.md` 前面，标明是全局说明和它的路径，写法照项目那段。
- 64 KiB 上限、不是 UTF-8、读不了时的警告，照项目 `AGENTS.md` 的规则；文件不存在不警告。
- 和项目的一样，只在打开会话时读一次（DESIGN 第 17 条）：会话中途改了，下次打开会话才用上；重开时开头变了照 P2-15 的做法记一条新开头。
- `crates/canter-core/README.md` 讲上下文的地方跟着改。

## 怎么算做完

- 有 `~/.agents/AGENTS.md` 时，新会话发给模型的系统提示词里有它，排在项目的 `AGENTS.md` 前面。（主控拟，用户 10-25：「验收照你拟的，两件一起派吧」）
- 验证只做这些：
  - 一条针对的测试，先写、确认因为还不读全局的而失败：临时 HOME 放 `.agents/AGENTS.md`，临时项目放 `AGENTS.md`，建会话后发给模型的系统提示词里两份都有、全局在前。
  - 边角用例：全局文件不存在时不警告、系统提示词里没有那一段；超过 64 KiB 时跳过并警告；会话中途改全局文件，下一次请求的开头不变。
  - 做完跑一次 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`（都带上面的 `CARGO_TARGET_DIR`）。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不读 `~/.codex/AGENTS.md`、`~/.claude/CLAUDE.md` 等别家 agent 的配置；不读服务端进程自己的 HOME。
- 不改协议、服务端、终端；不改 `AGENTS.md` 的大小上限。
- 测试用隔离的 HOME、`CANTER_HOME` 和临时目录；不碰用户的 `~/.agents`、`~/.canter`，不碰用户正在跑的服务端。
- 不调真实模型；不升级依赖，要加新依赖的，停下来问。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）：主控和别的 agent 的进程命令行里都带着项目名和工作目录，一条命令能把它们全杀掉。停自己起的服务端用起的时候记下的 PID。
- 碰到要拿主意的事（包括要动上面范围以外的文件），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不开着命令等，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `p2-19-global-agents` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
