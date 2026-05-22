# FLA Reanim Compiler

> **内测提醒**
>
> 这个工具目前正在内测，还不稳定。它主要服务当前 PvZ reanim 工作流，生成的 `.reanim.compiled` 在正式使用前必须和原包效果对照，并在游戏内验证。
>
> 目前不要把它当作通用、稳定、可直接投入生产的 FLA 编译器。后续格式细节、转换行为和 UI 都可能继续变化。

FLA Reanim Compiler 是一个 WinUI 3 / Windows App SDK 桌面工具，用于把 Animate ZIP/XFL 格式 `.fla` 转成 PopCap 风格的 `.reanim.compiled`。

## 当前状态

- 仍在内测，不稳定。
- 输出兼容性还在和现有 `.reanim.compiled` 原包对照验证。
- 一些 Flash/Animate 时间轴行为目前是近似实现。
- 仍可能出现组件位置异常、插值不一致、额外绘制或运行时卡顿等问题。

## 使用

运行 WinUI 工具：

```powershell
dotnet run --project FlaReanimCompiler.csproj
```

把 `.fla` 拖进窗口，或通过“选择文件”按钮选择文件。生成的 `.reanim.compiled` 会写到源 `.fla` 所在目录。

也可以走命令行转换：

```powershell
FlaReanimCompiler.exe path\to\Anim.fla
```

## 支持的输入

- 现代 Animate/Flash ZIP/XFL 格式 `.fla`。
- 包含 `DOMDocument.xml` 的 FLA。
- 主时间轴动画；如果主时间轴为空，会回退到第一个非空库符号时间轴。
- `DOMSymbolInstance`、`DOMBitmapInstance`、`DOMGraphicInstance` 的放置数据。
- 位置、缩放、倾斜、透明度和 `firstFrame`。
- 基础 classic motion tween 采样。

转换器可以读取任意目录下的 FLA。转换过程不依赖特定游戏项目目录，也不会搜索项目资源文件夹。

## 输出格式

生成的 `.reanim.compiled` 使用游戏侧 Definition cache 布局：

- 文件头：`0xDEADFED4` 加未压缩长度。
- 文件体：zlib 压缩数据。
- 解压后：schema hash `0xB393B4C0` 加 packed `ReanimatorDefinition`。
- track 内写入 packed `ReanimatorTransform` 帧数据，包括位置、倾斜、缩放、透明度、image、font 和 text 字段。

图片名会根据 FLA 库项目和 bitmap 名称写成 `IMAGE_REANIM_*` 资源 ID。

## 已知限制

- 这不是完整 Flash 运行时。
- 高级时间轴行为不一定和 Animate 完全一致。
- 嵌套 symbol 只覆盖当前转换器支持的路径。
- 辅助轨道、空 symbol 和多层渲染 symbol 已按当前 reanim 工作流处理，但仍可能有边界问题。
- 生成文件需要在游戏内测试，并继续和原始 compiled 包对照。

## 构建

要求：

- Windows 10 19041 或更新版本。
- .NET 8 SDK。
- Windows App SDK / WinUI 3 构建支持。

Debug：

```powershell
dotnet build FlaReanimCompiler.csproj -c Debug
```

Release：

```powershell
dotnet build FlaReanimCompiler.csproj -c Release
```

发布：

```powershell
dotnet publish FlaReanimCompiler.csproj -c Release
```

项目发布设置会把运行时依赖打包进 `runtime/` 文件夹。

## 项目结构

```text
src/
  App.xaml
  MainWindow.xaml
  FlaToReanimConverter.cs
  ReanimCompiledWriter.cs
  FlaReanimCompiler.csproj
```

构建中间目录使用常规 `bin/` 和 `obj/`。

## License

MIT License. See [LICENSE](LICENSE).
