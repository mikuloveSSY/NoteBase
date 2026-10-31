# NoteBase 四人成果整合工程

这是四人小组的共同工程。本分支在一二四集成版上真实合入成员三，保留 **0.7.0 开发版**，未正式发布。当前功能、内部接口和验证见 [四人成果整合验收](docs/四人成果整合验收.md)，原成员交接保留其历史状态。

后续Windows候选版已推进历史重命名/中断恢复、保留天数/关闭、回拨序列与恢复故障保护，交付及最新检查见[Windows候选版验收](docs/Windows候选版验收.md)。程序用户参阅[Windows使用说明](docs/Windows使用说明.md)，开发者继续使用下方命令。完整需求状态以根目录需求矩阵为准，不从“可双击启动”推算全部验收完成。

## 现在具备什么

- Tauri 2 桌面入口、Rust 核心、TypeScript 与 Vite 前端、CodeMirror 6 编辑器。
- 笔记库打开、文件枚举、新建、读取与手动保存。桌面模式操作真实 `.md` 文件。
- 请求与响应信封及 23 个既有命令；桌面已装配真实服务，环境或参数不满足时明确报错。独立嵌入调用没有装配端口时不返回假成功。
- 同名文件自动加序号；保存采用同目录临时文件和原子替换；保存去除 UTF-8 BOM 并保留 LF/CRLF 风格；保存前检查内容哈希，拒绝外部修改后的覆盖。
- 新建标题自动修正非法字符和系统保留名；非法 UTF-8、只读文件和超过 10 MiB 的文件以只读方式展示，大文件仅预览前 256 KiB。列表可继续加载后续分页。
- 基础路径校验，拒绝越界路径、系统保留名和符号链接。编辑器保留完整原文，属性面板可显式更新frontmatter，非法或重复YAML拒绝改写。
- 浏览器联调模式，仅使用内存数据，刷新即重置；界面会显著标注。浏览器模式不能验证真实磁盘写入。

- 已整合成员二：2 秒空闲/10 秒连续输入自动保存、500ms 草稿调度、恢复、外部冲突显式处理、属性/标签/模板/附件、回收站、双链/反链/提及/重命名及关系图。
- 已补齐成员四：主题、编辑偏好、快捷键、命令面板、轻量标签页与恢复、同库多窗口、原生菜单、Windows 全局快捷键适配和本地脱敏日志。
- 已接入成员一 0.7.0：锁外检索、读连接复用、切库取消、中文短词筛选、重建空间回收；最终规模性能仍待复测。
- 已接入成员三：共用 comrak 安全预览、MD/HTML/Windows PDF/整库导出、版本快照与回滚、备份校验和恢复、WebDAV 与系统凭据。数据长任务在 App 锁外执行，维护时保留编辑草稿并限速重试写入；多窗口先关闭其他窗口再恢复。

**尚未完成验收**：完整需求追溯、角色布局、安装升级卸载、公网 WebDAV/代理、跨平台和最终规模性能；不能从本机测试推定整套需求完成。PDF 需要 Windows Chrome 或 Edge，独立 WebDAV/凭据专项使用临时环回服务。

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
# Windows NSIS 安装包构建命令（本轮未生成/验收）
npm run desktop:build
# 便携测试包，要求已构建 release
./scripts/package-portable.ps1
# 在已安装依赖和 Edge 的 Windows 开发环境执行完整检查
./scripts/verify.ps1
```

Windows 可执行文件输出到 `src-tauri/target/release/notebase.exe`。桌面测试使用独立临时笔记库及 WebView2 配置，不操作日常笔记。浏览器回归可设置 PowerShell 环境变量 `$env:PLAYWRIGHT_CHANNEL='msedge'` 使用已安装 Edge。Windows 安装包目标为 NSIS，输出到 `src-tauri/target/release/bundle/nsis/`；旧成员四分支有 0.6.0 打包记录，本轮 0.7.0 只构建 no-bundle 程序，安装/卸载和其他平台未验收。便携包输出 `dist-delivery/`，仍需系统 WebView2，不自带运行库。

## 四个人从哪里开始

| 成员 | 当前入口 | 接下来先做的一件事 |
|---|---|---|
| 一 存储与检索 | `crates/notebase-core/src/infra.rs`、`index/` | 已接入 0.7.0 并发与维护暂停；后续补最终规模性能及路径竞态验证 |
| 二 编辑与知识组织 | `note_service.rs`、`links.rs`、`src/editor-session.ts` | 本分支已合入其交付；复核多窗口与索引联调，继续维护 note/link 业务 |
| 三 导出与数据保障 | `data_services/`、`src/member3.ts` | 已接入渲染/导出/版本/备份/同步；后续验证公网、跨平台及中断恢复边界 |
| 四 界面与集成 | `src/`、`src-tauri/`、`settings.rs`、`logger.rs` | 已接全员服务及维护生命周期；组织需求追溯、安装升级卸载和最终验收 |

详细职责边界见 `docs/团队开发指南.md`。`src/contracts.ts` 和 Rust `domain.rs` 是双方共享约定，变更时一起修改、一起评审。

## 第一次小组联调

1. 全员从同一份工程开始，在测试库中跑通新建、修改、保存、关闭后重开。
2. 成员四将这份工程放入小组仓库，成员分别建分支；不要各自新建一套工程。
3. 成员一和成员二先约定存储端口，成员二和成员三约定保存后快照，成员三和成员四约定渲染接入。
4. 每次合并至少执行前端构建、契约测试和 Rust 核心测试。

## 当前验证边界

当前实际结果以[四人成果整合验收](docs/四人成果整合验收.md)及其 evidence/team-all 日志为准；原成员四 184 项与一二四 195 项只是对应提交的历史记录。本轮不发布安装包。

NoteStore 字节端口与路径规则保留，索引队列/watcher 已接通，indexQueued 只表示入队。snapshotCreated 按独立快照结果返回，失败不撤销成功的文件保存。索引暂停等待在途查询和写线程结束，恢复完成重装服务并重建；外部写入竞态、大库枚举与最终性能仍须专门验收。内部契约与回退注意事项见[整合验收](docs/四人成果整合验收.md)。

## 技术资料

- Tauri 2 开发配置：https://v2.tauri.app/develop/
- Tauri Windows 前置条件：https://v2.tauri.app/start/prerequisites/
- CodeMirror 6：https://codemirror.net/examples/basic/
- 原始项目材料见 `docs/reference/`。
