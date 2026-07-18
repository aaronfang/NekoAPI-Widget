# NekoAPI Widget

一个低负荷的 NekoAPI 桌面用量监视器。应用常驻系统菜单栏，显示今日消费、余额、Token 用量和平台分布。

## 第一版功能

- NekoAPI 账号登录和 TOTP 二次验证
- access token / refresh token 存入系统钥匙串
- 每次同步仅请求用户资料和用量汇总两个只读接口
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
cargo check --manifest-path src-tauri/Cargo.toml
```

生成 macOS 应用：

```bash
npm run tauri build -- --bundles app
```

## 请求策略

应用固定连接 `https://999555999.com/api/v1`。正常刷新只读取：

- `GET /user/profile`
- `GET /usage/dashboard/stats`

仅在 access token 返回 401 时调用一次 `/auth/refresh`。应用不会抓取网页、模拟点击、绕过 CAPTCHA 或轮询消费明细。

## 数据存储

- 登录令牌：macOS Keychain / Windows Credential Manager / Linux Secret Service
- 面板缓存和刷新间隔：Tauri WebView 的应用专属本地存储
- 密码：只在登录请求期间保存在内存中，不写入磁盘

使用前请确认账号所有者和站点运营方允许第三方客户端读取账号统计数据。
