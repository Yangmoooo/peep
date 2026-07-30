# Changelog

本项目遵循 [Semantic Versioning](https://semver.org/)。

## [0.1.0] - 2026-07-30

### Added

- 初始 Rust CLI 与编码助手式终端界面。
- EPUB/TXT 文档 adapters、连续阅读视口、搜索和进度恢复。
- 非标准 EPUB nav/NCX 目录恢复、XML 优先的 XHTML 解析和章节目标校验。
- TXT 严格行首章节识别，以及卷、章和复合卷章层级。
- 基于终端默认色的低对比 composer、左对齐状态栏与可滚动信息浮窗。
- 冒号只聚焦命令 composer，不作为可见输入内容。
- 同步帧绘制和跨平台按键连发滚动。
- 正文和目录浮窗的按行、半页、整页及首尾导航，并显示目录选择位置。
- 左右方向键整页翻动。
- macOS、Linux 和 Windows CI 验证。
