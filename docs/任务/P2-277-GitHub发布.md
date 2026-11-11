# 任务：GitHub Release 发布（占号）

依据：11-11 用户想做 GitHub 的发布，主控先探索，用户定「先弄成一个任务，之后完全稳定了再发release」。DESIGN 第 181 行 P2-14 当时定的是「不发 GitHub release」，开工前先改那一条。
依赖：无
优先：低
待用户：canter 稳定后决定开工；开工前定下面「要用户定的」几点

## 用户原话

- 11-11：「我有一个想做的，就是github的发布。这个你能先探索一下吗？」
- 11-11（主控说发布版的人不能直接用 `canter upgrade`）：「所以说现在麻烦点在 canter upgrade?」
- 11-11：「采用安装包方式安装的话，不会涉及到覆盖之类的错误吧？」
- 11-11：「这个也先弄成一个任务吧，之后完全稳定了再发release」

## 探索结果（11-11）

- **现状**：仓库公开（Apache-2.0，有 `LICENSE`、`NOTICE`），没有 `.github/`、没有 CI、没有标签和 release。版本一直是 `0.1.0`，靠提交号区分（`canter 0.1.0 · <提交>`）。安装：`cargo install --locked --path crates/canter`；升级：`git pull` 后 `canter upgrade`（P2-14）。Linux `x86_64-unknown-linux-musl` 静态版已能在 Docker 里编，验证过 Debian 11、Ubuntu 20.04、Alpine（P2-160）。
- **编进程序的源码路径**：`crates/canter/build.rs` 把编译时的仓库路径编成 `CANTER_SOURCE_DIR`，`canter upgrade` 不带参数时用它（`crates/canter/src/deployment.rs` 的 `build()`）。
  - 本机编好直接发布，会把 `/Users/<用户名>/...` 带出去，所以发布版要在 GitHub Actions 上编。
  - GitHub 上编的程序直接 `canter upgrade` 会报 `cannot open source directory …; pass the canter repository to canter upgrade`，程序和服务端都不动；有仓库和 Rust 工具链时 `canter upgrade <仓库路径>` 照常能用。所以发布不用改 canter。
- **工具**：dist（原 cargo-dist，`axodotdev/cargo-dist`，11-02 发 v0.34.0，仍活跃）生成 GitHub Actions 发布流程、压缩包、校验和、`curl … | sh` 安装脚本，可选 Homebrew。release-plz（11-02 发 v0.4.3）可自动升版本、写更新日志。Codex 是自写流程，macOS 签名公证，Linux 用 Sigstore。公开仓库用 Actions 免费，有 macOS 机器。依赖里有 `aws-lc-sys`（C 代码，GitHub 机器自带编译工具）。
- **用安装脚本装会怎样**（看了 dist v0.34.0 的 `installer.sh.j2`）：
  - 先放到安装目录下的临时目录，再 `mv` 原子替换，正在跑的服务端不受影响，不会「文件正忙」或 macOS 原地覆盖被杀。
  - 默认装进 `~/.cargo/bin`，和 `cargo install` 同一处，会替换源码编的那份。
  - 不换服务端：装完要 `canter serve --replace`（最多等轮 60 秒，超过打断）；`canter upgrade` 是一直等跑完。
  - 不备份：`canter rollback` 只认 `upgrade` 的备份，退回只能装旧版本的包。

## 要用户定的（开工前）

1. 发给谁：只给自己几台机器，还是公开给别人装。公开就要写安装说明，并在压缩包里附第三方依赖的许可证文本（Apache-2.0、MIT 的要求）。
2. 平台：主控建议先发 macOS Apple 芯片（`aarch64-apple-darwin`）和 Linux x86_64 静态版（`x86_64-unknown-linux-musl`），都验证过；Intel Mac、Linux ARM 以后再加。不做 Windows。
3. macOS 签名公证：要 Apple 开发者账号（$99/年）。不签时，`curl` 安装脚本下载的能直接跑，浏览器下载的会被系统拦下，要手动放行。
4. `canter upgrade`：保持从源码编（用户自己的用法不变），还是加从 Release 下载。加的话要改 DESIGN，等轮、备份、rollback 沿用 P2-14。
5. 版本号：发布时手动升（比如 `0.2.0`，打 `v0.2.0` 标签），还是用 release-plz 自动升版本、写更新日志。

主控建议（11-11）：先做最小版——GitHub Actions + dist，只发上面两个平台，带校验和和 `curl` 安装脚本，不签名，版本号手动升，`canter upgrade` 不动；发布说明写清装完运行 `canter serve --replace`、退回装旧版本。不改 canter 代码，只加发布流程和 DESIGN 一段。

## 不要做

- canter 稳定前不发 release、不打标签。
- 不在本机编好的程序上传发布（会带出本机路径）。
- 没有用户同意不买 Apple 开发者账号、不加需要密钥的发布渠道（Homebrew tap 要单独的令牌）。
