# canter

canter 是用户自己做的 coding agent：Rust 写，一台机器一个常驻服务端，多个客户端（先做终端界面，以后有 GPUI 桌面应用和网页）。它独立于 ranch、corral、paddock、cairn，在任何终端里都能开。

设计和当前决定以 `docs/DESIGN.md` 为准。实现中要改设计，先改那份文档，并在提交说明里写清楚。

## 先读

- `docs/DESIGN.md`：用户已定的事和原话、和调研报告不同的地方、还没定的。
- `docs/调研/R49-自研agent调研.md`：核心理念、组成、协议、模型接入、分期。和 DESIGN 冲突时以 DESIGN 为准。
- `HANDOFF.md`（有了以后）：照着执行的事实：装的哪一版、怎么退回、机器和环境的长期事实、还没派的草稿、已知没修的问题。会话经过、待定的事、约定和经验由 cairn 开局注入。

## 规矩

- **语言与依赖**：Rust stable；只用成熟、活跃维护的库，依赖版本由 canter 自己决定，升级作为单独任务。实现、测试和辅助工具不用 Python，工具用 Rust 或纯数据文件。终端界面可以用 ratatui、crossterm（ranch 不许用的那条只管 ranch）。
- **独立**：不在 Cargo 里引用 ranch、paddock、cairn 的包，不读它们的状态目录（`~/.corral`、`~/.pony` 等），代码和文档里不为 corral 写特例。以后接 corral，是 corral 去适配 canter 的公开接口（DESIGN 第 1、12 条）。
- **协议**：服务端和客户端之间用 canter 自定的 HTTP + SSE 协议，结构照 Codex app-server v2（DESIGN 第 8 条）。和 Codex 结构不一样的地方（加了什么、改了什么）写进 DESIGN，说明为什么。
- **从别的项目搬来的代码**（比如 Codex 的 `apply-patch`）：文件开头注明来源（项目、版本标签、原文件），保留原许可证声明；Apache-2.0 的代码按要求维护 `NOTICE`。只参考、不搬的，在任务文件里写参考了哪里。
- **账号和花钱**：
  - 凭证（ChatGPT 登录得到的令牌、DeepSeek 的 API key）只放本机，不进仓库，不出现在日志、测试快照和提交里。
  - 测试不调真实的模型接口：用本机起的假服务端回放固定的响应。真调 ChatGPT Pro（占用户的套餐额度）或 DeepSeek（花钱）只在用户在场、同意时做，任务文件里写明调了几次。
  - 不用 Claude 订阅（用户 10-23）。
- **不改别的仓库**：不改 ranch、paddock、cairn；读别人的源码（Codex、pi、omp、opencode）放 `/tmp` 下的克隆，只读。要别的仓库配合的，写成需求交用户。
- **不要干扰用户正在用的 agent**：`corral ls` 里现有的 agent 都是用户的。可以 `corral ls/status/reply` 读；不要对它们 `stop`、`send`、`keys`、`upgrade`，也不要 attach 上去打字。需要真实 agent 实测时，自己开一个 `canter/test-<名字>`（`--label role=test`），用完 `corral stop`。
- **不按名字批量杀进程**：不要用 `pkill -f canter` 这类命令。停自己起的进程（包括测试里起的服务端）用记下的 PID。
- **测试不依赖用户状态**：用隔离的 HOME 和 canter 自己的状态目录、临时端口和临时 socket，不碰用户真实的会话和配置。
- **验证**：小改跑直接相关测试；跨模块改动和合并前跑 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`。保留有价值的测试，不靠删测试、放宽断言或缩短超时来通过。
- **独立编译目录**：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/<子目录>`，主仓库用 `main`，任务 worktree 用分支名。同一个仓库的两份代码（比如审查时另拷一份）不共用一个编译目录。清 worktree 时一并删掉它的子目录。
- **安装和部署**：还没定，定了写进 DESIGN。切换用户 PATH 上的程序、装成系统服务、重启常驻服务端，都在用户在场时做。

## 开发方式（主控分派）

- 开发任务由主控（`canter/main`）拆开，派给别的 agent 做。主控负责拆任务、写任务文件、审查、合并，不自己写功能代码。分派时按 corral-dispatch 技能做。
- 被委派的 agent（任务文件里写明了身份）照任务文件做，不再往下派。
- 需求单只写用户要的结果，写好给用户看过再派；验收照抄用户原话，不补验收点。主控觉得该加的，列出来问用户。
- agent 名字以 `canter/dev-` 开头；任务文件放 `docs/任务/`；每个任务一个分支，worktree 放 `../canter-worktrees/<分支>`，交叉审查用 detached worktree `../canter-worktrees/review-<分支>`。
- 任务队列（照 paddock 的看板约定，看板从文件、提交、worktree 和 agent 状态推出每件活在哪一步，不另建状态或队列文件）：
  - 一件活一份任务文件，文件名以编号开头：`P<期>-<件>-<标题>.md`，比如 `P1-2-模型接入.md`；交叉审查用同一个编号，`P1-1-交叉审查.md`。
  - 首行 `# 任务：<标题>`；第一个 `##` 之前按需写 `依据：`、`依赖：<编号>`、`执行：派出 / 主控`、`待用户：<什么事>`（等用户时加，处理完删掉）、`优先：高 / 低`。
  - 有 `## 用户原话` 一节；`## 在哪里干活` 第一行写 ``- worktree：`<路径>`，分支 `<分支>`（已从 main 建好）。``
  - 写好就提交（按路径 `git add`），提交了、还没开 worktree 的就是排队中。合并提交首行写「合并 <编号>：」加一句话。
- 做的人合并前自查，在任务文件末尾写「完成记录」：做了什么、验证了什么、拿主意的地方、没做的事。
- 审查：主控审查每个任务。
- 合并：审查通过后本地合并进 main，再推送到 `origin`（`github.com/firegnu/canter`，public，Apache-2.0）。推送前用 gitleaks、trufflehog 查，确认没有密钥、令牌、截图和私人数据。
- 收尾记号：一件活合并完、worktree、分支和编译子目录清干净之后，在 main 上补一条空提交（`git commit --allow-empty`），首行写「收尾: <编号> 」加一句话说明这件活是什么。只记真正落地的活；说好不合并、停在审查的不记；决定不做的写「收尾: <编号> 不做：<原因>」。
- 开出来的 agent：清掉某个 worktree 时，把住在里面的那个一并关掉（它的工作目录没了，接不了新活）；其余的用户说关才关。
- 主控只在这些变了时更新 `HANDOFF.md`：装的版本和退回办法、机器和环境的长期事实、还没派的草稿、已知没修的问题。不写会话经过、进度流水、待定的事、约定和经验，这些由 cairn 记。设计和理由进 `docs/DESIGN.md`，每件活的经过进 `docs/任务/`。
