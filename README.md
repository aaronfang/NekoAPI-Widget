# NekoAPI Widget

一个低负荷的 NekoAPI 桌面用量监视器。应用常驻 macOS 菜单栏或 Windows/Linux 系统面板，显示今日消费、余额、Token 用量和平台分布。

## 功能

- NekoAPI 账号登录和 TOTP 二次验证
- access token / refresh token 存入系统钥匙串
- 概览和记录双页面，提供 7 日消费图及最近请求明细
- 点击菜单栏图标显示锚定气泡，失去焦点后自动收起
- 菜单栏实时显示今日 Token、月额度已用金额和使用百分比
- Token 统一使用 `M` / `K` 单位，并提供独立的 7 日 Token 趋势图
- 套餐显示当前服务倍率；Token 趋势同时展示 Input、Output、Cache Creation、Cache Read 和 Cache Hit Rate
- 菜单栏采用紧凑的 `今日 Token · 订阅已用%` 格式
- 显示有效订阅的日、周、月消耗、剩余额度、到期和重置时间
- 每次常规同步仅请求用户资料、用量汇总和有效订阅三个只读接口
- 15、30、60 分钟自动刷新，附带 ±10% 随机抖动
- 手动刷新 30 秒冷却，失败后指数退避，最长两小时
- 隐藏窗口后继续通过菜单栏显示、刷新或退出
- 本地保留上一次展示数据；密码和登录令牌不会进入前端缓存

## 开发

需要 Node.js、Rust 和当前平台的 Tauri 2 系统依赖。

```bash
npm install
npm run tauri dev
```

前端类型检查和生产构建：

```bash
npm run build
npm run tauri build
```

生成 macOS 应用：

```bash
npm run tauri build -- --bundles app
```

### Linux

Linux 默认使用 AppIndicator/StatusNotifier 系统托盘，可安装发布页提供的 `.deb` 或 `.AppImage`。登录令牌保存在 Secret Service；Debian/Ubuntu 需要可用的 `gnome-keyring` 或兼容服务。

XFCE 内建 systray 不显示 AppIndicator 的动态文字，也不向应用提供主点击事件和图标位置。发布页同时提供 `NekoAPI-Widget-v0.4.2-xfce-panel.tar.gz`，它通过 GenMon 显示 `今日 Token · 订阅已用%`，支持单击直接显示/隐藏，并让面板锚定在点击位置下方：

```bash
sudo apt install xfce4-genmon-plugin xdotool
tar -xzf NekoAPI-Widget-v0.4.2-xfce-panel.tar.gz
./linux/install-xfce-panel.sh
```

该脚本只修改当前用户的 XFCE 面板和自启动配置。未启用 XFCE 桥接时，应用继续使用标准 Linux 托盘菜单。

### Windows

Windows 10/11 x64 使用系统 Credential Manager 保存令牌，并通过 NSIS 安装包分发。安装程序会在需要时静默引导安装 Microsoft WebView2 Runtime。

在 Windows 开发机上运行：

```powershell
npm ci
npm run tauri dev
npm run tauri build -- --bundles nsis
```

仓库中的 `Build Windows` 工作流提供手动 NSIS 构建；版本标签由统一的 `Release` 工作流在原生 Windows、macOS 和 Linux runner 上构建并发布。

> Windows 系统托盘不支持 macOS 式的常驻文字标题，因此今日 Token 和订阅百分比显示在托盘提示中；点击托盘图标仍会显示同一套锚定气泡界面。

Windows 气泡会检测任务栏所在边缘：底部任务栏向上展开，顶部向下展开，左右侧任务栏向屏幕内部展开，并始终限制在当前显示器工作区内。macOS 继续使用顶部菜单栏下方展开逻辑。

## 发布包

推送 `v*` 标签后，`Release` 工作流仅在三个系统全部构建成功时创建 GitHub Release：

- Windows x64 NSIS 安装程序（`.exe`）
- macOS Intel/Apple Silicon 通用磁盘映像（`.dmg`）
- Linux x64 Debian 包和 AppImage（`.deb` / `.AppImage`）
- XFCE 面板集成脚本（`.tar.gz`）

当前发布包未进行 Apple Developer ID 或 Windows Authenticode 签名，系统可能显示来源确认提示。

## 请求策略

应用固定连接 `https://999555999.com/api/v1`。正常刷新只读取：

- `GET /user/profile`
- `GET /usage/dashboard/stats`
- `GET /subscriptions/active`

以下接口按需加载，打开对应页面时缓存 15 分钟，不参与后台轮询：

- `GET /usage/dashboard/trend`（概览中的 7 日趋势）
- `GET /usage`（记录页中的最近 18 条请求）

仅在 access token 返回 401 时调用一次 `/auth/refresh`。应用不会抓取网页、模拟点击、绕过 CAPTCHA 或轮询消费明细。

## 数据存储

- 登录令牌：macOS Keychain / Windows Credential Manager / Linux Secret Service
- 面板缓存和刷新间隔：Tauri WebView 的应用专属本地存储
- 密码：只在登录请求期间保存在内存中，不写入磁盘

使用前请确认账号所有者和站点运营方允许第三方客户端读取账号统计数据。
