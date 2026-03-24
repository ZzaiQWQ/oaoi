# 🌸 OAOI Launcher

基于 **Tauri 2 + Rust + HTML/JS** 构建的 Minecraft 启动器，樱花粉主题设计，支持正版/离线，支持 Fabric / Forge / NeoForge / Quilt。

---

## ✨ 功能特性

| 功能 | 说明 |
|------|------|
| 🎮 游戏启动 | 支持 Vanilla / Fabric / Forge / NeoForge / Quilt |
| 🔑 微软正版登录 | OAuth2 → XSTS → Xbox → Minecraft 完整鉴权链 |
| 📦 版本管理 | 在线拉取版本列表，一键下载安装 |
| ☕ Java 自动管理 | 扫描系统 Java，版本不足时自动下载 JRE |
| 📂 整合包导入 | 拖拽 `.zip` / `.mrpack` 一键安装 CurseForge / Modrinth 整合包 |
| 🖥️ 服务器列表 | 预设服务器，点击直接进服 |
| ⚙️ 设置中心 | 内存分配、Java 路径、账号、主题切换 |
| 🌸 樱花动画 | 飘落花瓣粒子 + 点击粒子效果 |
| 🎨 DIY 个性化 | 背景图、主题色、花瓣样式、背景模糊半径、窗口透明度/圆角 |
| 🔇 静默后台 | 所有后台进程无黑窗（`CREATE_NO_WINDOW`） |
| ⚡ 智能省电 | 游戏启动后自动最小化，失焦时冻结所有动画和 GPU 渲染 |

---

## 🔨 开发 & 编译

### 环境要求

- Node.js 18+
- Rust (stable)
- `npm install` 安装依赖

### 开发模式

```bash
npx tauri dev
```

热重载，前端改动即时生效，Rust 改动自动重编译。

### 生产编译

```bash
cd src-tauri
cargo build --release
```

输出：`src-tauri/target/release/app.exe`

> ⚠️ 生产编译前确认 `tauri.conf.json` 中 `frontendDist` 指向正确路径。

---

## 📁 项目结构

```
oaoi-launcher/
├── index.html                     # 前端入口（data-no-drag 拖拽控制）
├── .env                           # 环境变量（MS_CLIENT_ID）
├── package.json
│
├── js/                            # 前端逻辑（模块化）
│   ├── main.js                    # 初始化入口，首次启动引导
│   ├── tauri.js                   # Tauri API，窗口控制，SakuraPetals，失焦冻结
│   ├── navigation.js              # 页面导航切换（SPA）
│   ├── sakura.js                  # 点击粒子特效
│   └── pages/
│       ├── home.js                # 主页：启动、Java 校验、启动后自动最小化
│       ├── download.js            # 下载页：版本列表、安装Modal、整合包拖拽
│       ├── settings.js            # 设置页：内存、Java路径、微软账号
│       ├── diy.js                 # DIY：主题、背景、粒子、模糊半径、透明度
│       └── instance-detail.js     # 实例详情：Mod 管理、在线搜索
│
├── styles/                        # 模块化 CSS
│   ├── global.css                 # 全局样式、CSS 变量、樱花动画
│   ├── titlebar.css               # 标题栏
│   ├── sidebar.css                # 侧边栏
│   └── pages/
│       ├── home.css / home/       # 主页（hero、news）
│       ├── servers.css / servers/ # 服务器页（header、server-cards）
│       ├── download.css / download/  # 下载页（版本列表、已安装、活跃下载、弹窗）
│       ├── settings.css / settings/  # 设置页（performance、account、visual）
│       ├── about.css / about/     # 关于页（hero、team、changelog）
│       ├── diy.css                # DIY 个性化
│       └── instance-detail.css    # 实例详情
│
├── assets/                        # 静态资源（背景图、新闻图等）
│
└── src-tauri/                     # Rust 后端
    ├── tauri.conf.json            # Tauri 应用配置
    ├── Cargo.toml                 # Rust 依赖
    ├── capabilities/
    │   └── default.json           # Tauri 权限声明
    └── src/
        ├── main.rs                # Rust 程序入口
        ├── lib.rs                 # 命令注册中心 + 拖拽事件
        ├── auth.rs                # 微软 OAuth 登录
        ├── instance.rs            # 实例管理（列表、删除、Mod）
        ├── game_dir.rs            # 游戏目录路径解析
        ├── java_detect.rs         # Java 扫描（轻量级内存查询）
        ├── java_download.rs       # Java JRE 自动下载（Adoptium）
        ├── launch.rs              # 游戏启动逻辑
        ├── modpack.rs             # 整合包导入（CF / Modrinth）
        ├── installer/
        │   ├── mod.rs             # 通用工具 + 公共函数（merge_libraries 等）
        │   ├── vanilla.rs         # 原版安装
        │   ├── fabric.rs          # Fabric 安装
        │   ├── forge.rs           # Forge 安装（ZipSlip 防护）
        │   ├── neoforge.rs        # NeoForge 安装（ZipSlip + 镜像）
        │   └── quilt.rs           # Quilt 安装
        └── versions/
            ├── mod.rs
            ├── fabric.rs          # Fabric 版本查询
            ├── forge.rs           # Forge 版本查询（竞速）
            ├── neoforge.rs        # NeoForge 版本查询
            └── quilt.rs           # Quilt 版本查询
```

---

## 🗂️ 文件功能详解

### 前端 JS

#### `js/main.js`
应用初始化入口。DOMContentLoaded 后执行：
- 检查 `localStorage` 中是否已设置游戏目录，若无则弹出首次启动引导
- 依次初始化各页面模块（home、download、settings 等）
- 注册全局事件监听（导航、窗口控制等）

#### `js/tauri.js`
前后端通信桥接层：
- 封装 `window.__TAURI__.core.invoke` 为统一调用接口
- 封装窗口拖拽 `startDragging()`、最小化、最大化、关闭
- 全窗口拖拽使用 `data-no-drag` 属性排除交互区域
- 封装 Tauri 事件监听 `listen()`
- 实现 `SakuraPetals` 类，控制飘落花瓣粒子
- 窗口失焦/最小化时自动冻结所有动画和 `backdrop-filter`，降低 GPU 占用

#### `js/navigation.js`
单页应用路由：
- 监听侧边栏导航点击
- 切换对应页面 section 的 `active` 状态
- 无刷新页面切换

#### `js/pages/home.js`
主页核心逻辑：
- 从 `localStorage` 读取已安装实例列表并渲染
- 根据 MC 版本自动判断所需 Java 版本（`getRequiredJavaMajor`）
- 调用 `find_java` 扫描系统 Java，不满足时触发自动下载
- 维护启动状态机（未选版本 / 检测Java / 启动中 / 运行中）
- 调用 `launch_minecraft` Tauri 命令启动游戏
- 启动成功后自动最小化窗口，减少资源占用
- 监听 `java-download-progress` / `java-download-done` 事件更新进度

#### `js/pages/download.js`
下载与安装中心：
- 调用 Mojang / BMCLAPI 拉取版本清单，渲染版本列表（release / snapshot / 历史版本）
- 点击版本弹出安装 Modal，支持选择 Loader 类型和版本
- 监听 `install-progress` 事件，实时显示安装进度条
- 注册文件拖拽区域，接收 `.zip` / `.mrpack` 传递给 `import_modpack` 命令

#### `js/pages/settings.js`
设置管理：
- 游戏目录选择（Tauri dialog）并持久化到 `localStorage`
- 内存分配滑块（128MB ~ 32GB）
- Java 路径：自动扫描模式调用 `find_java`，手动模式直接输入
- 微软账号：调用 `start_ms_login` 打开浏览器授权，监听回调更新账号信息，支持退出登录
- 设置保存与读取全部基于 `localStorage`

#### `js/pages/diy.js`
UI 自定义：
- 主题色切换（写入 CSS 变量）
- 自定义背景图（读取本地文件 base64 存入 localStorage）
- 控制樱花粒子动画开关、花瓣数量、颜色、样式
- 背景模糊半径调节（降低可减少 GPU 占用）
- 窗口透明度和圆角调节
- 所有配置持久化到 `localStorage`，下次启动自动恢复

---

### 后端 Rust

#### `src/lib.rs`
Tauri 命令注册中心：
- `tauri::generate_handler!` 注册所有后端命令
- 监听 `tauri://drag-drop` 事件，将拖入文件路径转发给前端
- 负责窗口初始化配置

#### `src/auth.rs` — 微软 OAuth 登录

完整鉴权链路：
```
启动本地 HTTP 回调服务器（随机端口）
  → 打开系统浏览器访问 Microsoft 授权页
  → 用户授权后浏览器重定向到本地回调
  → 捕获 code → POST 换取 access_token + refresh_token
  → 调用 Xbox Live 换取 XBL token
  → 调用 XSTS 换取 XSTS token
  → 调用 Minecraft API 换取 MC access_token + UUID + 用户名
  → 返回完整 Profile 信息给前端
```

#### `src/java_detect.rs` — Java 扫描

多源扫描策略，按优先级：
1. Windows 注册表（`HKLM\SOFTWARE\JavaSoft`）
2. 系统 `PATH` 环境变量中的 `java.exe`
3. 常见安装目录（`C:\Program Files\Java\`、`C:\Program Files\Eclipse Adoptium\` 等）
4. 游戏目录下的 `runtime\jre-{major}\`（启动器自动下载的 JRE）
5. 磁盘根目录暴力扫描（兜底）

每个找到的 Java 会执行 `java -version` 解析大版本号，返回去重后的完整列表。

#### `src/java_download.rs` — Java 自动下载

- 主源：`https://api.adoptium.net/v3/binary/latest/{major}/ga/windows/x64/jre/hotspot/normal/eclipse`
- 备源：`https://mirrors.tuna.tsinghua.edu.cn/Adoptium/{major}/jre/x64/windows/`
- 下载到 `{gameDir}/runtime/jre-{major}/`，解压后自动定位 `java.exe`
- 提供异步版本（带进度事件 `java-download-progress`）和同步版本（供整合包安装使用）

**Java 版本映射：**

| Minecraft 版本 | 需要 Java |
|---------------|-----------|
| 1.16 及以下 | Java 8 |
| 1.17 ~ 1.20.4 | Java 17 |
| 1.20.5 ~ 1.25.x | Java 21 |
| 26.x+ | Java 25 |

#### `src/instance.rs` — 实例管理

- `list_instances`：扫描 `{gameDir}/instances/` 目录，读取每个子目录的 `instance.json`，返回实例列表
- `delete_instance`：删除指定实例目录
- `resolve_game_dir`：统一解析游戏目录路径（供其他模块复用）

#### `src/launch.rs` — 游戏启动

核心流程：
1. 读取实例目录下的 `instance.json`，获取 `mainClass`、`assetIndex`、`libraries`
2. 构建 classpath：遍历 `libraries` 数组，按 OS rules 过滤，按 group:artifact 去重，拼接所有 `.jar` 路径 + `client.jar`
3. 解压 natives：老版本需要的 LWJGL `.dll` 从 natives jar 中提取到 `natives/` 目录
4. 自动检测系统语言，初次启动写入 `options.txt` 设置 MC 语言
5. 组装 JVM 参数：`-Xmx`、`-Xmn`、`-Djava.library.path`、`-DLog4J漏洞修补`、`-XX:+UseG1GC` 等
6. 替换版本 JSON 中的 `${变量}` 占位符（natives_directory、classpath 等）
7. 组装 Game 参数：`--username`、`--uuid`、`--accessToken`、`--gameDir`、`--assetsDir` 等
8. 支持 `--server {ip} --port {port}` 直连指定服务器
9. 优先使用 `javaw.exe`（无控制台窗口），启动输出重定向到 `launch_output.log`

#### `src/modpack.rs` — 整合包导入

**格式识别：**
- Modrinth：检测 zip 内是否含 `modrinth.index.json`
- CurseForge：检测 zip 内是否含 `manifest.json`

**Modrinth 安装流程：**
1. 解析 `modrinth.index.json` 获取 MC 版本、Loader 类型/版本、文件列表
2. 并发下载所有 mod 文件（最多 8 线程，带信号量限流）
3. 解压 `overrides/` 和 `client-overrides/` 覆盖配置

**CurseForge 安装流程：**
1. 解析 `manifest.json` 获取 MC 版本、Loader、mod 文件列表（fileID + projectID）
2. 分批（每批 500）调用 CurseForge API `/v1/mods/files` 批量获取下载链接
3. API 失败回退 CDN（`edge.forgecdn.net` / `mediafilez.forgecdn.net`）
4. 根据 modules 字段智能分类：mod → `mods/`，材质包 → `resourcepacks/`，光影 → `shaderpacks/`，存档 → `saves/`
5. 解压 overrides 目录

**安装前** 先并行安装基础 MC + Loader，**同时** 后台下载 mod 文件，最大化利用网络带宽。

---

### 安装器模块 `installer/`

#### `installer/mod.rs` — 通用工具 + 公共安装器函数
- `mirror_url()`：将 Mojang / Forge / Fabric / NeoForge 官方 URL 替换为 BMCLAPI 国内镜像
- `download_file_if_needed()`：下载前校验 SHA1，已存在且 hash 匹配则跳过；失败自动重试 5 次；官方源失败自动回退镜像源
- `parallel_download()`：并发下载任务池
- `library_allowed()`：根据 OS rules 判断 library 是否适用当前平台（Windows）
- `create_instance()` / `do_create_instance()`：创建新实例的主入口
- `maven_name_to_path()`：Maven 坐标转文件路径（公共函数，Forge/NeoForge 共用）
- `build_data_map()`：构建安装器 data 变量映射
- `resolve_data_arg()`：解析 `{DATA}` 和 `[maven]` 引用
- `get_jar_main_class()`：从 jar 的 MANIFEST.MF 获取 Main-Class
- `merge_libraries()`：按 group:artifact 去重合并库（四个 loader 安装器共用）

#### `installer/vanilla.rs` — 原版安装
1. 下载版本 JSON（`meta_url`）
2. 下载 `client.jar`（SHA1 校验）
3. 并发下载所有 libraries（32 线程）
4. 下载 asset index JSON，再并发下载所有 asset objects（32 线程）
5. 写入 `instance.json` 基础信息

#### `installer/fabric.rs` — Fabric 安装
1. 请求 `https://meta.fabricmc.net/v2/versions/loader/{mc}/{loader}/profile/json`
2. 并发下载 Fabric 专有 libraries
3. 合并 mainClass、libraries、JVM arguments 到 `instance.json`
4. 自动从 Modrinth 下载对应 MC 版本的最新 Fabric API

#### `installer/quilt.rs` — Quilt 安装
同 Fabric，API endpoint 为 `https://meta.quiltmc.org/v3/versions/loader/{mc}/{loader}/profile/json`

#### `installer/forge.rs` — Forge 安装
1. 从 BMCLAPI 下载 `forge-{mc}-{forge}-installer.jar`
2. 创建临时 `.minecraft` 目录，复制 `client.jar` 避免重复下载
3. 执行 `java -jar forge-installer.jar --installClient {tempDir}`
4. 从生成目录复制 libraries 到 `{gameDir}/libs/`
5. 解析 installer.jar 内的 `version.json`，合并 mainClass、libraries、arguments
6. 清理临时目录和 installer.jar

#### `installer/neoforge.rs` — NeoForge 安装（ATLauncher 方案）
NeoForge 不能直接运行 installer（需要 UI），改用手动执行方案：
1. 下载 `neoforge-{version}-installer.jar`
2. 解压 installer.jar 到 `.neoforge_temp/`
3. 读取 `version.json` 和 `install_profile.json`
4. 优先复制 installer 内 `maven/` 目录中的本地库，缺失的从网络下载
5. 执行 `install_profile.json` 中的 Processors（client 端），替换 `{DATA}` 和 `[maven]` 占位符
6. 合并 mainClass、libraries、arguments 到 `instance.json`
7. 清理临时目录

---

### 版本查询模块 `versions/`

| 文件 | API 端点 |
|------|---------|
| `fabric.rs` | `https://meta.fabricmc.net/v2/versions/loader/{mc}` |
| `quilt.rs` | `https://meta.quiltmc.org/v3/versions/loader/{mc}` |
| `forge.rs` | BMCLAPI（主）/ `files.minecraftforge.net` HTML 解析（备），竞速获取 |
| `neoforge.rs` | `https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge`，按 MC 版本前缀过滤，过滤 alpha/beta |

---

## ⚙️ 配置说明

### `.env`
```
MS_CLIENT_ID=你的_Azure_应用_Client_ID
```
用于微软 OAuth 登录。需在 [Azure 门户](https://portal.azure.com) 注册应用并开启 `XboxLive.signin` 权限。

### `tauri.conf.json` 关键配置
```json
{
  "app": {
    "windows": [{
      "transparent": true,      // 毛玻璃透明窗口
      "decorations": false,     // 无系统标题栏
      "width": 1000,
      "height": 620
    }]
  }
}
```

### `capabilities/default.json`
声明了窗口操作权限：最小化、最大化、取消最大化、关闭、拖拽、焦点、创建子窗口（MS 登录用）、文件对话框。

---

## 🔇 静默进程一览

所有后台子进程均设置 `CREATE_NO_WINDOW (0x08000000)`，防止黑窗闪烁：

| 位置 | 命令 |
|------|------|
| `java_detect.rs` | `where java`、`reg query`、`java -version` |
| `launch.rs` | `powershell -c (Get-Culture).Name` |
| `forge.rs` | `java -jar forge-installer.jar` |
| `neoforge.rs` | `java -cp ... {MainClass} {args}`（Processors） |
| `auth.rs` | `rundll32 url.dll,FileProtocolHandler {url}` |

---

## 📡 Tauri IPC 事件一览

| 事件名 | 方向 | 说明 |
|--------|------|------|
| `install-progress` | Rust → 前端 | 安装进度（stage / current / total / detail） |
| `java-download-progress` | Rust → 前端 | Java 下载进度 |
| `java-download-done` | Rust → 前端 | Java 下载完成（success / path / error） |
| `tauri://drag-drop` | 系统 → Rust → 前端 | 文件拖入窗口 |

---

## 📦 数据存储

所有用户配置存储在浏览器 `localStorage`：

| Key | 说明 |
|-----|------|
| `gameDir` | 游戏根目录路径 |
| `playerName` | 玩家昵称 |
| `memoryMB` | 分配内存（MB） |
| `javaMode` | `auto` 或 `manual` |
| `javaPath` | 手动指定的 Java 路径 |
| `msAccount` | 微软账号信息（JSON，含 uuid/name/token） |
| `selectedInstance` | 上次选择的实例名 |
| `useMirror` | 是否使用 BMCLAPI 国内镜像 |
| `theme` | 主题配置 |
| `bgImage` | 自定义背景图（base64） |
| `sakuraEnabled` | 樱花粒子开关 |
| `clickEffect` | 点击特效开关 |
| `blurRadius` | 背景模糊半径（0-30px，影响 GPU 占用） |

---

## 🗄️ 游戏目录结构

```
{gameDir}/
├── instances/
│   └── {实例名}/
│       ├── instance.json      # 实例配置（版本JSON + loader信息）
│       ├── client.jar         # 游戏主体
│       ├── mods/              # Mod 文件
│       ├── resourcepacks/     # 材质包
│       ├── shaderpacks/       # 光影包
│       ├── saves/             # 存档
│       ├── config/            # 配置文件
│       ├── natives/           # 原生库（.dll）
│       ├── options.txt        # MC 设置（含语言）
│       └── launch_output.log  # 启动日志
├── libs/                      # 所有版本共享的 libraries
│   └── {group}/{artifact}/{version}/*.jar
├── res/                       # 资源文件
│   ├── indexes/               # Asset index JSON
│   └── objects/               # Asset 对象（音效/材质）
└── runtime/                   # 启动器自动下载的 JRE
    └── jre-{major}/
        └── bin/
            └── java.exe
```
