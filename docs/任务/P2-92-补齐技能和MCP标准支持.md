# 任务：补齐 canter 对技能和 MCP 标准的支持

依据：P2-85（用户在看板上建的「将codex/claude code的mcp和skills尝试同步给canter」）的目的是看 canter 能不能照标准加载技能和 MCP。主控 10-27 核对了代码：都能加载，但离「无缝」还差一些。本件记下结论，等用户处理。
待用户：定怎么做（先调研还是直接分件）
执行：派出

## 用户原话

- 10-27 用户：「接下来的三件事情，85， 90， 91一件件来，咱们先来看85.你觉得可行吗？如果可行的话，值得吗？」
- 10-27 用户：「我的理解是，技能只读~/.agents/skill下的，不读取claude code的，因为现在大多数的agent是统一的，claude code有点特殊。 MCP的话应该读取canter自己的，而不是跑去读codex或者claude code的」
- 10-27 用户：「那么是不是意味着canter的MCP设计还有缺陷。我建立这个任务的目的不是为了加载什么技能或者mcp，而是为了看看canter是否可以加载技能或者mcp（我理解这两个都是标准，应该无缝支持）。」
- 10-27 用户：「上面的结论列一个单独的task，这个task中写清楚你上面的结论。我稍后处理它」

## 已定的

- 技能只读 `~/.agents/skills` 和项目里的 `.agents/skills`，不读 `~/.claude/skills`、`~/.codex/skills`。MCP 只读 canter 自己的 `~/.canter/config.json`，不读 Codex、Claude Code 的配置。canter 现在就是这样（`crates/canter-core/src/skills.rs` 第 15–18 行；DESIGN 第 15 条「MCP 配置（P2-12）」）。这条规矩还没写进 DESIGN，做本件时补上。

## 主控 10-27 核对的结论

快速核对，不全；标「没查」的要调研再定。

### 技能

- 已照标准：从 `.agents/skills` 读，每个技能一个文件夹、一个 `SKILL.md`；读开头的 `name`、`description`（另认 `disable-model-invocation`），系统提示词里只放清单，模型用到时用 `read` 读全文。用户现在的 18 个技能都能正常读。
- **bug**：`SKILL.md` 开头是手写的简化解析（`skills.rs` 的 `read_header`、`scalar`），只认一行的不带引号或双引号文字，标准用 YAML。下面两种写法读错且不报错：
  - 多行写法 `description: >` 或 `description: |`：说明被读成 `>` 或 `|`，后面缩进的行被跳过，模型不知道这个技能做什么。
  - 单引号 `description: '…'`：引号留在说明里。
  - 用户现有技能都没用这两种写法，还没碰上；别人写的技能常这样写。
- 小差别：没写 `name` 的技能被跳过。标准要求必须写，不算错；Claude Code 允许不写（用文件夹名）。

### MCP

现在支持本机起程序（stdio）和网址（Streamable HTTP）两种服务，能调工具、按执行权限批准、有启动和调用超时（P2-12）。缺的，按影响大小：

1. **只收文字结果**：服务返回的图片、内嵌资源等非文字内容一律换成 `[MCP non-text content omitted]`（`crates/canter-tools/src/mcp.rs` 第 500 行附近），比如浏览器类服务的截图模型看不到。canter 本身支持图片输入（P2-5），这里没接上。`structuredContent` 也不读（没查有没有服务只给它不给文字）。
2. **没有登录授权**：标准里要登录的远程服务走 OAuth，canter 没做，只能在配置里写死请求头。
3. **没有资源和提示词模板**：Codex、Claude Code 能读 MCP 资源，canter 没有。P2-12 有意只接工具；P2-88 报告记成以后再说。
4. **工具全量加载**：所有 MCP 工具说明每次请求都带，不违反标准，但工具多了占上下文（比如 gitlab 服务约 40 个工具）。别家用「工具搜索、用到再加载」（P2-88 报告：以后再说）。
5. 其它小的：
   - 旧的 SSE 连法不支持（标准已弃用，不少 Claude Code 配置还在用）；
   - 服务要的 token 不能从环境变量继承（canter 起服务时只留 8 个基本变量），要写进配置；Codex 有 `env_vars` 透传，Claude Code 有 `${VAR}` 展开；
   - 服务主动问用户（elicitation）、sampling、roots 这些客户端能力，没查 canter 支持到哪一步；
   - 工具列表在会话打开时定下、中途不变（为了缓存，DESIGN 第 17 条），服务发的「工具列表变了」通知不管。这是有意的。

## 主控的建议

1. 先派一个调研：对照技能标准、MCP 标准（以当时最新版为准）和 Codex、Claude Code 的做法，把 canter 缺的列全，每条给做 / 不做 / 以后再说和大小。上面的结论作起点。
2. 用户定了再分件做。主控倾向的顺序：先修技能解析的 bug（小），再做 MCP 图片结果和 OAuth 登录，资源和工具搜索往后放。

## 待定

1. 先调研再分件，还是照上面的结论直接分件。
2. 各条做不做、先后。
