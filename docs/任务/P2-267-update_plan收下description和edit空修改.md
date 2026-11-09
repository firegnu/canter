# 任务：update_plan 收下 description，edit 的空修改不再误报

2026-11-09，canter/main 交给 canter/dev-plan-edit-noop（canter，`canter --yolo`，用 canter 服务端配置的模型）。
路由：常规 / 交叉审查不要 / 影响面：改行为（路由：常规（置信 0.94）、拿不准、拿不准）
执行：派出
类型：功能变更
依据：P2-266 调研的建议 1、2。和 P2-265 同一类：模型按别家习惯写参数，或用「原文和新文一样」的修改当锚点，canter 报错或误报，模型多来回一次。只改 canter 内置的 `update_plan` 和 `edit` 两个工具，所有仓库、所有模型一样。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-09（P2-266 调研后，主控建议先做 P2-258）：「我想先把266做了，这样的话，我做258就没有这么多错了」
- 主控问三条建议做哪些、建议 1、2 合成一件、3 单独设计：用户同意，另问第 3 条为什么要交叉审查（第 3 条是 P2-268）。

## 背景（P2-266，日常 304 个会话）

- `update_plan`：5 次每一步写成 `{"description", "status"}`，canter 要 `{"step", "status"}`，退回 `Invalid arguments: missing field step`，模型再整份重发（另 219 次写对了）。
- `edit`：42 次调用里夹着一条 `oldText` 和 `newText` 一字不差的修改，后面跟真正的修改，看样子是当定位锚点。其中 11 次锚点和真修改叠在同一段，被报 `edit N overlaps edit M`。另有整次调用全是这种空修改的，canter 照样回 `Updated …: 0 lines added, 0 lines removed.`，看起来像改成功了。

## 先读

- `crates/canter-tools/src/lib.rs`：`UpdatePlanArgs` 和 `deserialize_plan`（约 436～470 行）；`Tool::Edit` 的校验和替换（约 1405～1455 行）
- `crates/canter-tools/tests/plan.rs`、`crates/canter-tools/tests/files.rs` 里 `edit_` 开头的测试
- `docs/任务/P2-266-工具调用报错调研.md`

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/plan-edit-noop`，分支 `plan-edit-noop`（已从 main 建好）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/plan-edit-noop`。

## 要做的

### 1. `update_plan` 收下 `description`

- 每一步的 `description` 当 `step` 收下，结果和写 `step` 一样。
- 同一步 `step` 和 `description` 都写了：报错，说明只能写一个（英文）。
- 发给模型的工具说明和参数定义不改，不提 `description`（同 P2-265）。

### 2. `edit` 的空修改

- `oldText` 和 `newText` 一样的那条：照旧要在文件里找得到、只出现一次（找不到、多处照旧报错），但不参与重叠检查，也不替换。这样「锚点 + 真修改」叠在一起时，只按真修改执行。
- 一次调用里所有修改都是空修改：不写文件，按失败返回，说明这次什么都没改（英文，例如 `nothing changed: every edit has identical oldText and newText`），不再回 `Updated … 0 lines added, 0 lines removed`。
- 至少有一条真修改时，结果和只写那几条真修改一样。

## 怎么算做完

- 验收（主控提议，用户 11-09 说派）：用户日常用一段时间，会话里不再出现 `update_plan` 因 `description` 被拒、`edit` 因空修改叠在一起被报 overlaps；整次空修改不再回「Updated」。主控事后照 P2-266 的办法再数一次。
- 验证只做这些：
  - `tests/plan.rs`：用 `description` 写的计划和用 `step` 写的结果一样；两个都写报错。
  - `tests/files.rs`：「锚点 + 和它重叠的真修改」能改成功，结果和只写真修改一样；空修改的原文找不到、出现多处照旧报错；整次都是空修改时失败、文件没动、返回说明什么都没改。
  - 原有的 `update_plan`、`edit` 测试和工具说明快照不变。
  - `cargo nextest run -p canter-tools`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

  觉得不够，在回复里说，不要自己加。

## 不要做

- 不改别的工具（`read` 已由 P2-265 做完；只读 shell 是 P2-268）。
- 不收 `update_plan` 别的字段名（比如 `content`、`title`）：记录里只有 `description`。
- 不按模型、按服务商分支：所有模型一样处理。
- 不改工具说明和参数定义。
- 不调真实模型接口，不读、不改 `~/.canter`、`~/.canter-eval`。
- 不用 Python。不要按项目名或路径批量杀进程（`pkill -f canter` 这类）；停自己起的进程用记下的 PID。
- 碰到要拿主意的事，停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。
- 不合并到 main，不推送。只在 `plan-edit-noop` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。

## 完成记录

- 做了什么：`update_plan` 的每一步现在收 `description`，当 `step` 用；同一步两个都写报错 `update_plan steps accept step or description, not both`。`edit` 里 `oldText` 和 `newText` 一样的条目照旧要求找得到且只出现一次，但不参与重叠检查、不替换；一次调用全是这种条目时按失败返回 `nothing changed: every edit has identical oldText and newText`，不写文件。工具说明和参数定义没动。
- 验证了什么：先加两个测试并确认按预期变红（`missing field step`、`edit 1 overlaps edit 2`），再实现转绿。`cargo nextest run -p canter-tools` 243 个测试通过（22 个跳过），`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check` 都干净。
- 拿主意的地方：Step 结构体没加 `deny_unknown_fields`，保持原来容忍多余字段的行为，只特判 `step`/`description`。两个都不写时的报错写成 `update_plan steps require step or description`（任务文件没规定这条的措辞）。空 `oldText`（哪怕 `newText` 也一样）仍走原有的 `oldText must not be empty`。锚点先做查找和唯一性检查，再跳过。
- 没做的事：没跑全套测试（按任务文件只跑相关的那几条命令）；没改工具说明、参数定义和其他工具；没合并、没推送。
- 踩过的坑：`deserialize_plan` 改成逐条返回 `Result` 后，先把 `collect` 的返回值套进了外层的 `Ok(...)`，编译不过，去掉外层 `Ok` 就对了。新测试第一版在空修改用例前没把文件内容从 `ALPHA beta gamma` 写回 `alpha beta gamma`，锚点找不到，是测试自己的问题，补了一行写回。

## 主控审查

- diff 只动 `crates/canter-tools`（`lib.rs`、`tests/plan.rs`、`tests/files.rs`）和本文件；工具说明、参数定义、快照没动，「不要做」一条没碰。`update_plan` 两种写法、两个都写报错；`edit` 空修改先查找和唯一性再跳过、全空时失败不写文件，都有测试，测试先红后绿。
- 合并前全套（主控，兼审查重跑）：`cargo nextest run --all-targets` 2072 个全过（50 跳过），`cargo test --doc --workspace` 没有失败。
- 对方拿的主意（`Step` 不加 `deny_unknown_fields`、两个都不写的报错措辞、空 `oldText` 先走原有报错）都同意。DESIGN 补了一句（和 P2-265 放在一起）。
- 派 canter 做：一次做完，约 5 分钟，没有必须改。

结果：一次过
