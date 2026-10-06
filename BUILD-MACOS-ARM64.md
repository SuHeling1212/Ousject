# 一键生成 Apple 芯片版

在项目根目录运行：

```sh
./scripts/build-macos-arm64
```

脚本会自动准备编译工具并生成项目根目录下的 `ousject-macos-arm64`。第一次运行需要网络并会下载工具；后续运行会复用工具和已有编译结果。它会检查输出确实是 Apple 芯片（arm64）的 macOS 可执行文件后才更新成品。

在 Mac 终端中进入该文件所在目录后启动：

```sh
./ousject-macos-arm64
```

在项目目录里也可以把它当作原来的 `ousject` 命令使用，例如：

```sh
./ousject-macos-arm64 system-install ./system --local
./ousject-macos-arm64 run ./system/shell.px --local --steps 200000
```

脚本针对当前 Linux x86_64 环境生成 Apple 芯片版文件。生成的文件只面向 Apple 芯片 Mac。
