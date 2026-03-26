# 🌸 OAOI Launcher — Minecraft 服务器启动器

基于 **Tauri 2** 构建的自定义 Minecraft 服务器启动器，樱花粉主题设计。

![preview](assets/preview.png)

## ✨ 功能特性

- 🎮 **游戏启动** — 支持 Vanilla / Fabric / Forge / NeoForge / Quilt 多加载器
- 🔑 **微软正版登录** — 一键 OAuth 认证
- 📦 **版本管理** — 在线下载、筛选、安装 Minecraft 版本
- ☕ **Java 自动管理** — 自动扫描系统 Java，按版本需求自动下载 JRE
- 🖥️ **服务器列表** — 精选服务器展示
- ⚙️ **设置中心** — 内存分配、Java 自动/手动查找、主题切换
- 🌸 **樱花动画** — 飘落花瓣 + 点击粒子效果
- 🔇 **静默后台** — 所有后台进程无黑框闪烁（Windows `CREATE_NO_WINDOW`）
- 📁 **Natives 自动解压** — 老版本（1.14 等）启动时自动解压 LWJGL native dll

## 技术栈

| 层级 | 技术 | 说明 |
|------|------|------|
| 前端 | HTML + JS + CSS | 模块化架构，组件化样式 |
| 后端 | Tauri 2 (Rust) | 窗口管理、微软登录、系统调用 |
| 构建 | npm + Cargo | `@tauri-apps/cli` 驱动构建流程 |

---

## 🔨 编译方式

### 方式一：开发模式（`npx tauri dev`）

热重载模式，适合开发调试。前端资源直接从项目根目录加载。

```bash
# 确保 tauri.conf.json 中：
#   "frontendDist": "../"

npx tauri dev
```

- ✅ 修改 JS/CSS 自动刷新
- ✅ 修改 Rust 自动重编译
- ⚠️ 使用 debug 编译，性能较差
- 输出：`src-tauri/target/debug/app.exe`

### 方式二：生产编译（`cargo build --release`）

发布版本，需要将前端资源放到 `dist/` 目录。

**步骤：**

```bash
# 1. 修改 tauri.conf.json
#    "frontendDist": "../"  →  "../dist"

# 2. 确保 dist/ 目录结构正确（符号链接或复制）
mkdir dist 2>$null
# 复制 index.html
Copy-Item index.html dist/
# 创建符号链接（管理员权限）
mklink /D dist\js js
mklink /D dist\styles styles
mklink /D dist\assets assets

# 3. 编译
cd src-tauri
cargo build --release

# 4. 编译完成后改回 dev 配置
#    "frontendDist": "../dist"  →  "../"
```

- ✅ 优化编译，体积小性能好
- ✅ 输出：`src-tauri/target/release/app.exe`
- ⚠️ 编译前必须改 `frontendDist` 为 `"../dist"`，编译后改回 `"../"`

### `tauri.conf.json` 切换对照

| 配置项 | 开发模式 | 生产编译 |
|--------|----------|----------|
| `frontendDist` | `"../"` | `"../dist"` |

---

## 项目结构

```
oaoi-launcher/
├── index.html                          # 入口页面
├── .env                                # 环境变量（Azure Client ID）
├── package.json                        # 依赖配置
│
├── js/                                 # 模块化 JavaScript
│   ├── main.js                         # 入口，初始化 + 首次引导
│   ├── tauri.js                        # Tauri API + 窗口控制 + 樱花类
│   ├── navigation.js                   # 页面导航切换
│   ├── sakura.js                       # 点击粒子效果
│   └── pages/
│       ├── home.js                     # 启动按钮、Java 自动选择、新闻
│       ├── download.js                 # 版本下载、安装 Modal
│       └── settings.js                 # 设置、Java 查找、微软登录
│
├── styles/                             # 模块化 CSS
│   ├── global.css                      # 变量、动画、页面切换
│   ├── titlebar.css                    # 窗口标题栏
│   ├── sidebar.css                     # 左侧导航栏
│   └── pages/
│       ├── home.css                    # 主页样式
│       ├── servers.css                 # 服务器列表样式
│       ├── download.css                # 下载页样式
│       ├── settings.css                # 设置页样式
│       └── about.css                   # 关于页样式
│
├── assets/                             # 静态资源
│   ├── bg.png / bg2.png ...            # 背景图
│   └── news1.png / news2.png           # 新闻图
│
├── dist/                               # 生产构建资源（符号链接）
│   ├── index.html                      # 复制自根目录
│   ├── js/ → ../js/                    # 符号链接
│   ├── styles/ → ../styles/            # 符号链接
│   └── assets/ → ../assets/            # 符号链接
│
└── src-tauri/                          # Tauri 后端（Rust）
    ├── tauri.conf.json                 # 应用配置
    ├── Cargo.toml                      # Rust 依赖
    ├── src/
    │   ├── main.rs                     # Rust 入口
    │   ├── lib.rs                      # 核心模块注册 + Tauri 命令列表
    │   ├── auth.rs                     # 微软 OAuth 登录流程
    │   ├── game_dir.rs                 # 游戏目录管理
    │   ├── instance.rs                 # 实例列表 + 版本删除
    │   ├── java_detect.rs              # Java 系统扫描（registry/PATH/磁盘根）
    │   ├── java_download.rs            # Java JRE 自动下载（Adoptium API）
    │   ├── launch.rs                   # 游戏启动（参数拼接/natives解压/语言设置）
    │   ├── installer/                  # 安装器模块
    │   │   ├── mod.rs                  # 通用安装逻辑 + 库下载
    │   │   ├── vanilla.rs              # 原版安装
    │   │   ├── fabric.rs               # Fabric 安装
    │   │   ├── forge.rs                # Forge 安装
    │   │   ├── neoforge.rs             # NeoForge 安装
    │   │   └── quilt.rs                # Quilt 安装
    │   └── versions/                   # 在线版本查询
    │       ├── mod.rs                  # 版本查询入口
    │       ├── fabric.rs               # Fabric 版本列表
    │       ├── forge.rs                # Forge 版本列表
    │       ├── neoforge.rs             # NeoForge 版本列表
    │       └── quilt.rs                # Quilt 版本列表
    ├── capabilities/
    │   └── default.json                # Tauri 权限配置
    └── icons/                          # 应用图标
```

---

## ☕ Java 自动管理

### 版本映射

| Minecraft 版本 | 需要 Java |
|---------------|-----------|
| 1.16 及以下 | Java 8 |
| 1.17 ~ 1.20.4 | Java 17 |
| 1.20.5 ~ 1.25.x | Java 21 |
| 26.x+ | Java 25 |

### 自动流程

1. 点击启动 → 根据 MC 版本确定所需 Java 版本
2. `find_java` 扫描系统（PATH / 注册表 / 磁盘根目录 / `gameDir/runtime/`）
3. 找到匹配版本 → 直接使用
4. 未找到 → 自动从 [Adoptium](https://adoptium.net/) 下载 JRE
5. 下载路径：`{gameDir}/runtime/jre-{major}/`

---

## 🔇 后台静默

所有 `std::process::Command` 调用（除游戏启动外）均添加了 Windows `CREATE_NO_WINDOW`（`0x08000000`）标志，防止黑色控制台窗口闪烁。

| 调用位置 | 命令 | 静默 |
|---------|------|------|
| `java_detect.rs` | `where java` | ✅ |
| `java_detect.rs` | `reg query` | ✅ |
| `java_detect.rs` | `java -version` | ✅ |
| `launch.rs` | `powershell`（语言检测） | ✅ |
| `launch.rs` | 游戏启动 | ❌（需要显示游戏窗口） |
| `forge.rs` | Forge 安装器 | ✅ |
| `neoforge.rs` | NeoForge 安装器 | ✅ |
| `auth.rs` | `rundll32`（打开浏览器） | ✅ |

---

## 📋 环境要求

- **Node.js** 18+
- **Rust** (stable)
- **Tauri CLI**: `npm install @tauri-apps/cli`
- **Windows 10/11**（WebView2 运行时）
