# NoteBase 主干基础工程

这是四人小组的共同工程，当前软件版本 **0.6.0**。主干已整合成员一的搜索、增量索引、文件监视、重建与迁移，以及成员二的自动保存、恢复和知识组织。接口与本轮验证见[成员二主干合并验证](docs/成员二主干合并验证.md)。

## 现在具备什么

- Tauri 2 桌面入口、Rust 核心、TypeScript 与 Vite 前端、CodeMirror 6 编辑器。
- 笔记库打开、文件枚举、新建、读取、手动及自动保存、持久草稿与显式恢复、软删除与回收站恢复。桌面模式操作真实 `.md` 文件。
- 属性、标签/层级标签、收藏/置顶、模板和附件插入；真实双链、反链、提及转换、可恢复重命名与受限关系图谱。
- 请求与响应信封、已有 23 个命令名称的入口注册。未实现命令返回 `E_INTERNAL` 并说明未实现，不返回假数据。
- 同名文件自动加序号；保存采用同目录临时文件和原子替换；保存去除 UTF-8 BOM 并保留 LF/CRLF 风格；保存前检查内容哈希，拒绝外部修改后的覆盖。
- 新建标题自动修正非法字符和系统保留名；非法 UTF-8、只读文件和超过 10 MiB 的文件以只读方式展示，大文件仅预览前 256 KiB。列表可继续加载后续分页。
- 基础路径校验，拒绝越界路径、系统保留名和符号链接。编辑器保留完整原文，属性面板可显式更新frontmatter，非法或重复YAML拒绝改写。
- 浏览器联调模式，仅使用内存数据，刷新即重置；界面会显著标注。浏览器模式不能验证真实磁盘写入。

**后续接入**：Markdown预览、导出、版本快照/备份、同步、其余设置及多窗口。链接查询按权威文件计算，未完成10万篇规模验收。

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
| 一 存储与检索 | `crates/notebase-core/src/infra.rs`、`domain.rs` | 搜索、增量队列、watcher和重建迁移已接入；继续性能和并发优化 |
| 二 编辑与知识组织 | `src/note_service.rs`、`src/links.rs`（相对 core crate） | 已与主干增量索引和迁移接通，继续对接快照及团队界面 |
| 三 导出与数据保障 | Rust 核心的应用层与基础设施层扩展位置 | 先实现 renderer 端口和 comrak，供预览与导出共用，再做备份及版本快照 |
| 四 界面与集成 | `src/`、`src-tauri/` | 保持现有读写闭环可用，逐项连接其余服务；统一维护 IPC、界面、日志和打包 |

详细职责边界见 `docs/团队开发指南.md`。`src/contracts.ts` 和 Rust `domain.rs` 是双方共享约定，变更时一起修改、一起评审。

## 第一次小组联调

1. 全员从同一份工程开始，在测试库中跑通新建、修改、保存、关闭后重开。
2. 成员四将这份工程放入小组仓库，成员分别建分支；不要各自新建一套工程。
3. 成员一和成员二先约定存储端口，成员二和成员三约定保存后快照，成员三和成员四约定渲染接入。
4. 每次合并至少执行前端构建、契约测试和 Rust 核心测试。

## 当前验证边界

主干0.6原实现验证见[重建迁移交接](docs/重建迁移交接-v0.6.0.md)，成员二原实现见[成员二验证](docs/成员二验证记录.md)，合并后的实际结果见[合并验证](docs/成员二主干合并验证.md)。

NoteStore字节接口与SQL模式保持不变。笔记实际文件变动已接入增量索引，indexQueued仅表示已接收；快照尚未接入时snapshotCreated=false。全库枚举、App串行锁和外部并发写入竞态仍需后续完善；不宣称大库性能达标。

## 技术资料

- Tauri 2 开发配置：https://v2.tauri.app/develop/
- Tauri Windows 前置条件：https://v2.tauri.app/start/prerequisites/
- CodeMirror 6：https://codemirror.net/examples/basic/
- 原始项目材料见 `docs/reference/`。
