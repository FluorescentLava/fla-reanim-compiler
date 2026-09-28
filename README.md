# FLA Reanim Compiler

> **内测提醒**
>
> 这个工具目前正在内测，还不稳定。它主要服务当前 PvZ reanim 工作流，生成的 `.reanim.compiled` 在正式使用前必须和原包效果对照，并在游戏内验证。
>
> 目前不要把它当作通用、稳定、可直接投入生产的 FLA 编译器。后续格式细节、转换行为和 UI 都可能继续变化。

FLA Reanim Compiler 是一个 WinUI 3 / Windows App SDK 桌面工具，用于在 Animate ZIP/XFL 格式 `.fla` 与 PopCap 风格 `.reanim.compiled` 之间双向转换。

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

把 `.fla` 或 `.reanim.compiled` 拖进窗口，或通过“选择文件”按钮选择。工具按扩展名自动判断方向，输出写到源文件所在目录。

命令行：

```powershell
FlaReanimCompiler.exe path\to\Anim.fla                        # 正向编译
FlaReanimCompiler.exe compiled\reanim\Blover.reanim.compiled  # 逆向为 FLA
FlaReanimCompiler.exe --inspect Anim.reanim.compiled          # 只打印内容
FlaReanimCompiler.exe --verify  Anim.reanim.compiled          # 逆向 + 回环校验
FlaReanimCompiler.exe --link    Anim.reanim.compiled          # 链接贴图，不内嵌
```

## 支持的输入

### 正向

- 现代 Animate/Flash ZIP/XFL 格式 `.fla`。
- 包含 `DOMDocument.xml` 的 FLA。
- 主时间轴动画；如果主时间轴为空，会回退到第一个非空库符号时间轴。
- `DOMSymbolInstance`、`DOMBitmapInstance`、`DOMGraphicInstance` 的放置数据。
- 位置、缩放、倾斜、透明度和 `firstFrame`。
- 基础 classic motion tween 采样。
- `libraryItemName="locator"` 的空符号实例（保留为纯变换占位轨道）。

### 逆向

- 任意 `.reanim.compiled`（校验 cookie 与 reanim schema hash）。
- 位图轨道、locator 轨道（有变换无贴图）、纯 `anim_` 标记轨道。
- 贴图按 `resources.xml` 的 `idprefix` 与 `reanim/`、`images/`、`particles/` 目录解析。

## 输出格式

### `.reanim.compiled`

使用游戏侧 Definition cache 布局：

- 文件头：`0xDEADFED4` 加未压缩长度。
- 文件体：zlib 压缩数据。
- 解压后：schema hash `0xB393B4C0` 加 packed `ReanimatorDefinition`。
- track 内写入 packed `ReanimatorTransform` 帧数据，包括位置、倾斜、缩放、透明度、image、font 和 text 字段。
- 图片名根据 FLA 库项目和 bitmap 名称写成 `IMAGE_REANIM_*` 资源 ID。

### `.fla`（逆向）

标准 Animate ZIP/XFL 文档，包含 `DOMDocument.xml`、内嵌 `images/`，以及 locator 所需的最小 `LIBRARY/locator.xml`。

### `.reanim.xml`（逆向侧车）

引擎自身的 reanim XML 表示，字段与 compiled 完全一致（含 `-10000` 占位符、attacher 字符串、字体、文本），是**无损**输出。

## 已知限制

- 这不是完整 Flash 运行时。
- 高级时间轴行为不一定和 Animate 完全一致。
- 嵌套 symbol 只覆盖当前转换器支持的路径。
- 辅助轨道、空 symbol 和多层渲染 symbol 已按当前 reanim 工作流处理，但仍可能有边界问题。
- 逆向无法恢复补间曲线与嵌套符号层级，详见上级 README 的「无法保真的部分」。
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

离线构建：仓库内附带 `NuGet.config`，在所有包已存在于本机 global packages 时清空远端源即可离线构建。

## 项目结构

```text
src/
  App.xaml                         应用入口，命令行分发
  MainWindow.xaml                  拖放 UI
  FlaToReanimConverter.cs          正向：FLA -> compiled
  CompiledDefinitionFormat.cs      compiled 容器格式常量与 schema CRC 复算
  ReanimCompiledReader.cs          逆向：compiled 读取 + 引擎语义辅助
  ImageResourceCatalog.cs          资源 id <-> 文件解析、图片尺寸
  ReanimToFlaConverter.cs          逆向：compiled -> FLA
  ReanimXmlWriter.cs               逆向：无损 .reanim.xml
  ReanimRoundTripVerifier.cs       回环校验
  ReanimCompiledWriter.cs          正向写出
  FlaReanimCompiler.csproj
```

构建中间目录使用常规 `bin/` 和 `obj/`。

## License

MIT License. See [LICENSE](LICENSE).
