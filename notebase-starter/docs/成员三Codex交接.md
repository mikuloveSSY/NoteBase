# 成员三代码交接给 Codex

目标仓库：https://github.com/mikuloveSSY/NoteBase
基线提交：4fb98597587fd5e32a26f79d5e2627295d634ef0（软件版本 0.6.0）。本包是未提交工作快照，尚未上传 GitHub，不是正式发布。

## 包内文件
- source/：基于上述提交的完整源码，排除 .git、依赖、构建产物及根目录历史 ZIP。
- member3.patch：包含修改和新增文件的 Git 二进制格式补丁，相对于基线提交。
- verification-logs/：此前运行留下的原始验证日志，包含早期失败记录，方便追溯。

## 已写入代码
- comrak 渲染、沙箱 iframe 预览、HTML/Markdown 导出与整库 ZIP；链接与资源问题提示。
- 版本快照、历史查看与恢复、版本去重与清理。
- ZIP 备份、校验、恢复前快照、恢复到新位置或当前库、定时维护配置。
- WebDAV 后端、三方比较、冲突副本与系统凭据接口；配置与手动同步界面。
- Rust、Tauri 命令装配、TypeScript 契约、现有保存流程以及界面接入。
核心入口：notebase-starter/crates/notebase-core/src/data_services/；界面入口：notebase-starter/src/member3.ts。

## 仍需完成和复核
- PDF 真实导出尚未实现：当前返回不支持。UI 中 PDF 平台打印选项不能视为完成。
- Windows 桌面编译与操作、真实系统凭据库、真实 WebDAV 服务和断网恢复尚未验收。同步算法测试不等于远端服务实测。
- 同类工具迁移兼容、完整需求逐项验收、大库性能、跨平台及安装包尚未确认完成。
- 测试辅助脚本在非 Windows 上跳过 5 个迁移案例；这些是未运行项，不算通过。需在 Windows 运行完整核心套件。
- 共享契约扩展、恢复与同步的安全边界须由接手者复核。不要直接将整份快照覆盖更新后的主干。

## 当前验证证据
先前 core-platform-final.log 中运行的核心测试均通过，另有 5 个迁移案例被过滤；core.log 与 core-platform.log 是早期失败记录。完整原始日志同时保留，不隐藏失败。
前端单元测试 17 项通过；浏览器测试 17 项通过；前端构建与核心 Clippy 日志显示成功。浏览器不提供真实磁盘/凭据/WebDAV验证。
这些是交接前已有日志，本次打包没有重新运行全部功能测试。补丁已检查可应用于准确基线，源码已作文件一致性检查。

## Codex 接手步骤
1. 读取目标仓库 AGENTS.md，检查远端和本地状态，保留其他成员改动。用户当前承担成员三，旧文件中的成员一身份不能覆盖当前职责。
2. 在目标仓库创建成员三功能分支。核对本包基线与最新 main。
3. 若分支正好处于基线，可先 git apply --check /path/member3.patch，再 git apply /path/member3.patch。
4. 若 main 已前进，在基线分支应用补丁、提交后与最新 main 合并/变基，逐项解决冲突；不要覆盖式复制 source/。
5. 检查新增 data_services、脚本、测试与界面文件都纳入提交。维护 CHANGELOG.md 和接口交接说明；本包保持 0.6.0，不擅自打发布标签。
6. 在 notebase-starter/ 执行 npm ci、npm test、npm run build、npm run test:e2e；Windows 上执行 npm run test:core。非 Windows 可用 python3 scripts/test_core_current_platform.py，但必须记录跳过项。
7. 执行 cargo clippy --manifest-path crates/notebase-core/Cargo.toml --all-targets -- -D warnings。验证 Tauri 桌面构建和关键操作，在隔离测试库检验恢复与同步。
8. 补齐或明确保留未完成项，再提交并推送功能分支到目标仓库，核对远端 SHA；报告分支/提交链接及未完成内容。功能分支推送不等于 main 已合并。

## 可直接发给 Codex 的指令
请读取此交接包，将成员三改动集成到 mikuloveSSY/NoteBase 的独立功能分支。先核对基线和最新主干，复核代码与测试，保留其他成员工作；不要把 PDF、Windows 或真实 WebDAV 标记为已验收。修复阻塞问题，更新 CHANGELOG 与交接文档，运行适用检查后提交并推送，给我远端分支和提交链接。

## 本次集成（2026-11-01）

- 独立功能分支：`feat/member3-render-data-services`；集成基线为远端 main `4fb98597587fd5e32a26f79d5e2627295d634ef0`。通过补丁检查后应用，保留成员一、二及外壳代码；软件版本保持 0.6.0。
- 修复手动整库备份忽略已保存目录的问题，显式 targetDir 仍可覆盖默认值；增加核心回归。禁用未实现的 PDF 选项并增加浏览器回归。
- 升级测试依赖 Vitest 与 Playwright，修复 npm audit 报告的依赖漏洞。

### 共享接口影响

保留既有顶层命令，通过可选 payload 字段扩展，不修改数据库 schema。此处记录集成约定，尚不代表其他成员已逐项验收。

| 命令 | 成员三扩展 |
| --- | --- |
| export_note | preview/body 用于当前文本预览；format/targetPath/scope 用于文件导出；PDF 返回不支持 |
| note_list / note_read | scope=versions 查询历史；versionId 读取历史正文 |
| note_restore | kind=version，结合原有 baseHash/expectedMtimeMs 防止覆盖外改 |
| backup_create / backup_restore | scope=note 创建版本，否则整库备份；verifyOnly 校验；mode=new/original 恢复 |
| settings_get / settings_set | values.backup 维护开关、周期、保留数与库外目录 |
| sync_configure / sync_now / sync_status | WebDAV 配置、系统凭据引用、resolvedPaths 显式冲突处理 |

Rust 定义位于 data_services 与 application；TypeScript 返回类型位于 src/contracts.ts。备份恢复暂停索引写入后重开；原目录与恢复前快照保留。同步使用三方比较、条件请求与冲突副本；真实服务和系统凭据仍需人工验收。

### 本次验证与修复记录

- 完整 Windows 核心测试：165 项通过，0 失败、0 忽略、0 过滤，包含先前非 Windows 跳过的迁移案例。
- 初次 Windows 核心运行发现 no_links 查询 verbatim 路径前缀导致 os error 1；已跳过非独立文件对象的卷前缀和根组件，再检查实际路径项。完整重跑通过。
- Windows 专用 Clippy 未使用导入已通过条件编译修复；`cargo clippy --all-targets -- -D warnings` 通过。
- 前端单元 17 项通过；Chrome 浏览器回归 18 项通过；TypeScript 与 Vite 构建通过（保留大于 500 kB 的打包提示）。npm audit 报告 0 漏洞。
- Tauri Windows release `--no-bundle` 编译通过；未生成安装包。桌面初轮 9 通过、4 启动等待超时，后续加入页面错误日志并将冷启动等待设为 30 秒；最终结果见下方。
- 本次 HTTP 适配器测试使用本机测试服务；同步三方算法含离线恢复测试。没有真实外部 WebDAV 服务或系统凭据库端到端验收。PDF、跨平台、大库性能与完整需求验收继续保留未完成状态。

最终桌面复测：完整 14 项中 13 项通过、1 项成员二自动保存 5 秒轮询超时；该失败项单独复测通过（1/1）。成员三新增真实 IPC 预览、HTML 导出、版本快照、整库备份校验及恢复到新位置全部通过。桌面仍存在计时不稳定性，不能表述为整轮零失败；没有降低自动保存断言或更改成员二保存逻辑。

本次代码已集成至功能分支，尚未合并 main；上文“工作快照尚未上传”和“此前日志”描述的是原交接包状态，不是本次验证状态。工具链、依赖、真实测试数据和生成程序均不纳入 Git。
