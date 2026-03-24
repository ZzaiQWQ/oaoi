# 🌸 OAOI Launcher — Minecraft 服务器启动器

基于 **Tauri 2** 构建的自定义 Minecraft 服务器启动器，樱花粉主题设计。

![preview](assets/preview.png)

## ✨ 功能特性

- 🎮 **游戏启动** — 支持 Vanilla / Fabric / Forge 多加载器
- 🔑 **微软正版登录** — 一键 OAuth 认证
- 📦 **版本管理** — 在线下载、筛选、安装 Minecraft 版本
- 🖥️ **服务器列表** — 精选服务器展示
- ⚙️ **设置中心** — 内存分配、Java 自动查找、主题切换
- 🌸 **樱花动画** — 飘落花瓣 + 点击粒子效果

## 技术栈

| 层级 | 技术 | 说明 |
|------|------|------|
| 前端 | HTML + JS + CSS | 模块化架构，组件化样式 |
| 后端 | Tauri 2 (Rust) | 窗口管理、微软登录、系统调用 |
| 构建 | npm + Cargo | `@tauri-apps/cli` 驱动构建流程 |

## 项目结构

```
oaoi-launcher/
├── index.html                          # 入口页面（内联HTML骨架）
├── .env                                # 环境变量
├── package.json                        # 依赖配置
│
├── js/                                 # 模块化 JavaScript
│   ├── main.js                         # 入口，初始化所有模块
│   ├── tauri.js                        # Tauri API + 窗口控制 + 樱花类
│   ├── navigation.js                   # 页面导航切换
│   ├── sakura.js                       # 点击粒子效果
│   └── pages/
│       ├── home.js                     # 启动按钮、版本选择、新闻
│       ├── download.js                 # 版本下载、安装 Modal
│       └── settings.js                 # 设置交互、Java查找、登录
│
├── styles/                             # 模块化 CSS
│   ├── global.css                      # 变量、动画、页面切换
│   ├── titlebar.css                    # 窗口标题栏
│   ├── sidebar.css                     # 左侧导航栏
│   └── pages/
│       ├── home.css         → home/hero.css + news.css
│       ├── servers.css      → servers/header.css + server-cards.css
│       ├── download.css     → download/official-versions.css
│       │                      + installed-versions.css
│       │                      + active-downloads.css
│       │                      + install-modal.css
│       ├── settings.css     → settings/performance.css
│       │                      + account.css + visual.css
│       └── about.css        → about/hero.css + team.css
│                              + changelog.css
│
├── assets/                             # 静态资源
│   ├── bg.png / bg2.png ...            # 背景图
│   └── news1.png / news2.png           # 新闻图
│
└── src-tauri/                          # Tauri 后端（Rust）
    ├── tauri.conf.json                 # 应用配置
    ├── Cargo.toml                      # Rust 依赖
    ├── src/
    │   ├── main.rs                     # Rust 入口
    │   └── lib.rs                      # 核心逻辑
    ├── capabilities/
    │   └── default.json                # 权限配置
    └── icons/                          # 应用图标

```

