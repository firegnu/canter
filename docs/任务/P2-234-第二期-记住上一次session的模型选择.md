# 任务：第二期 - 记住上一次session的模型选择

2026-11-06，用户在看板上新建；canter/main 补全，交给 canter/dev-last-model（Codex，`gpt-6-astra` / xhigh）。
路由：高 / 交叉审查不要 / 影响面：改行为（路由：重（置信 0.56）、拿不准（并发 0.72、核心规则 0.78）、拿不准；主控定：只在建会话取默认和改会话设置时记一笔，不改会话存盘格式、不碰权限，按改行为算，不交叉审查）。不算 Codex/Kimi 对比样本。
类型：功能变更
依据：新会话现在总用 `config.json` 的 `model`，`/model` 只改当前会话。本件让服务端记住用户最近一次用 `/model`、`/think` 选的模型和思考档位，新会话默认用它。
提示：围绕已确认的使用目标完成变更，优先沿用现有机制。
执行：派出
你是被委派的 agent：照本文件做，不要再开别的 agent。

## 用户原话

- 11-06 用户：「234我碰到的问题是，我在canter中选了opencode go的glm-flash，但是关掉打开之后新的session还是deepseek。我的理解是这一次打开了什么下一次就该是什么。我理解的对吗？」
- 主控答对，查了 Codex `rust-v0.163.0`：`/model` 选完发 `PersistModelSelection { model, effort }`，把模型和档位一起写进 `config.toml` 作为以后的默认（`codex-rs/tui/src/app/event_dispatch.rs`）。
- 11-06 主控问四件，用户都选推荐：
  - 存哪：「服务端单独记」——另存一个小文件，不改 `config.json`；新会话优先用它，模型不在列表里了就退回 `config.json` 的 `model`。
  - 思考档位：「一起记」——记住模型和档位；该模型不支持这个档位时用它的默认档位。
  - `canter -m`：「不算」——`-m` 只管这一次；只有界面里 `/model`（和 `/think`）选的才记。
  - 手改配置：「听后改的」——谁新听谁：手改了 `config.json` 的 `model` 就清掉记住的，用新配的；之后再用 `/model` 选，又按新选的。

## 现状（主控查过的）

- 建会话：`crates/canter-server/src/http.rs` `create`，`params.model` 为空时取 `app.metadata().0`（配置的 `model`，热重载后取新值）；档位为空时取该模型的默认档位（`config.options.reasoning(&model).default`）。`canter -m` 走的是 `params.model`。
- 改会话设置：同文件 `update`（`PATCH /v1/sessions/{id}`），终端的 `/model`、`/think` 走这里改 `model`、`reasoningEffort`；换模型时档位不支持会自动换档（P2-206）。
- `GET /v1/models` 的 `isDefault` 现在按配置的 `model` 标。
- 配置热重载在 `crates/canter-server/src/reload.rs`（P2-154）。状态目录 `~/.canter/` 下已有 `tui.json`、`trusted_folders.json` 等小文件，各自配 `.lock`。
- 用户本机配置 `model` 是 `opencode-go/deepseek-v4.1-flash`。

## 先读

- `docs/DESIGN.md`：第 8 条「可用模型（P2-2）」「几家同时配（P2-200）」「界面里跨家选模型（P2-201）」「模型档位（P2-206）」，以及「改配置不用重启（P2-154）」那段（搜 `model` 改新会话的默认模型）；第 15 条（状态目录）。
- 项目规矩 `CLAUDE.md`。

## 在哪里干活

- worktree：`../canter-worktrees/last-model`，分支 `last-model`（已从 main 建好）。
- 编译目录：命令前加 `CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/last-model`。
- 主要动 `crates/canter-server/`，按需 `crates/canter-core/`、`crates/canter-protocol/`、`crates/canter-tui/`，以及 `docs/DESIGN.md`。

## 要做的

1. **记**：用户用 `/model` 或 `/think` 改了会话的模型或档位后，服务端把这个会话当时的模型和档位记进状态目录里的一个小文件（名字、格式你定，写进 DESIGN 第 15 条）。只记用户的选择：建会话时 `-m` 指定的、子 agent 的模型、换模型时服务端自动换的档（记最终结果即可，不单独算一次）、打开旧会话都不算。多个会话先后改，最后一次为准。
2. **用**：新会话没指定模型时用记住的模型和档位；记住的模型已不在可选列表里，就用 `config.json` 的 `model` 和它的默认档位；记住的档位该模型不支持，就用该模型的默认档位。指定了模型（`-m`）的照旧用指定的。
3. **配置后改的优先**：用户之后改了 `config.json` 的 `model`（不管服务端开着热重载读到，还是重启后读到），就清掉记住的，新会话用新配的；再用 `/model` 选，又按新选的记。
4. **`isDefault`**：`GET /v1/models` 的 `isDefault` 标新会话实际会用的那个模型。
5. **坏了不卡**：记的文件读不出、格式坏了，当作没记过，照常用配置，写一条服务端日志；不影响服务端启动和建会话。写文件要整个替换，不留半截。
6. **DESIGN** 写清：记什么、什么时候记、新会话怎么取、和配置谁优先、和 Codex（写进 `config.toml`）不同的地方和原因（用户不想改手写的配置）。

## 怎么算做完

- 用户要的：这一次在界面里选了什么（模型和档位），下一次新会话就是什么；不改 `config.json`；`canter -m` 不算；手改了 `config.json` 的 `model` 就听新配的。
- 验证只做这些：
  - 每处一条针对它的测试（先确认现在的行为让它失败）：`/model` 换模型后新会话用它；`/think` 换档后新会话用它；`-m` 建的会话不改记住的；子 agent 不改记住的；记住的模型不在列表里退回配置；记住的档位不支持时用默认档；改了配置的 `model` 后清掉记住的（热重载和重启各一条）；文件坏了照常用配置；`isDefault` 跟着变；服务端重启后还记得。
  - 改到的包跑全部测试（`cargo nextest run -p canter-server`，改了别的包一起加），加 `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`git diff --check`。
  - 觉得不够，在回复里说，不要自己加。

## 不要做

- 不改 `config.json`，不改已有会话存盘的设置和格式；打开旧会话照旧用它自己的模型。
- 不按目录分开记（全局一份）；不记 `-m`、子 agent、`canter exec` 带的模型。
- 不碰用户真实的 `~/.canter`：测试用隔离的 `CANTER_HOME` 和 HOME、临时端口和 socket。不调真实的模型接口。
- 界面上的字一律英文。
- 第二期核心不绑 DeepSeek；DeepSeek 这些别家的测试一条都不许改，碰到了停下来问。
- 不要按项目名或路径批量杀进程（`pkill -f canter` 这类）。停自己起的进程用记下的 PID。
- 碰到要拿主意的事（上面没写到的取舍、和现有建会话或热重载冲突、要改协议），停下来，在回复里写清问题、可选办法和你的建议，然后结束这一轮，等主控回复再接着做；不在自己窗口里问用户，不要自己换别的办法。主控的决定写进完成记录。
- 不合并到 main，不推送。只在 `last-model` 上提交。

## 做完

在本文件末尾追加「## 完成记录」（在你的分支里提交）：做了什么、验证了什么、拿主意的地方、没做的事、踩过的坑，各几句话。踩过的坑指试了不行、换了办法的，工具或环境的行为和预想不一样的；没有就写「没有」，不要凑。回复里只写这几样，加上有没有要主控决定的事。命令都在前台跑完，全部做完后，回复最后一行写 DONE。
