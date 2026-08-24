# Peep

Peep 是一个本地、离线的终端 EPUB/TXT/Markdown 阅读器。界面保持为克制的编码助手会话风格：正文占据主体，底部只有命令输入框和状态栏。

## 特性

- EPUB spine、目录和容错恢复。
- UTF-8、UTF-8 BOM 与 GB18030 TXT。
- CommonMark/GFM Markdown，包括标题目录、列表、引用、代码、链接和表格。
- 连续滚动、Unicode 宽度感知折行和稳定阅读位置。
- Vim 风格导航、宽松/精确/正则搜索和目录跳转。
- 独立保存的书签与最近阅读列表。
- 目录、书签和最近阅读浮窗的实时宽松过滤。
- 可浏览当前目录并选择 EPUB、TXT 或 Markdown 文件。
- 命令和 `:e` 路径的 Tab 补全。
- 可即时切换并保存的自动、亮色和暗色主题。
- 可用方向键浏览的跨会话命令与搜索历史。
- 自动保存进度；文件仅改名或移动时通过 BLAKE3 恢复。
- macOS、Linux 和 Windows Terminal。
- 完全离线，无模型调用、遥测或远程文档资源加载。

## 安装与运行

可以从 [GitHub Releases][releases] 下载 Linux x86_64、Windows x86_64 或
macOS Apple Silicon 的预编译包。解压后将 `peep`（Windows 为 `peep.exe`）放入 `PATH`。

从源码构建需要 Rust 1.88 或更新版本：

```bash
cargo build --release
cargo run -- path/to/book.epub
cargo run -- path/to/book.txt
cargo run -- path/to/README.md
```

也可以从本地源码安装：

```bash
cargo install --path .
peep path/to/book.epub
```

或直接安装 Git tag 对应的版本：

```bash
cargo install --git https://github.com/Yangmoooo/peep --tag v0.4.1 --locked
```

不带参数运行会恢复最近阅读的文件：

```bash
peep
```

默认启用鼠标滚轮。若终端或 tmux 的鼠标行为不合适，可以禁用：

```bash
peep --no-mouse book.txt
```

默认主题会根据终端颜色自动选择，也可以仅为本次运行覆盖已保存的主题：

```bash
peep --theme light book.txt
peep --theme dark book.epub
```

## 阅读操作

| 按键 | 行为 |
| --- | --- |
| `j` / `k`、`↑` / `↓` | 上下滚动一行 |
| `Ctrl-d` / `Ctrl-u` | 上下滚动半页 |
| `Space` / `b`、`→` / `←`、`PageDown` / `PageUp` | 下/上滚动一页 |
| `g` / `G`、`Home` / `End` | 文档开头/结尾 |
| `]` / `[` | 下一个/上一个目录条目 |
| `/text` | 宽松搜索文本，忽略内部空白、标点、大小写和常见全半角差异 |
| `n` / `N` | 下一个/上一个搜索结果 |
| `Ctrl-C` | 退出 |

在命令或搜索输入框中，`↑` / `↓` 可以浏览各自的历史，并在越过最新记录时恢复输入前的草稿。

底部输入框支持：

```text
:e <path>       打开文件
:toc            目录
:mark [label]   在当前位置添加或更新书签
:marks          浏览当前书籍的书签
:recent         浏览最近阅读的文件
:exact <text>   精确搜索文本
:re <pattern>   使用 Rust 正则表达式搜索
:results        浏览最近一次搜索的结果
:goto <percent> 跳到百分比位置
:info           文件和恢复警告
:theme [name]   查看或切换 auto/light/dark 主题
:history clear  清除命令和搜索历史
:help           快捷键帮助
:q              退出
```

`:e` 也可以接收目录。使用 `peep <directory>` 或 `:e <directory>` 会打开当前目录下的文件选择器，
列出支持的 EPUB、TXT 和 Markdown 文件以及子目录；可用 `j/k`、翻页、`/` 过滤，按 `Enter` 打开或进入目录。
在命令输入框中按 `Tab` 可以补全命令和 `:e` 的路径，连续按 `Tab` 可轮换候选项。

所有浮窗都支持 `j/k`、方向键、`Ctrl-d/Ctrl-u`、`Space/b`、
`PageUp/PageDown` 和 `Home/End` 导航。目录、书签、最近阅读、文件选择器和搜索结果浮窗可用
`Enter` 跳转或打开，书签浮窗使用 `x` 删除所选书签；`Esc` 关闭浮窗。
在目录、书签、最近阅读和文件选择器浮窗中按 `/` 会聚焦底部输入框并实时过滤；`Enter` 或
`Esc` 返回列表，清空过滤词即可恢复完整列表。过滤使用与默认全文搜索相同的宽松匹配规则。

默认 `/text` 是宽松字面搜索，只容忍排版差异，不会跳过额外的正文字符或跨越换行。
使用 `:exact` 搜索必须完全一致的文本，使用 `:re` 执行正则搜索。命令和 `/text` 搜索词
分别保留最近 100 条；可用 `:history clear commands`、`:history clear searches` 或
`:history clear all` 单独清理。浮窗过滤词和搜索结果正文不会写入磁盘。

启用鼠标捕获时，大多数终端使用 `Shift` + 拖动进行原生文本选择。

## 格式范围

Peep 面向普通 reflowable EPUB，不支持 DRM 和固定版式 EPUB。若 container、OPF、spine 或目录缺失，会在可安全判断时按文件和标题恢复，并通过 `:info` 显示 warning。

TXT 保留原始换行，并以严格的行首规则识别 `第N章/回`、`第N卷/部/集` 和复合卷章结构；
普通正文中的内嵌编号不会被当作章节。解码后的纯文本在 100 MiB 内属于正式支持范围，
更大文件尽力运行。

Markdown 支持 `.md` 和 `.markdown`，标题会生成目录，段落会按终端宽度重新排版；支持基础行内样式、列表、任务列表、引用、代码块和 GFM 表格。链接会保留目标地址，图片显示为 alt text 与源路径。Peep 不下载远程资源，也不执行原始 HTML、脚本或终端控制序列。

## 本地状态

阅读状态写入操作系统约定的用户状态目录，不会在书籍旁创建文件：

- macOS：`~/Library/Application Support/peep`
- Linux：`$XDG_STATE_HOME/peep`，未设置时通常为 `~/.local/state/peep`
- Windows：用户的 Local App Data 目录下的 `peep`

每本书的进度和书签使用分开的原子 JSON 记录。路径相同则直接恢复；路径变化但文件字节完全相同时，
通过 BLAKE3 指纹恢复完整阅读状态。书签不会因为频繁保存阅读进度而被重写。主题偏好和输入历史也使用
各自独立、带 schema 的原子 JSON 记录，不会改变现有进度或书签格式。

## 开发

```bash
cargo +nightly fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
```

## License

MIT

[releases]: https://github.com/Yangmoooo/peep/releases
