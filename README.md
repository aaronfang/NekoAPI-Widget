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
- 菜单栏采用紧凑的 `今日 Token · 最高额度占用率` 格式；多订阅、多周期时显示最接近耗尽的一项
- 菜单栏图标使用最近使用的有效 API Key 所属分组对应渠道的状态灯：绿色正常、黄色性能下降、红色故障、灰色未知
- 显示有效订阅的日、周、月消耗、剩余额度、到期和重置时间
- 显示 NekoAPI 网页渠道监控中的状态、探测延迟、Ping 延迟和 7 日可用率
- 显示幸运抽奖的期号、截止时间、参与人数和参与状态；站长开启新一期时发送系统通知，并在状态灯右上角叠加琥珀色标记
- 支持手动参与当前抽奖；可开启自动参与，检测到可参与后随机延迟 1–15 分钟再提交
- 每次常规同步请求用户资料、用量汇总、有效订阅、API Key、可用分组和网页渠道监控六个只读接口
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

### macOS

首次启动会弹出钥匙串授权提示，因为登录令牌保存在 Keychain。开发构建每次重新编译都会改变 adhoc 签名，系统可能反复询问，点击「始终允许」即可。

生成应用包，或生成发布页使用的通用磁盘映像：

```bash
npm run tauri build -- --bundles app
rustup target add x86_64-apple-darwin   # 首次构建通用包时需要
npm run tauri build -- --bundles dmg --target universal-apple-darwin
```

首次投递系统通知时 macOS 会请求通知权限，这次授权请求会占用掉第一条通知，授权通过后新一期抽奖才会真正弹出横幅。开发模式下 `tauri-plugin-notification` 会把通知归属到 `com.apple.Terminal`，而 Terminal 通常未开启通知，因此**通知只能在打包后的应用中验证**。

### Linux

Linux 默认使用 AppIndicator/StatusNotifier 系统托盘，可安装发布页提供的 `.deb` 或 `.AppImage`。登录令牌保存在 Secret Service；Debian/Ubuntu 需要可用的 `gnome-keyring` 或兼容服务。

XFCE 内建 systray 不显示 AppIndicator 的动态文字，也不向应用提供主点击事件和图标位置。发布页同时提供 `NekoAPI-Widget-v<标签>-xfce-panel.tar.gz`（随包含 Linux 构建的版本发布），它通过 GenMon 显示 `今日 Token · 订阅已用%`，支持单击直接显示/隐藏，并让面板锚定在点击位置下方：

```bash
sudo apt install xfce4-genmon-plugin xdotool
tar -xzf NekoAPI-Widget-v0.5.3-xfce-panel.tar.gz   # 替换为发布页上的实际标签
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

> Windows 系统托盘不支持 macOS 式的常驻文字标题，因此今日 Token 和订阅百分比显示在托盘提示中；点击托盘图标仍会显示同一套锚定气泡界面。抽奖进行中时，托盘状态灯图标右上角会叠加一个琥珀色标记，托盘提示的首行也会显示「幸运抽奖进行中」——提示文本受 Windows 的 128 字符上限约束，因此该标记放在最前面以保证可见。

Windows 气泡会检测任务栏所在边缘：底部任务栏向上展开，顶部向下展开，左右侧任务栏向屏幕内部展开，并始终限制在当前显示器工作区内。macOS 继续使用顶部菜单栏下方展开逻辑。

## 发布包

推送 `v*` 标签后，`Release` 工作流在原生 Windows、macOS 和 Linux runner 上构建，且仅在三个系统全部构建成功时创建 GitHub Release：

- Windows x64 NSIS 安装程序（`.exe`）
- macOS Intel/Apple Silicon 通用磁盘映像（`.dmg`）
- Linux x64 Debian 包和 AppImage（`.deb` / `.AppImage`）
- XFCE 面板集成脚本（`.tar.gz`）

也可以在 Actions 中手动触发 `Release` 工作流做分阶段发布：指定标签和平台（`all` / `windows` / `macos` / `linux`），只有被选中的平台会构建，产物追加到该标签的 Release。例如先只发布 Windows，待 macOS 验证通过后再以同一标签追加 macOS（追加时用 `--ref <标签>` 指向同一提交，避免产物与标签不一致）。若被推迟的平台在验证中暴露了必须先修复的问题，应把修复合入标签指向的提交（必要时重新打标签）再构建，否则该平台产物对应的源码会与标签不一致。

当前发布包未进行 Apple Developer ID 或 Windows Authenticode 签名，系统可能显示来源确认提示。

## 请求策略

应用固定连接 `https://www.999555999.com/api/v1`。正常刷新只读取：

- `GET /user/profile`
- `GET /usage/dashboard/stats`
- `GET /subscriptions/active`
- `GET /keys`
- `GET /groups/available`
- `GET /channel-monitors`

幸运抽奖按独立节奏轮询（默认 5 分钟，含 ±10% 抖动）：

- `GET /lottery`（当前期次、参与状态和参与资格）

以下明细接口在对应界面需要时加载，并缓存 15 分钟：

- `GET /usage/dashboard/trend`（概览中的 7 日趋势）
- `GET /usage`（记录页中的最近 18 条请求）

仅在 access token 返回 401 时调用一次 `/auth/refresh`。应用不会抓取网页、模拟点击、绕过 CAPTCHA 或轮询消费明细。

唯一的写操作是参与抽奖：用户点击「参与」，或开启「自动参与」后应用代为提交时，调用一次 `POST /lottery/rounds/{id}/join`。自动参与默认关闭，且每次提交前都会重新校验资格、名额和截止时间。

## 数据存储

- 登录令牌：macOS Keychain / Windows Credential Manager / Linux Secret Service
- 面板缓存、刷新间隔和自动参与开关：Tauri WebView 的应用专属本地存储
- 抽奖提醒开关与已通知期号：应用配置目录下的 `lottery-state.json`
- 密码：只在登录请求期间保存在内存中，不写入磁盘

使用前请确认账号所有者和站点运营方允许第三方客户端读取账号统计数据。
