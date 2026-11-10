# 原生 Rust 转换器性能与兼容性验证

原版格式修复后的主要兼容性证据见 [GOTY_COMPATIBILITY.md](GOTY_COMPATIBILITY.md)：
包含原版舞台和 JSFL 规则、157 个资源的编辑后正常回编，以及游戏原 C++
加载器核心验收。`results/corpus-validation.json` 是此前版本的未编辑快照
恢复与像素验证，不能代替正常编译兼容性检查。性能报告都绑定实际 CLI SHA；
旧 `process-comparison.json` 的测量不代表后续修复版。

`compare.py` 比较完整命令行进程的运行时间和内存。测试脚本需要 Python、psutil 和 Pillow；发布的转换器不依赖或启动它们。

旧版本基线来自 `Project_LawnMower` 的提交 `4af1489f0327534bab48edd3ac3e5026291c3ecd`。该提交跟踪的 EXE、实际实现 DLL 和 Python 脚本与测量前工作目录一致，所有 SHA256 都记录在 `results/process-comparison.json`。清理旧版后，测试脚本仍可通过 `git archive` 恢复基线到 `.obj/fla-native-benchmark/historical-baseline`，不会切换分支或修改生产文件。

```powershell
python -m pip install psutil Pillow
cargo build --manifest-path tools/fla_to_compiled/src/Cargo.toml --release --bin fla-reanim-cli
python tools/fla_to_compiled/src/benchmarks/compare.py `
  --project-root . `
  --rust tools/fla_to_compiled/fla-reanim-cli.exe `
  --baseline-revision 4af1489f0327534bab48edd3ac3e5026291c3ecd `
  --sample-dir ../寄存
```

真实正向样本是外部提供的 Animate ZIP/XFL FLA，未在此报告中重新分发。文件名、大小与 SHA256 标识每一个输入。没有这些私有样本时，可用 `--only plus_bones_512x32 reverse_Zombie_boss reverse_Zombie_gargantuar` 运行仓库资源和脚本生成的骨骼样本。脚本只把源文件复制到 `.obj` 后转换，不覆盖原始动画。

逆向测量使用默认的内嵌贴图模式。`--link` 当前会明确报错并解释不支持的原因，不会以成功状态输出不能正常显示的链接 FLA；它不在性能对比范围内。

每个场景先分别预热一次，然后交替以 AB、BA 顺序测量各 7 次并报告中位数。耗时包括进程启动、解析、计算、压缩和磁盘写出，文件系统缓存已预热。内存记录每 2ms 采样的整个子进程树当前 working set 峰值，同时保留各进程已观察到的 Windows 生命周期峰值 working set 之和；根进程还会在退出后通过保留的进程句柄查询精确生命周期峰值。采样可能漏掉极短峰值，各进程峰值之和也不表示同一时刻的精确总量。

性能结果必须通过以下正确性检查才会计算加速比：

- 正向：独立 Python 读取器精确消费整个解压后的 compiled，比较轨道、帧数、FPS、全部字符串和展开后的数值。角度按整圈等价处理，最大数值容差为 `1e-3`，同时记录解压后字节是否完全一致。
- 增强编译：除经典 core 检查外，RPLS JSON 逐字段比较，部件图片比较解码后的 RGBA。测试包含 512 帧、32 个骨骼、父子关系、枢轴、烘焙位姿和位图图片。它是确定性合成负载，不代表所有实际项目。
- 逆向：解析输出的全部 XML、独立解码每张 Animate DAT，按真实源图片和引擎使用的蓝通道 alpha mask 合并像素，逐像素比较 Adobe 所需的预乘 ARGB。PNG 的各通道必须精确相等；原生 JPEG 解码器与 Pillow/libjpeg 允许最多 4 级 RGB 舍入差异，并单列实际差值，alpha 仍要求精确相等。验证曾发现 zune-jpeg 在一张图上存在 13 级偏差，因此实现改用了纯 Rust 的 jpeg-decoder；没有通过上调校验阈值掩盖此问题。反预乘造成的低透明度量化误差不误报为源图损坏。另检查 Rust 的未编辑 FLA 能精确恢复原 compiled。

旧版进程即使返回成功，只要 FLA 位图或像素错误，也只记录故障和耗时，不把错误输出当成成功转换计算加速比。Rust 的逆向文件额外保存完整原始字段和可编辑文档哈希；这部分工作包含在 Rust 的测量中。

补间另有实际 XFL 回归验证：101 帧线性运动和 41 帧二次缓动可重建为两个关键帧，同时检查真实端点矩阵和 Adobe 使用的 `acceleration` 属性。Adobe 实测曾抓到 `tweenEasing` 会被忽略、另存时被删除的缺陷，因此自身正向回编和元数据还原不能代替 Adobe 原生解释的验收。测试还覆盖图片、可见性和帧号切换断开，多圈旋转和两个轴跨越 ±180°，以及“整数帧吻合、子帧却偏移 0.625px”的错误拟合拒绝。拟合检查同时覆盖子帧；默认编辑模式的位置、角度、缩放、alpha 参数域误差限制分别为 0.25px、0.1°、0.002、0.004，严格模式分别为 0.05px、0.02°、0.0005、0.001。

参数域限制不代表整张贴图的像素位移上界。真实贴图四角按每 1/16 帧采样的结果、连续保守界和原生质量测量工具的复现命令见 [TWEEN_QUALITY.md](TWEEN_QUALITY.md)。离散切换和未拟合区间另行处理。

这些拟合限制不等于整个“逆向再正向”的数值误差上界：经典正向仍沿用 0.1px / 0.1° / 0.001 的量化网格，再次编译可能另有最多半格的量化偏差；报告把补间重建误差与正向量化分别验证和说明。

源码缓动还由独立 Python 检查器验证：它先按原始 XFL 的外层实例、注册点和 `acceleration` 插值，再组合静态子符号矩阵，并处理相同图片更换包装符号的注册坐标转换，然后比较 compiled 展开的参数。Chomper 全部 123 个缓动区间、591 个帧采样和 Polevaulter 全部 42 个区间、146 个帧采样均通过。最大位置偏差约 0.0500031px，符合 0.1px 网格的半格量化及浮点舍入。旧版忽略这些真实缓动，所以这两项只记录原始耗时，不计算等价工作加速比。

在运行上面的完整基准后，可从项目根目录复现独立缓动检查；两条命令的结果分别对应 [Chomper](results/source-easing-Chomper.json) 和 [Polevaulter](results/source-easing-Zombie_polevaulter.json)：

```powershell
python tools/fla_to_compiled/src/benchmarks/validate_source_easing.py `
  --source ../寄存/Chomper.fla `
  --compiled .obj/goty-compat-benchmark/forward_Chomper/rust/Chomper.reanim.compiled `
  --output tools/fla_to_compiled/src/benchmarks/results/source-easing-Chomper.json
python tools/fla_to_compiled/src/benchmarks/validate_source_easing.py `
  --source ../寄存/Zombie_polevaulter.fla `
  --compiled .obj/goty-compat-benchmark/forward_Zombie_polevaulter/rust/Zombie_polevaulter.reanim.compiled `
  --output tools/fla_to_compiled/src/benchmarks/results/source-easing-Zombie_polevaulter.json
```

实际 Adobe Animate 2024 打开、查询关键帧、导出 PNG、另存并编辑的验收记录和复现方法见 [ADOBE_VERIFICATION.md](ADOBE_VERIFICATION.md)。该原生应用检查验证了两个关键帧的 41 帧缓动、透明位图显示、正常回编和编辑生效，不依赖未编辑快照还原代替 Adobe 的真实解释。

最终结果表和发行体积见 [RESULTS.md](RESULTS.md)，本轮原始测量见 [process-goty-fixed.json](results/process-goty-fixed.json)，157 个动画的编辑后正常回编检查见 [editable-goty-final.json](results/editable-goty-final.json)，原游戏加载器记录见 [game-loader-editable.json](results/game-loader-editable.json)。旧 [corpus-validation.json](results/corpus-validation.json) 仅作为未编辑恢复与像素验证的历史记录。这不是冷缓存、游戏内帧率或 Adobe Animate UI 启动速度测试。working set 包含被测进程当前驻留的私有页和共享映射页；其它非子进程的内存不计入，不能把这个指标当成整个系统的增量内存。
