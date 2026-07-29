# Peep

Peep 是一个本地、离线的终端 EPUB/TXT 阅读器。界面保持为克制的编码助手会话风格：正文占据主体，底部只有命令输入框和状态栏。

## 特性

- EPUB spine、目录和容错恢复。
- UTF-8、UTF-8 BOM 与 GB18030 TXT。
- 连续滚动、Unicode 宽度感知折行和稳定阅读位置。
- Vim 风格导航、全文字面搜索和 EPUB 章节跳转。
- 自动保存进度；文件仅改名或移动时通过 BLAKE3 恢复。
- macOS、Linux 和 Windows Terminal。
- 完全离线，无模型调用、遥测或远程 EPUB 资源加载。

## 构建与运行

需要 Rust 1.88 或更新版本。

```bash
cargo build --release
cargo run -- path/to/book.epub
cargo run -- path/to/book.txt
```

也可以从本地源码安装：

```bash
cargo install --path .
peep path/to/book.epub
```

不带参数运行会恢复最近阅读的文件：

```bash
peep
```

默认启用鼠标滚轮。若终端或 tmux 的鼠标行为不合适，可以禁用：

```bash
peep --no-mouse book.txt
```

## 阅读操作

| 按键 | 行为 |
| --- | --- |
| `j` / `k`、`↑` / `↓` | 上下滚动一行 |
| `Ctrl-d` / `Ctrl-u` | 上下滚动半页 |
| `Space` / `b` | 上下滚动一页 |
| `g` / `G` | 文档开头/结尾 |
| `]` / `[` | EPUB 下一章/上一章 |
| `/text` | 搜索普通文本 |
| `n` / `N` | 下一个/上一个搜索结果 |
| `Ctrl-C` | 退出 |

底部输入框支持：

```text
:e <path>       打开文件
:toc            EPUB 目录
:goto <percent> 跳到百分比位置
:info           文件和恢复警告
:help           快捷键帮助
:q              退出
```

在信息和帮助浮窗中，可使用 `j/k`、方向键、PageUp/PageDown、Home/End 滚动。

启用鼠标捕获时，大多数终端使用 `Shift` + 拖动进行原生文本选择。

## 格式范围

Peep 面向普通 reflowable EPUB，不支持 DRM 和固定版式 EPUB。若 container、OPF、spine 或目录缺失，会在可安全判断时按文件和标题恢复，并通过 `:info` 显示 warning。

TXT 保留原始换行，当前不推断章节。解码后的纯文本在 100 MiB 内属于正式支持范围，更大文件尽力运行。

## 本地状态

阅读状态写入操作系统约定的用户状态目录，不会在书籍旁创建文件：

- macOS：`~/Library/Application Support/peep`
- Linux：`$XDG_STATE_HOME/peep`，未设置时通常为 `~/.local/state/peep`
- Windows：用户的 Local App Data 目录下的 `peep`

每本书使用独立的原子 JSON 记录。路径相同则直接恢复；路径变化但文件字节完全相同时，通过 BLAKE3 指纹恢复。

## 开发

```bash
cargo +nightly fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
```

## License

MIT
