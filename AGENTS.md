# 工程与发布目录

本目录是独立的 `FluorescentLava/fla-reanim-compiler` Git 工程。部署在
LawnMower 中时，工程位于 `tools/fla_to_compiled/src`，Rust 源文件自然位于
`tools/fla_to_compiled/src/src`。

源码、资源和打包脚本在本工程中单独提交。LawnMower 的 Git 必须
忽略整个 `tools/fla_to_compiled/src/`，只跟踪外层 `tools/fla_to_compiled`
中的最终发布文件，ZIP 作为 GitHub Release 附件发布。不要将内层工程作为
LawnMower 的普通目录或 submodule 提交。

`Packaging/BuildRelease.ps1` 默认把发行文件写到工程父目录；所有打包操作
只能选择本次生成的发布文件，不得把同级 `src`、`.git` 或缓存
装进 ZIP。显式 `-DestinationDirectory` 可指定其他发行输出目录。

程序使用纯 Rust 和 WinUI 3，生产转换不依赖 .NET 或 Python，WinUI 使用
系统安装的 Windows App Runtime。
