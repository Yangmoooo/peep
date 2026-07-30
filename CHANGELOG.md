# Changelog

这里记录 Peep 面向使用者的重要变更。格式参考
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循
[Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

## [0.1.0] - 2026-07-30

### 新增

- **EPUB 与 TXT 阅读**：支持 EPUB、UTF-8、UTF-8 BOM 和 GB18030 文本，正文会按终端宽度自动折行并连续滚动。
- **快捷导航**：支持按行、半页、整页、章节和文档首尾移动，方向键、PageUp/PageDown 与 Vim 风格按键均可使用。
- **目录浏览**：可在目录浮窗中快速翻页、查看当前位置并跳转到所选章节。
- **搜索与进度恢复**：支持全文字面搜索，自动保存每本书的阅读位置；文件移动或改名后仍可恢复进度。
- **低干扰界面**：采用编码助手式终端界面，底部命令框可用于打开文件、跳转、查看目录和文档信息。
- **更广泛的 EPUB 兼容性**：能够从缺失、不完整或指向错误的目录信息中恢复可用章节，并避免跳转到书前或书末的目录文字。
- **TXT 章节识别**：自动识别常见的章、回、卷、部、集及复合卷章结构，同时尽量避免把正文中的编号误判为章节。
- **跨平台支持**：可运行于 macOS、Linux 和 Windows Terminal。

[Unreleased]: https://github.com/Yangmoooo/peep/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Yangmoooo/peep/releases/tag/v0.1.0
