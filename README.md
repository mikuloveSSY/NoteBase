# NoteBase

当前软件版本为 **0.5.0，成员一增量与监视**。可运行工程位于 [notebase-starter](notebase-starter/README.md)，原始 0.1.0 ZIP 保留归档。

本版在文件存储闭环上增加库外 SQLite 索引、后台构建和索引状态显示。已接入中文检索；已接入增量监视；编辑自动保存等尚未实现。

- [版本说明](CHANGELOG.md)
- [成员一开发计划](docs/成员一开发计划.md)
- [存储接口交接](notebase-starter/docs/存储初版交接.md)
- [索引基础交接](notebase-starter/docs/索引基础交接.md)
- [本版验证记录](notebase-starter/docs/增量监视交接-v0.5.0.md)

在工程目录执行 `npm ci`、`npm run desktop` 启动桌面开发环境。Node、Rust MSVC、Visual Studio C++ 和 WebView2 要求详见工程 README。
