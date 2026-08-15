# DSH Client

基于 [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 的桌面客户端，用 **Tauri 2** 构建。本目录是 fork 内的独立壳工程，**不修改任何上游代码**，方便随时 `git merge upstream/master` 同步官方。

## 架构

```
┌─────────────────────────────┐
│  Tauri 2 壳 (Rust)          │ 窗口 / 托盘 / 单实例 / 自启 / 更新
│  ├─ 内核管理 kernel.rs      │ 安装、spawn、守护、崩溃重启、在线更新
│  └─ lib.rs                  │ 托盘菜单、命令、更新编排
├─────────────────────────────┤
│  Node.js 运行时 (sidecar)   │ 由 scripts/prepare.mjs 下载打包
├─────────────────────────────┤
│  dsh 内核 (@deepseek-ai/dsh)│ 自包含 npm 安装产物，完全离线运行
│  └─ `dsh web --port 0`      │ 随机端口 + stdout 就绪信号
└─────────────────────────────┘
```

- **窗口**：加载 `dsh web` 服务的随机本地端口（`--port 0`），监听 dsh 打印的 `dsh web: http://127.0.0.1:PORT` 就绪行后导航；健康检查通过才算就绪。
- **进程守护**：内核崩溃自动重启（最多 3 次），日志写入系统日志目录（`open_logs` 命令可打开）。
- **托盘**：打开主界面 / 检查客户端更新 / 检查内核更新 / 打开日志 / 开机自启 / 退出。关闭窗口 = 隐藏到托盘。
- **双层在线更新**：
  - 客户端（壳）：Tauri updater（TUF 签名），更新源为 GitHub Releases 的 `latest.json`。
  - 内核（dsh）：启动时及托盘手动检查 npm registry 最新版，从发布渠道下载 `kernel-<version>.tar.gz`，原子安装后自动重启内核。

## 本地开发

```sh
cd client
npm install
npm run dev        # 起 dsh web(1420) + tauri dev 窗口
npm run build      # 打包（macOS 产出 .app/.dmg）
```

`scripts/prepare.mjs` 会自动下载 Node.js 运行时并安装 dsh 内核（产物在 `resources/`，已 gitignore）。可用环境变量覆盖：`DSH_VERSION`（内核版本）、`NODE_VERSION`、`NPM_REGISTRY`、`NODE_DIST_MIRROR`。

## 发布流程

1. **发版客户端**：推送 `v0.1.1` 之类的 tag。CI（`.github/workflows/client-release.yml`）在 macOS/Windows/Linux 三平台构建，发布 GitHub Release，并自动生成聚合更新清单 `latest.json`。
   - 首次需在仓库 Settings → Secrets and variables → Actions 添加 `TAURI_SIGNING_PRIVATE_KEY`（本机密钥在 `~/.tauri/dsh-client.key`，公钥已写入 `tauri.conf.json`；私钥丢失将无法发布更新，务必妥善备份）。
2. **发版内核**：上游 `@deepseek-ai/dsh` 出新版本后，推送 `kernel-<版本号>` tag（或手动触发 kernel-release workflow），CI 构建自包含内核包并发布。
3. **同步上游**：每天自动检查上游新提交并开 sync issue；手动同步：

```sh
git fetch upstream
git merge upstream/master
git push origin dev
```

## 安全说明

- dsh 服务绑定 `127.0.0.1`，仅本机可访问。
- 更新包使用 TUF 风格签名校验，防止中间人替换。
- 内核更新下载 HTTPS 资源并原子替换（临时目录 + rename），失败自动回滚到旧版本目录。

## 目录结构

```
client/
├── src/              前端 loading 页（启动/错误状态）
├── src-tauri/        Rust 壳（kernel.rs 为内核管理核心）
├── scripts/          prepare.mjs（资源准备）、dev.mjs、make-update-json.mjs
└── resources/        构建时生成（node 运行时 + kernel.tar.gz，gitignore）
```
