# FLA Reanim Compiler

纯 Rust 的 WinUI 3 动画转换器，支持 `.fla` / XFL 与 `.reanim.compiled` 双向转换。

双击 `FlaReanimCompiler.exe`，拖入文件或点击“选择文件”。结果写在输入旁边，已有输出不会自动覆盖。逆向时可选择游戏的 `Resources` 目录，程序会读取资源表、图片和分离的 alpha 遮罩，并生成内嵌图片的可编辑 FLA。

发行包无需 .NET、Python 或 VC++ Redistributable。GUI 使用系统安装的 **x64 Windows App Runtime 2.3.1 或兼容的更新 2.x 版本**；包内只包含微软官方 bootstrap DLL。缺少 runtime 时会显示获取提示，完成安装后重新打开程序。CLI 不需要 Windows App Runtime。

命令行示例：

```powershell
.\fla-reanim-cli.exe Character.fla
.\fla-reanim-cli.exe --plus Character.fla
.\fla-reanim-cli.exe Character.reanim.compiled --resource-root C:\Game\Resources --output C:\Work\Character.fla
.\fla-reanim-cli.exe --strict-tweens Character.reanim.compiled --output C:\Work\Character-strict.fla
```

默认优先减少关键帧；“精确采样”或 `--strict-tweens` 可收紧运动误差。compiled 已丢失原始手工关键帧，逆向会根据采样重建补间。老 Flash 的 OLE 二进制 FLA 需先用 Animate 另存为 ZIP/XFL。

源码、构建说明及验证报告：[FluorescentLava/fla-reanim-compiler](https://github.com/FluorescentLava/fla-reanim-compiler)。发行文件的大小、SHA-256 和源码版本记录在同目录 `release-manifest.json`；主程序及依赖许可证见 `LICENSE` 和 `Licenses`。
