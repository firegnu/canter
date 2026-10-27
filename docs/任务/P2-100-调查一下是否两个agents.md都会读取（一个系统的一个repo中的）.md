# 任务：全局 AGENTS.md 改放 canter 自己的目录

2026-10-27，用户在看板上新建（原题「调查一下是否两个agents.md都会读取（一个系统的一个repo中的）」）。
类型：功能变更
依据：主控查了：两份都读，全局读 `~/.agents/AGENTS.md`（P2-19，10-25 主控建议、用户同意），项目从 git 根读到工作目录。`~/.agents/AGENTS.md` 不是行业标准，AGENTS.md 标准只管仓库里那份；各家全局放自己目录：Codex `~/.codex/AGENTS.md`、pi `~/.pi/agent/AGENTS.md`、opencode `~/.config/opencode/AGENTS.md`、Claude Code `~/.claude/CLAUDE.md`。用户要 canter 也有自己的一份。
执行：主控（用户 10-27：「直接做吧，我觉得这个任务不难吧？」）

## 用户原话

- 10-27：「关于P2-100的话，pi现在读的是什么？理论上是不是~/.agents/AGENTS.md是不是要有一份（这是行业标准）。」
- 10-27 主控答 pi 读 `~/.pi/agent/`、`~/.agents/AGENTS.md` 不是标准，建议不改代码、用户自己放文件；用户：「不要，我想的是，我自己要有一份。理解我意思吧？而不是当时定的位置」
- 10-27 主控写好任务文件；用户：「直接做吧，我觉得这个任务不难吧？」

## 先读

- `docs/任务/P2-19-全局AGENTS.md`（全局说明怎么读、排在哪、出错怎么办）。
- `crates/canter-core/src/context.rs` 读全局说明那几行（现在是会话环境 HOME 下的 `.agents/AGENTS.md`）。

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/p2-100-canter-agents`，分支 `p2-100-canter-agents`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-100-canter-agents`。

## 要做的

1. 全局说明改读 canter 自己目录下的 `AGENTS.md`：会话环境里的 `CANTER_HOME`，没设就是 `<HOME>/.canter`（和终端找 `config.json`、状态栏脚本的规矩一样，`crates/canter-tui/src/statusline.rs` 的 `home`）。不再读 `~/.agents/AGENTS.md`。
2. 别的照 P2-19：排在项目的前面，64 KiB 上限、UTF-8、出错跳过并警告，文件不存在不警告，只在打开会话时读。
3. `~/.agents/skills`（全局技能）不动。
4. 改用到旧路径的测试、`crates/canter-core/README.md`；DESIGN 里写明全局说明的位置和理由（各家都放自己目录，`~/.agents/AGENTS.md` 不是标准），提交说明里写。

## 怎么算做完

- （用户 10-27）「我自己要有一份」：放一份 `~/.canter/AGENTS.md`，新会话发给模型的系统提示词里有它，排在项目的前面；`~/.agents/AGENTS.md` 不再读。
- 验证只做这些：改一条已有的全局说明测试，先改测试、确认因为还读旧位置而失败，再改代码；再跑一次项目的标准测试：`cargo nextest run --all-targets`、`cargo test --doc --workspace`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`。觉得不够，在回复里说，不要自己加。

## 不要做

- 不读别家的文件（`~/.codex/AGENTS.md`、`~/.claude/CLAUDE.md` 等），不加 `CLAUDE.md` 后备。
- 不改协议（上下文弹框的路径照原样显示实际读的文件）、存盘格式。
- 测试用隔离的 HOME 和 `CANTER_HOME`、临时端口，不读用户真实的 `~/.canter`，不调真实的模型接口。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用起的时候记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做。
- 不合并到 main，不推送。只在 `p2-100-canter-agents` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
