# mctl

一个跑在终端里的 Minecraft 工具：本地实例管理、Java 国际服列表与进服、
资源下载、插件系统，以及一套可脚本化的层级 CLI。

> **alpha**：`0.1.0-alpha.1`。核心流程可用，接口和界面仍可能调整。
> 目前只在 Linux 上实测；Windows / macOS 未验证。
>
> **关于作者**：本人是高三生，时间有限；项目主要由 AI 生成，可能有不少问题，欢迎多提 Issue。

---

## 中文

### 能做什么

- **TUI + CLI**：不带参数进入终端界面；带参数走 `mctl tree` 里的层级命令。
- **本地实例 / 世界**：扫描已安装版本，管理存档，启动游戏；支持 Java 8 / 17 / 21
  自动选择。
- **Java 国际服**：服务器页用浏览器式标签分成「用户服务器 / Java 国际服」。
  状态由原版 Server List Ping 直连获取（版本、在线人数、延迟、favicon）；
  选中条目后用本地实例 `--server/--port` 进服。
- **用户服务器双向同步**：在服务器页按 `1` 查看当前实例的
  `servers.dat`，按 `a` 添加、`x` 删除。mctl 写的就是 Java 客户端用的那份
  文件；Java 里加的服务器，回到 mctl 按 `r` 也能看到。按 `2` 回到
  「Java 国际服」全部列表。
- **资源下载**：网易资源中心、Modrinth、Mojang 三个来源。
- **账号**：4399 登录（网易资源）与 LittleSkin / 离线名（本地与 Java 服务器）。
- **插件**：从本地路径、文件 URL 或 Git 仓库安装；插件可提供资源来源和版本。
- **终端体验**：可停靠窗口、39 套内置主题、鼠标/键盘操作、按世界自动截图。

### alpha 的边界

- **没有 Microsoft 登录**：正版验证服需要正版认证，alpha 只支持 LittleSkin
  与离线名；进这种服会提示认证失败。
- 4399 登录只用于访问网易资源；本地游戏与 Java 服务器不使用它。
- 插件 API 仍在演进，暂不保证跨版本兼容。
- 不包含官方客户端注入、账号池或任何绕过正版验证的功能。

### 构建

需要 Rust（建议最新 stable）和 Java（启动游戏时使用）：

```bash
git clone https://github.com/yangstafiltra/mctl.git
cd mctl
cargo install --path ./ --force 
```

升级时重跑同一条命令并加 `--force`。`cargo install` 只编译并安装可执行文件，
游戏目录、配置和下载仍在你自己的 `~/.config/mctl/` 下。

### 上手

```bash
mctl                         # TUI
mctl install 1.21.1          # 安装版本到默认游戏目录
mctl local list              # 看本地实例
mctl server list             # Java 服务器 + 实时状态
mctl server add mc.example.net:25565 我的服
mctl tree                    # 完整命令树
```

配置、缓存和下载默认在 `~/.config/mctl/`。设置页可改游戏目录和下载目录。
服务器页用方向键选卡片，回车用当前本地实例进服；`r` 重新 ping。
服务器页内按 `1` 切到用户服务器、`2` 回到 Java 国际服；
标签也可以直接用鼠标点。焦点在横幅时 `←/→` 循环标签、`↑/↓` 进入卡片网格；
进入网格后方向键按卡片行列移动。

### 资源

- TUI 资源页顶部可切换来源：网易 / Mojang / Modrinth。


### 许可证

Apache License 2.0，见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。
`third_party/` 下的两个编译期依赖保持 MIT，见
[third_party/README.md](third_party/README.md)。

仓库不含 Minecraft 客户端、资源文件、Java runtime 或任何官方二进制；
这些内容在运行时从官方源获取。

---

## English

A terminal-first Minecraft toolkit: local instance and world management,
a Java server browser with real-time status, resource downloads, plugins,
and a scriptable hierarchical CLI.

> **alpha**: `0.1.0-alpha.1`. Core flows work, but interfaces may change.
> Tested on Linux only; Windows and macOS are unverified.
>
> **About the author**: I'm a high-school senior with limited time. This project is largely AI-generated and may contain many issues; please open an issue when you find one.

### Features

- **TUI + CLI**: run without arguments for the terminal UI, or use the
  hierarchical commands shown by `mctl tree`.
- **Local instances / worlds**: scan installed versions, manage saves, and
  launch the game with automatic Java 8 / 17 / 21 selection.
- **Java servers**: the server page uses browser-style tabs for user servers
  and the Java server list. Status, player counts, latency, and favicons come
  from the vanilla Server List Ping protocol; pressing Enter joins through a
  local instance with `--server/--port`.
- **Two-way user-server sync**: press `1` to show the current instance's
  `servers.dat`. Press `a` to add and `x` to remove; mctl writes the same file
  the Java client uses, so servers added in Java appear after pressing `r`,
  and servers added in mctl appear in Java. Press `2` to return to the Java
  server list.
- **Resources**: NetEase resource center, Modrinth, and Mojang.
- **Accounts**: 4399 login for NetEase resources; LittleSkin or offline names
  for local play and Java servers.
- **Plugins**: install from a local path, file URL, or Git repository; plugins
  can provide resource sources and versions.
- **Terminal UX**: dockable windows, 39 built-in themes, mouse and keyboard
  support, and per-world automatic screenshots.

### Alpha limitations

- **No Microsoft login**: verified/premium servers require Mojang authentication,
  which alpha does not implement. LittleSkin and offline names are supported.
- 4399 login is only used to access NetEase resources.
- The plugin API is still evolving and is not yet stable across versions.
- No client injection, account pools, or DRM bypass.

### Build

Rust (latest stable recommended) and Java are required:

```bash
git clone https://github.com/yangstafiltra/mctl.git
cd mctl
cargo install --path ./ --force 
```

Re-run the same command with `--force` to upgrade. `cargo install` only builds
and installs the executable; configuration, game data, and downloads stay under
your own `~/.config/mctl/`.

### Quick start

```bash
mctl                         # TUI
mctl install 1.21.1          # install a version
mctl local list              # list local instances
mctl server list             # Java servers with live status
mctl server add mc.example.net:25565 "My server"
mctl tree                    # full command tree
```

Configuration, caches, and downloads live under `~/.config/mctl/` by default.
The settings page can change the game and download directories. In the server
view, use the arrow keys to select a card and Enter to join with the current
local instance; press `r` to re-ping. Press `1` for user servers and `2` for
the Java server list; tabs are also clickable. With the banner focused,
`Left/Right` cycles tabs and `Up/Down` enters the card grid; inside the grid
the arrow keys move by card row/column.

### Resources

- The TUI resource view can switch between NetEase, Mojang, and Modrinth.


### License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
The two vendored compile-time crates under `third_party/` remain MIT; see
[third_party/README.md](third_party/README.md).

This repository does not bundle Minecraft clients, assets, Java runtimes, or
any official binaries. Those are fetched from official sources at runtime.
