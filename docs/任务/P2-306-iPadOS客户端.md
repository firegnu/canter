# 任务：远程 3 · iPadOS 客户端：方案已定，拆成 rein 仓库 P1-1～P1-5（原生 App，App 自己开 SSH 隧道，和 Mac 连 5090 同一条路）

依据：从 P2-300 第 3 条拆出。用户嫌 Mac 重，想拿 iPad + Magic Keyboard 工作，连 5090（或别的机器）上的 canter 服务端。优先级比 canter-desktop 连远程（P2-308）高。
依赖：P2-299（远程连接已齐）
执行：派出
优先：高
说明：11-18 P2-149、P2-305 暂不做，本件提为下一件。大活，拆成 `rein` 仓库的 P1-1～P1-5，一次派一件，由 canter 主控布置和审查；本件是总单。

## 用户原话

- 11-17（主控列 P2-300 剩下的 4 条后）：「这里面的拆出来写成任务。然后我理解接下来就是 推送（纯用canter客户端，不牵涉corral，padock之类的，理解我意思吧？）。嗨哟一个就是ipados的客户端任务，这个比canter-desktop优先级还要高（因为mac太重了，我想拿ipados+magic keyboard来工作）」
- 11-17（cairn 摘录）：「不是，我是想做一个，但是我要确认是不是ipad的客户端的使用方式是不是类似我现在在mac上anter --server 5090这样的技术路线？如果又是另外一种，我就不需要开发了。」主控答是同一条路后：「我就是这个目的。其实这也是当时设计server/client架构的想法」
- 11-18：「先标记149和305暂不做，然后出306方案」；问模拟器：「另外我想现在ipad模拟器上测试，应该没问题吧？」

## 要的结果

- 在 iPad 上，用 Magic Keyboard，像现在 Mac 上的 canter 终端一样干活：开新会话、接回会话、聊天、批准、看输出、贴图。
- 只靠 canter 自己，不经 corral、paddock。

## 方案（11-18 主控出，待用户定）

### 一句话

iPadOS 原生 App（Swift + SwiftUI）。App 自己开 SSH 隧道到 VPS，走和 Mac 上 `canter --server 5090` 一样的路。canter 服务端、协议、VPS 都不用改。

### 怎么连

- 路：iPad App →（App 自己的 SSH 钥匙）SSH 到 VPS → 在这条 SSH 里转到 VPS 回环上 5090 挂的那个端口 → 5090 上的 canter。和 Mac 的 `ssh -L` 是同一条路，只是隧道由 App 自己开，只在 App 在前台时开着。
- 服务端和 VPS 那边只加东西、不改：5090 的反向隧道（P2-302）照旧；VPS 的 `authorized_keys` 加一把 iPad 的公钥；5090 上 `canter remote token add ipad` 发一个令牌。这两样都由用户自己做、自己复制。
- 钥匙：App 里生成 Ed25519，私钥放 iOS 钥匙串（Keychain），把公钥显示给用户复制。VPS 的主机指纹第一次连接时显示给用户确认，记住；以后变了就拒绝连接。令牌也放钥匙串。
- iPad 要能连上 VPS：Mac 现在经代理连 VPS（P2-302 的「断代理」），iPad 上也要有同样的系统级代理或 VPN。
- 不做 HTTPS，不做端到端加密。VPS 上看得到明文，和现在 Mac 一样（P2-305 用户 11-18 定暂不做）。

### App 进后台

- App 进后台后 iPadOS 很快挂起它，SSH 和事件流都会断。服务端照常干活：客户端离开不打断工作、不关会话（P2-172）。回到 App 时重开隧道，按事件编号续接（`Last-Event-ID`），断开期间的事件补上。
- 在后台时收不到任何提醒（P2-149 暂不做）。
- 用普通事件订阅，不用受管终端租约（P2-192 的租约在客户端退出时会关会话）。

### 协议

- 用 Swift 手写用得到的那部分类型（Codable），照 `crates/canter-protocol/generated/protocol.schema.json` 和 `crates/canter-protocol/README.md` 写。认不出的事件和项目跳过，协议本来就给旧客户端留了 Unknown。
- 连接先查 `/v1/info` 的协议版本和 features；不兼容时照 P2-310 的规则提示去服务端机器升级。
- canter 不为 iPad 写特例。主控看过，第一版用到的接口都已有；做的时候发现缺的，在 canter 这边另开任务、写进 DESIGN。

### 第一版拆成 5 件（一次派一件，每件做完都能用）

1. **仓库和协议客户端**：新仓库骨架；连接设置（地址、令牌）；`/v1/info`；会话列表；SSE 收事件和续接。在模拟器里直接连 Mac 本机的 canter（开回环端口、发一个令牌），不经 SSH。测试用本机起的假服务端回放固定事件，不调模型。
2. **对话**：新建会话（工作目录从已有会话的目录里挑，或手打绝对路径）、接回会话、发消息、流式显示回答、思考折叠、工具卡片（命令、改文件的 diff、读文件等，先只显示文字）、排队和插话、打断。
3. **批准和提问**：普通批准、沙箱批准、模型提问、计划清单。
4. **App 内 SSH 隧道**：生成钥匙、核对主机指纹、端口转发、切前后台时重连；装到真机，经 VPS 连 5090。
5. **键盘和贴图**：Magic Keyboard 快捷键（发送、打断、新会话、切会话等）；粘贴、拖入图片（`imageData`）；换模型和思考档位。

第 1～3、5 件在模拟器上测（模拟器跑在 Mac 上，直接连 Mac 本机的 canter，或经 Mac 已有的隧道连 5090）。第 4 件和最后的手感（真 Magic Keyboard、中文输入法、切后台）要上真机。

第一版不做：大部分斜杠命令（`/review`、`/memory`、`/export`、`/doctor`、`/stats` 等）、`@` 文件补全、状态栏脚本、子 agent 界面、ChatGPT 账号登录、iPhone 布局、推送。用起来缺什么再加。

### 仓库和工具

- 新开一个仓库，建议叫 `canter-ios`（以后 iPhone 也放在里面），放在 `~/Developer/personal_projs/canter-ios`。像 canter-desktop 一样有自己的 CLAUDE.md 和任务文件；本件留在 canter 这边当总单。canter 仓库不引用它。
- Swift + SwiftUI。本机已装 Xcode 27.2，模拟器有 iOS 26.5 和 27.0。开发和测试用 `xcodebuild` 加模拟器在命令行跑，派出去的 agent 能自己跑。
- SSH 库在第 4 件开工时比较后定：Apple 维护的 swift-nio-ssh，或基于它的 Citadel。显示 markdown 的库在第 2 件开工时定。只用成熟、活跃维护的库。
- 界面文字用英文，和终端一致。
- DESIGN 第 3 条写的是服务端和终端用 Rust。要补一句「iPad 客户端用 Swift，单独仓库，只用公开协议」，canter 的 CLAUDE.md 语言那条也注明这个例外。由主控在派第 1 件前改。

### 装到真机

- 模拟器免费。真机：免费 Apple ID 就能装，但 7 天过期要重装；每年 99 美元的开发者账号一年有效，还能用 TestFlight。第 4 件之前定就行。

## 用户已定（11-18）

用户原话：「1. 同意 2. 名字能否换一个单单词的 ，私有 3. 我的ipad的系统版本是最新的 可以连接到vps。4. 先用免费的」；名字从主控列的几个里选了 `rein`（缰绳）。

1. 第一版按上面 5 件做。
2. 仓库叫 `rein`，放 `~/Developer/personal_projs/rein`，GitHub 私有仓库 `firegnu/rein`。上面「仓库和工具」里的 `canter-ios` 一律按 `rein` 读。
3. iPad 是最新系统，能连上 VPS。App 最低支持 iPadOS 27（本机模拟器有 27.0）。
4. 苹果账号先用免费的（真机 7 天重装一次）。
5. 通知先不做（「先不做通知，先集中精力把ios/ipados的客户端做好」）。

## 不要做

- 不改「一台服务端只服务一个人」。
- 不写 canter 的特例给某个客户端；客户端只用公开协议，缺的接口在 canter 这边另开任务、写进 DESIGN。
- 不和任何一家模型绑定：模型、档位列表都从服务端取。
- 不改 VPS 的 sshd 设置；`authorized_keys` 和令牌由用户自己加。
- 凭证（SSH 私钥、令牌）只在 iPad 钥匙串里，不进仓库、日志、测试快照和提交。
- 文档和测试里不写真实的 IP、主机名、用户名。
