# 任务：第二期 - 干活那一行换成有趣的词，并显示发出去的 token

2026-11-03，主控建。
执行：主控（用户：「p2-199好做的话，你直接做吧。」）

## 用户原话

- 11-03 发来干活那一行的截图（`⠴ Thinking… · 36s · ↓ 2.92k tokens · Esc to interrupt`）：「出去的token呢（之前就没有还是这次改没了，我理解第一次就应该有很多出去的token）？另外最下面我想模拟claude code，最下面的状态应该是一些有趣的词，而不是thinking...writing...dengdeng」
- 主控说明 ↑ 是 P2-194 时用户选的「干活时只显示输出，结束时全显示」，只在一轮结束那行有。问了两件：
  - 干活那一行要不要也加 ↑：用户选「干活时也显示 ↑」（示意 `⠴ Cantering… · 36s · ↑ 48.1k · ↓ 2.92k tokens · Esc to interrupt`）。
  - 用什么词、怎么换：用户选「混合的有趣词，每轮随机」（示意 `⠴ Noodling… · 36s · ↓ 2.92k tokens · Esc to interrupt`）：像 Claude Code 那样不限主题，混几个马的词；一轮里不换，下一轮再抽；在做什么照旧看上面的卡片，跑命令时上面那行 `Running <命令>` 保留；等批准、等回答、打断中、压缩中这些照旧写实。

## 要做成什么

1. 干活那一行的词（现在的 `Thinking…`、`Writing…`、`Reading…`、`Editing…`、`Calling…`、`Fetching…`、`Searching…`、`Working…`）换成一个有趣的英文词加 `…`。词表自己起，三四十个，不照搬 Claude Code 的词表，混几个马的词（比如 `Cantering…`、`Trotting…`、`Galloping…`）。
2. 每一轮开始时随机抽一个，这一轮里不换；下一轮再抽，不和上一轮相同。
3. 照旧写实、不换成有趣词的：`Running <命令>` 那一行（P2-197）、`Compacting context…`、`⏸ Waiting for approval` / `⏸ Waiting for your answer`、`■ Interrupting…`、`⟳ Reconnecting…`。
4. 颜色、加粗、转圈、扫光照 P2-194 不变。
5. 干活那一行在用时后面写 `· ↑ 48.1k · ↓ 2.92k tokens`：↑ 是这一轮到目前发给模型的 token，和一轮结束那行的 ↑ 同一个数，每次模型请求做完更新；还没用量时两个都不写。数字写法照 P2-194（三位有效数字）。窗口窄时先省 ↑，再照原来的规矩省 ↓。
6. 一轮结束那行（`Worked for … · ↑ … (N% cached) · ↓ … · HH:MM`）不变。

## 怎么算做完

- 用户看到的就是上面示意的样子：干活时是随机的有趣词，同一轮不变；有 ↑ 和 ↓；写实的那几种状态照旧。

## 不要做

- 不加配置项、不做词表设置。
- 不改协议、服务端、模型请求和存盘。

## 完成记录

- 做了什么：新加 `crates/canter-tui/src/verbs.rs`，38 个自己起的词（8 个马的词，其余不限主题，没照搬 Claude Code 的词表），按轮 ID 的 FNV-1a 哈希挑，和上一轮相同就顺延一个；`app.rs` 的 `doing()` 只留命令和压缩两种写实，别的都用这一轮的词（P2-197 命令单独一行时下面那行也是这个词）；干活那一行加 `↑`，窗口窄时先省 ↑ 再省 ↓。DESIGN「显示用时和 token」下补一条。
- 验证：`cargo nextest run -p canter-tui`（659 过，含新加的 `verbs` 单元测试：500 轮里同一轮词不变、相邻两轮不同、分布开）、`cargo nextest run -p canter --test tui`（83 过）、clippy、fmt、`git diff --check`。旧测试照新词改了期望：测试里轮 ID `t1` 抽到 `Galloping…`；shimmer 三个测试因为词长了一个字，扫光落点照新长度改；token 测试加了 ↑ 和「只剩 ↓」那一档；canter 终端测试里等命令在跑的两处改看 `Ctrl+B to run in background`（真服务端的轮 ID 是随机的）。
- 拿主意的地方：随机用轮 ID 的哈希，不另加随机数库，也不存状态：同一轮在重连、多个终端里都是同一个词。
- 没做的事：全套测试（用户：P2-198 做完再跑）。
- 踩过的坑：没有。

## 主控审查

主控自己做（用户：「p2-199好做的话，你直接做吧。」），没有另外的审查者。相关测试全过，没跑全套，用户合并后自己试用。
结果：一次过
