# TV → PC 独立验证

生产转换位于 `src/tv.rs`，由 Rust 直接解析 TV 数据并写出 PC DefMap。包裹 TV 和裸 TV 都可输入；输出包含 `0xDEADFED4` 容器、准确的解压尺寸、完整 zlib 流及 PC schema `0xB393B4C0`。TV 的指针值和 `iPath`／`i2`／`i2Path` 被移除，8 个原始浮点 word、MISSING 和 image/font/text 字符串字节保留，不经过 UTF-8 重编码。

`tests/tv.rs` 覆盖非 UTF-8 与 NUL 字符串、非零 TV 指针、浮点位模式、MISSING、空动画和空轨，以及 PC 输入、截断、尺寸、标记、计数、字符串长度、checksum、尾随字节和拼接压缩流等负例。

真实文件检查器 [validate_tv_to_pc.py](validate_tv_to_pc.py) 使用旧工具独立解析和生成期望结果，要求两份完整解压 PC body 逐字节一致，并独立检查容器尺寸、zlib EOF、全部输入消费、PC schema 和轨道／帧／FPS 统计。压缩流的编码字节可以不同。另取一份真实文件的裸 TV body 通过 CLI 转换，以及 PC 输入、错误尺寸、截断、包裹尾随、裸 body 尾随五项 CLI 负控；失败不得产生输出文件。此脚本用于正确性验证，不用于性能计时。

最终发行 CLI SHA256 为 `0aa58004b65f9d551ac8cde4e96cfd80d8248358f4f66383e9cb0afa04ebdcf0`，体积 4,917,248 bytes。[本次报告](results/tv-to-pc.json) 的 175／175 个真实 TV 文件全部通过完整 PC body、schema 和统计检查，覆盖 3,075 条轨道、426,223 个帧记录；真实裸 TV 对照和五项 CLI 负控全部通过。报告中的 `completed`、`passed`、`binary_unchanged` 与 `baseline_unchanged` 均为 true。

旧基线可从 LawnMower 仓库的 `939af14be31aeca93fb842addbbbd0a262692ae2:tools/tv2pc_reanim.py` 导出，Git blob 的 SHA256 为 `465f1af93464b32970375250198bd6a8956ab145b68cf7066dbc444bde4f4bde`。实测使用的 Windows CRLF 备份 SHA256 为 `7df537e2a2d66657e42b0b12128016b44579f3915b758b084aa31d74056fa484`；二者只有换行差异。检查器记录原始 SHA，并要求 LF 归一化后与固定 Git blob 相符。在转换器源码目录运行以下命令，替换本机路径；以二进制方式保存 Git blob，避免 PowerShell 重定向改变文本编码：

```powershell
New-Item -ItemType Directory .qa -Force | Out-Null
python -c "from pathlib import Path; import subprocess; Path('.qa/tv2pc_reanim.py').write_bytes(subprocess.check_output(['git', '-C', 'C:/path/to/LawnMower', 'show', '939af14be31aeca93fb842addbbbd0a262692ae2:tools/tv2pc_reanim.py']))"
python benchmarks/validate_tv_to_pc.py `
  --rust C:/path/to/frozen-release/fla-reanim-cli.exe `
  --baseline .qa/tv2pc_reanim.py `
  --source-dir C:/path/to/TV/compiled/reanim `
  --output benchmarks/results/tv-to-pc.json
```

输入 TV 资源不在这里重新分发。报告记录每个输入的 SHA256、基线和 CLI SHA、模块与检查器 SHA、完整解压 body SHA、统计和负控结果。Python 仅用于开发验证；发行程序使用纯 Rust 模块。该检查验证格式转换和字节保留，游戏中的 TV 图片资源注册由使用方配置。
