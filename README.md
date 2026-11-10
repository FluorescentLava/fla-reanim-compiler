# FLA Reanim Compiler

纯 Rust 的双向动画转换器，界面使用原生 WinUI 3。经典编译、增强编译、逆向、位图解码和矢量栅格化直接运行在同一程序中。

双击 `FlaReanimCompiler.exe`，拖入多个 `.fla` / `.reanim.compiled`，或点击“选择文件”。转换后的文件写在输入旁边；已有输出不会自动覆盖。命令行可通过 `--output` 指定新路径，或显式 `--force` 替换输出。

```powershell
fla-reanim-cli.exe Character.fla
fla-reanim-cli.exe --plus Character.fla
fla-reanim-cli.exe Zombie_boss.reanim.compiled --resource-root C:\Game\Resources --output C:\Work\Zombie_boss.fla
fla-reanim-cli.exe --strict-tweens Character.reanim.compiled --output C:\Work\Character-strict.fla
fla-reanim-cli.exe --stage 320x180 Custom.reanim.compiled --output C:\Work\Custom.fla
fla-reanim-cli.exe --inspect Character.reanim.compiled
fla-reanim-cli.exe --verify Character.reanim.compiled --resource-root C:\Game\Resources
```

程序不需要 .NET、Python、resvg-py 或 VC++ Redistributable。GUI 使用系统安装的 **x64 Windows App Runtime 2.3.1 或兼容的更新 2.x 版本**，发行包只附微软官方的约 385 KiB bootstrap DLL。Windows 不保证预装此 runtime；缺少时，官方引导程序显示获取 runtime 的提示，用户完成安装后重新打开。提示不会静默安装。命令行转换在 WinUI 初始化前执行，可以在没有 Windows App Runtime 的环境使用。

经典模式输出游戏的 `0xDEADFED4` + zlib + `0xB393B4C0` DefMap 格式，支持原版单 run 静态文字、字体 URL 覆盖和 `attacher__` 指令。增强模式保留文件夹、DOMGroup、嵌套符号、父子骨骼、枢轴和 `betweenFrameMatrixList` 烘焙 IK 姿态，并把图片写在 compiled 旁边；游戏由 `ReanimPlusAnimation` 读取兼容的 RPLS v1 扩展。IK 或实际可见矢量自动使用增强模式。纯色矢量及描边由 Rust resvg 栅格化；增强模式的文本、遮罩、渐变和缺少烘焙姿态的 IK 继续给出具体错误。

输入支持 Adobe Animate 的 ZIP/XFL FLA 和 XFL 目录，兼容已验证的 ZIP 目录长度多计 54 字节问题。老 Flash 的 OLE 二进制 FLA 需要先用 Animate 另存为 ZIP/XFL。

逆向读取 `Resources/resources.xml` 的资源 ID、图片路径和精灵图网格，支持 PNG/JPEG 以及游戏分离的 alpha 遮罩。优先传入 `--resource-root`；在项目目录内也会向上查找 Resources。Animate 位图支持未压缩 ARGB、分块的单一 zlib 流和直接保存的 JPEG；输出 DAT 使用实际的预乘 ARGB 像素布局。普通位图通过 graphic symbol 承载 alpha 和运动补间。

舞台使用原点 `(0,0)`，不会因为舞台尺寸改变而平移贴图。compiled 不包含原始舞台宽高；程序附带从 GOTY 原版 151 个 FLA 提取的尺寸表，并与 Adobe 保存的文档及 JSFL 测量交叉核对。自定义尺寸可用 `--stage WIDTHxHEIGHT` 指定。正向编译另外写出 `.reanim.authoring.json`，保留当前 FLA 的舞台尺寸；逆向优先读取它，再查原版尺寸表。没有尺寸记录的自定义 compiled 会明确报告使用 800×600 默认值。

`anim_` 与普通图层一样，以该帧是否有一个元素决定 `f=0/-1`。逆向的活动标记帧含真实的空 locator 实例，停止帧为空；不再用帧标签猜测动作范围，也不会凭空添加 `anim_full`。`anim_hair` 等带贴图图层仍正常绘制。导出层序为从底到顶，`_` 开头的辅助层跳过（`_ground` 除外）；编辑器的隐藏眼睛和帧标签不决定游戏动作。补间先计算外层实例矩阵，再组合符号内部位图偏移，避免旋转子图被错误拉成直线。

逆向会重建可编辑的运动补间，按数值误差约束拟合直线与经典 easing 曲线；贴图、精灵帧和显隐切换处分段，无法拟合的运动保留必要关键帧。原始手工关键帧和曲线已经被 compiled 采样，无法辨认其原始作者操作；重建的补间以匹配采样结果和帧间运动为目标。

默认优先减少关键帧，各参数容差为位置 0.25px、角度 0.1°、缩放 0.002、alpha 0.004。勾选“精确采样”或传入 `--strict-tweens` 后，分别收紧至 0.05px、0.02°、0.0005、0.001。这些是参数误差，贴图角点的实际位移还取决于图片尺寸。详见 [补间质量审计](benchmarks/TWEEN_QUALITY.md)。XFL 使用 Adobe 实际识别的 `acceleration` 属性，其符号与 JSFL 的 `tweenEasing` 相反；正向、增强与逆向一致处理，已用 Animate 实际渲染和编辑保存验证。

图片始终内嵌为可编辑 DAT。旧 `--link` 选项会明确报错；当前不生成依赖外部路径的链接位图 FLA。

生成的 FLA 同时保存原始 compiled 和全部可编辑 ZIP 成员的 SHA256。未编辑时重新编译精确保留全部字段；任何 XML、矩阵或图片编辑都会使此恢复记录失效，转入正常编译。不会用原始快照吞掉编辑。正常经典编译从当前 DAT 导出 PNG 和 `.assets.json`，包括 Adobe 另存后丢失恢复元数据的 FLA。图片沿用资源表中的实际 `reanim` / `images` 路径，并同步更新独立 alpha 遮罩；这些图片需要与 compiled 一并更新到游戏。找不到资源表时使用 `reanim` 路径，请通过 `--resource-root` 指定目标游戏的 Resources。增强 FLA 的未编辑恢复会同时还原外部图片。

`--verify` 检查未编辑文档的原始数据恢复；它不能代替图像、补间或 Adobe 渲染检查。原版舞台、`anim_`、正常编辑回编和游戏 C++ 加载器的验收见 [GOTY 兼容性报告](benchmarks/GOTY_COMPATIBILITY.md)。独立像素、数值曲线、真实 Adobe 渲染及性能报告分别保存在 `benchmarks` 中。

逆向还会写出保留原始字段的 `.reanim.xml`；增强资源另存完整 `.reanim-plus.json`。增强格式在 FLA 中以世界姿态重建可编辑运动，原有骨骼图另保存在 JSON 中。经典 attacher 指令以命名符号重建，字体和文本以可编辑静态文字重建；预览字体取决于本机安装情况，编译使用 URL 中保存的游戏字体 ID。多字体 run 和输入文字不支持时会明确报错。

源码位于独立仓库 [FluorescentLava/fla-reanim-compiler](https://github.com/FluorescentLava/fla-reanim-compiler)。在 LawnMower 游戏目录中采用下面的层级：

```text
gameRoot/tools/fla_to_compiled/
  FlaReanimCompiler.exe
  fla-reanim-cli.exe
  Microsoft.WindowsAppRuntime.Bootstrap.dll
  README.md / LICENSE / Licenses/
  FLA-Reanim-Compiler-win-x64.zip
  release-manifest.json
  src/                         独立源码仓库根目录
    .git/
    Cargo.toml / Cargo.lock
    Packaging/ / benchmarks/ / tests/
    src/                       Rust 代码
```

游戏仓库仅跟踪外层发行文件，并忽略 `tools/fla_to_compiled/src/`。转换器的 Rust 源码、QA、构建配置和提交历史由内层独立 Git 仓库管理。查看或提交转换器源码时，在内层 `src` 执行 Git 命令；游戏仓库的操作在 `gameRoot` 执行。

构建需要 Rust stable MSVC 和 Visual Studio C++ Build Tools；不需要 .NET SDK。在独立源码仓库根目录运行：

```powershell
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
pwsh -File Packaging/BuildRelease.ps1
```

默认发布到源码仓库的父目录，也就是游戏中的 `tools/fla_to_compiled`。EXE、bootstrap DLL、精简发行 README、许可证、ZIP 和 `release-manifest.json` 都位于该输出目录；源码 README 与 QA 留在内层仓库。打包时仅枚举独立临时 staging 中的发行文件，输出目录中的 `src`、`.git` 和 QA 不会进入 ZIP。清单记录源码仓库、提交、工作区是否有改动以及二进制 SHA-256；从不含 Git 信息的源码包构建时，提交和工作区状态为 `null`。

可显式覆盖输出目录；相对路径从源码仓库根目录解析，CI 使用同样的命令：

```powershell
pwsh -File Packaging/BuildRelease.ps1 -DestinationDirectory dist/win-x64
```

此时 ZIP 与清单也在 `dist/win-x64` 内。已有 Release 缓存可加 `-SkipBuild`，缓存位置沿用 `CARGO_TARGET_DIR`，未设置时使用仓库内 `target`。脚本验证 x64 原生 PE、无 CLR/.NET apphost、无外置 C/C++ runtime 导入和无捆绑 WinUI runtime，并包含依赖许可证。性能与兼容性实测、旧版恢复方法见 [benchmarks/README.md](benchmarks/README.md)。Python 只用于独立测试和历史基线，发布程序不会调用它。
