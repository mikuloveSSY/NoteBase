# NoteBase 主干基础工程

这是四人小组的共同工程，当前为 **0.7.0 成员一性能优化与交接**。已在 Windows 验证“打开文件夹 → 新建笔记 → 编辑 → 手动保存 → 重新打开”，后续按四人分工完善性能、链接、渲染和备份。

当前接入请先阅读 [成员一交接总览](docs/成员一交接总览-v0.7.0.md)，各旧版交接文档保留历史行为。

## 现在具备什么

- Tauri 2 桌面入口、Rust 核心、TypeScript 与 Vite 前端、CodeMirror 6 编辑器。
- 笔记库打开、文件枚举、新建、读取与手动保存。桌面模式操作真实 `.md` 文件。
- 请求与响应信封、已有 23 个命令名称的入口注册。未实现命令返回 `E_INTERNAL` 并说明未实现，不返回假数据。
- 同名文件自动加序号；保存采用同目录临时文件和原子替换；保存去除 UTF-8 BOM 并保留 LF/CRLF 风格；保存前检查内容哈希，拒绝外部修改后的覆盖。
- 新建标题自动修正非法字符和系统保留名；非法 UTF-8、只读文件和超过 10 MiB 的文件以只读方式展示，大文件仅预览前 256 KiB。列表可继续加载后续分页。
- 基础路径校验，拒绝越界路径、系统保留名和符号链接。可读取 frontmatter，但编辑器仍编辑完整原文件，不重写 YAML。
- 浏览器联调模式，仅使用内存数据，刷新即重置；界面会显著标注。浏览器模式不能验证真实磁盘写入。

**尚未完成**：自动保存、草稿、崩溃恢复、链接、Markdown 预览、导出、回收站、备份、同步和完整设置（当前只支持 vaultPath）。没有达到交接包 W1 的完成定义，也没有完成整套需求验收。

## Windows 首次启动

打开库时会在后台建立 SQLite 索引，右侧显示状态。缓存默认位于应用数据目录的 `vaults/<vault_id>/index.db`，不放进笔记库。本版保存和外部文件变化会自动更新索引；监听失败时降级为默认每 300 秒对账。索引失败不阻止文件编辑。本版范围见 [重建迁移交接](docs/重建迁移交接-v0.6.0.md)。

1. 安装 Node.js 22.12+（或兼容的较新 LTS）、Rust 稳定版、Visual Studio Build Tools 的“使用 C++ 的桌面开发”工作负载及 Windows SDK。确认使用 Rust 的 MSVC 工具链。
2. 确认 Microsoft Edge WebView2 Runtime 可用。Windows 首次编译需联网下载 npm 与 Cargo 依赖。
3. 解压工程，用 VS Code 打开 `notebase-starter` 文件夹。在该文件夹的终端执行：

```powershell
npm ci
npm run desktop
```

首次编译会较慢。启动后，先在资源管理器创建一个测试文件夹，例如 `D:\NoteBase-demo`；在应用左侧输入该绝对路径，点击“打开已有文件夹”。初期请使用测试库。

如果 Rust 安装后终端找不到 `cargo`，关闭并重新打开终端。`npm run desktop` 才是桌面程序；`npm run dev` 只是浏览器演示。

### 仅看界面

```powershell
npm ci
npm run dev
```

浏览器访问终端显示的地址，默认 `http://127.0.0.1:1420`。演示库有欢迎笔记。创建、编辑、保存会修改内存中的演示内容，刷新后重置。

### 测试和构建

```powershell
npm run build
npm test
npm run test:core
# 可选 浏览器操作回归
npx playwright install chromium
npm run test:e2e
# Windows 桌面编译与真实 IPC 回归
npm run desktop:build -- --no-bundle
npm run test:desktop
# 可选 Windows 桌面安装包（本版未验收）
npm run desktop:build
```

Windows 可执行文件输出到 `src-tauri/target/release/notebase.exe`。桌面测试使用独立临时笔记库及 WebView2 配置，不操作日常笔记。浏览器回归可设置 PowerShell 环境变量 `$env:PLAYWRIGHT_CHANNEL='msedge'` 使用已安装 Edge。Windows 安装包目标为 NSIS，默认输出到 `src-tauri/target/release/bundle/nsis/`；本版未验收安装包、macOS/Linux。

## 四个人从哪里开始

| 成员 | 当前入口 | 接下来先做的一件事 |
|---|---|---|
| 一 存储与检索 | `crates/notebase-core/src/infra.rs`、`domain.rs` | 维护存储、检索、监视、恢复及迁移；完成性能证据并处理团队联调 |
| 二 编辑与知识组织 | `crates/notebase-core/src/application.rs` | 从现有笔记用例拆出 note-svc，补自动保存、草稿与恢复；随后接 link-svc |
| 三 导出与数据保障 | Rust 核心的应用层与基础设施层扩展位置 | 先实现 renderer 端口和 comrak，供预览与导出共用，再做备份及版本快照 |
| 四 界面与集成 | `src/`、`src-tauri/` | 保持现有读写闭环可用，逐项连接其余服务；统一维护 IPC、界面、日志和打包 |

详细职责边界见 `docs/团队开发指南.md`。`src/contracts.ts` 和 Rust `domain.rs` 是双方共享约定，变更时一起修改、一起评审。

## 第一次小组联调

1. 全员从同一份工程开始，在测试库中跑通新建、修改、保存、关闭后重开。
2. 成员四将这份工程放入小组仓库，成员分别建分支；不要各自新建一套工程。
3. 成员一和成员二先约定存储端口，成员二和成员三约定保存后快照，成员三和成员四约定渲染接入。
4. 每次合并至少执行前端构建、契约测试和 Rust 核心测试。

## 当前验证边界

本版已完成 Windows 核心测试、前端构建、契约测试、浏览器回归、桌面 release 编译和真实桌面 IPC 测试，见 [本版验证记录](docs/重建迁移交接-v0.6.0.md)。原 [0.1.0 验证记录](docs/验证记录.md) 仅作为历史记录。

NoteStore 已提供 RelPath 等值对象及 read、write_atomic、create_new、rename、delete、list、stat；原始字节读写签名仍需与成员二对齐。底层重命名、删除不代表对应业务命令已实现。初始索引、增量队列与 watcher 已接入，保存响应 indexQueued 表示任务接收；快照仍未接入，snapshotCreated=false。桌面搜索已移出全局锁并复用两个只读连接；其余命令仍串行。性能证据以 0.7.0 性能报告为准，路径竞态和大库枚举仍需注意。接口取舍和需求状态见 [存储初版交接](docs/存储初版交接.md)。

## 技术资料

- Tauri 2 开发配置：https://v2.tauri.app/develop/
- Tauri Windows 前置条件：https://v2.tauri.app/start/prerequisites/
- CodeMirror 6：https://codemirror.net/examples/basic/
- 原始项目材料见 `docs/reference/`。
