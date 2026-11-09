# 任务：读文件工具收下 limit 和 offset

执行：派出
类型：功能变更
依据：模型调读文件工具时常按别家的习惯写 `limit`、`offset`，canter 不认，整次调用报错，模型下一步再改成 `lineCount`、`startLine` 重读。改 canter 内置的读文件工具，所有仓库、所有模型的会话都受益。

## 用户原话

- 11-09（看到 eval 会话里 `Read src/cli.ts · failed`）：「我发现委派出去的agent这种错误有点多。虽然最后也能纠正过来。只调查给结论即可」
- 主控查明是 `{"path": "src/cli.ts", "limit": 40}` 被拒之后：「读文件手下limit，offset对所有的repo都起作用吗？我不想只针对某一个repo修复」
- 「先写任务单吧，我觉得是不是调查完再写？」

## 背景（主控 11-09 查的）

- 读文件的参数在 `crates/canter-tools/src/lib.rs` 的 `ReadArgs`（`deny_unknown_fields`），只认 `path`、`page`、`startLine`、`lineCount`；多一个字段就退回 `Invalid arguments: unknown field ...`。所有会话都用这一个工具，和仓库、模型无关。
- 会话记录里的次数：日常 `~/.canter` 304 个会话，读文件因 `limit` / `offset` 被拒 26 次；eval 64 个会话 3 次。报错来自好几家模型的调用（调用 ID 格式不同），不是哪一个模型独有。
- 模型实际写法：只写 `limit`；`offset` + `limit`；`startLine` + `limit`；`offset` + `lineCount`。另有 5 次把数字写成字符串（`"offset":"80"`）。
- 别家怎么数：pi 的 `offset` 是「从 1 数的行号」，opencode 一样（`offset || 1`），两家传 0 都当从头读；Claude Code 的 Read 也是从 1 数的行号。Codex 现在没有读文件工具（只用 shell），omp 用自己的选择器写法，都不参照。`limit` 各家都是最多读几行，和 canter 的 `lineCount` 一样。

## 先读

- `crates/canter-tools/src/lib.rs`：`ReadArgs`、`Tool::Read` 的校验（约 1358 行）、读文件失败时生成 `Item::FileRead` 的地方（约 2368 行，从原始参数取 `startLine`）
- `crates/canter-tools/tests/files.rs` 里 `read_` 开头的测试

## 在哪里干活

- worktree：`/Users/firegnu/Developer/personal_projs/canter-worktrees/read-aliases`，分支 `read-aliases`（已从 main 建好）。
- 编译目录：`CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/read-aliases`。

## 要做的

- 读文件工具收下 `offset` 当 `startLine`、`limit` 当 `lineCount`，读出来的结果和直接写 `startLine`、`lineCount` 一样。四种写法（只 `limit`、`offset` + `limit`、`startLine` + `limit`、`offset` + `lineCount`）都能读。
- 读出来的条目（界面上显示的起始行）按换算后的起始行。

下面几条主控拿不准，等用户定（定了改写成要做的）：

1. `offset: 0`：pi、opencode 都当从头读。主控建议同样当 1；`startLine: 0` 照旧报错。
2. 同一个意思写了两遍（`startLine` 和 `offset` 都有，或 `lineCount` 和 `limit` 都有）：主控建议报错，说明只能写一个；目前记录里没出现过。
3. 数字写成字符串（`"offset":"80"`，5 次）：改完以后这几次会换成「类型不对」的报错，模型照样能自己改。主控建议这次不收，要收的话 `startLine`、`lineCount` 也得一起收，范围就大了。
4. 工具说明和参数定义（发给模型的那份）不改，不对模型提 `limit`、`offset`，只在收到时照收。主控建议这样，系统提示词和缓存都不变。

## 怎么算做完

- 验收（主控提议，待用户确认）：用户日常用一段时间，会话里不再出现读文件因 `limit` / `offset` 被拒；主控事后可以用同样的办法再数一次。
- 验证只做这些：
  - `crates/canter-tools/tests/files.rs` 加测试：上面四种写法各读一次，结果和对应的 `startLine` / `lineCount` 写法逐字一样；用户定下的几条各有一个例子；
  - 原有读文件测试、工具说明快照（`tests/data/descriptions/read.txt`）不变；
  - `cargo nextest run -p canter-tools`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。

## 不要做

- 不改别的工具的参数（`edit` 缺 `oldText` 之类的另说）。
- 不按模型、按服务商分支：所有模型一样收。
- 不改工具说明和参数定义，除非用户在上面第 4 条定了要改。
- 不调真实模型接口，不读、不改 `~/.canter`、`~/.canter-eval`。
- 不用 Python。不按名字批量杀进程。
- 不合并到 main，不推送。只在 `read-aliases` 分支上提交。

## 做完

在本文件末尾追加「## 完成记录」：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑（没有就写「没有」）。
