# NoteBase

main 已汇总 **四人成果整合开发版 0.7.0**，尚非正式发布。可运行工程位于 [notebase-starter](notebase-starter/README.md)，原始 0.1.0 ZIP 保留归档。

已整合存储检索、笔记链接、UI 外壳，以及成员三的预览、导出、版本历史、备份恢复和 WebDAV。长任务在 App 锁外执行，恢复和维护有库级互斥；完整需求、规模性能、公网服务、跨平台和安装发布仍待验收。

- [版本说明](CHANGELOG.md)
- [主干合并记录](docs/主干合并记录-2026-11-01.md)
- [四人成果整合与本轮验收](notebase-starter/docs/四人成果整合验收.md)
- [完整整合计划](docs/四人成果完整整合计划.md)
- [一二四整合与本轮验收](notebase-starter/docs/一二四整合验收.md)
- [成员三后续接入约定](notebase-starter/docs/成员三接入约定.md)
- [四人研发整合计划](docs/四人研发整合计划.md)
- [成员一开发计划](docs/成员一开发计划.md)
- [存储接口交接](notebase-starter/docs/存储初版交接.md)
- [索引基础交接](notebase-starter/docs/索引基础交接.md)
- [成员四交接与职责清单](notebase-starter/docs/成员四交接.md)
- [成员四验证记录](notebase-starter/docs/成员四验证记录.md)
- [操作说明](notebase-starter/docs/成员四操作说明.md)
- [成员二交接（保留原分支历史说明）](notebase-starter/docs/成员二交接.md)

在工程目录执行 `npm ci`、`npm run desktop` 启动桌面开发环境。Node、Rust MSVC、Visual Studio C++ 和 WebView2 要求详见工程 README。
