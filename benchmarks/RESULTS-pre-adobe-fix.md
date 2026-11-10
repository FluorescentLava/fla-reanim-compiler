# Adobe 补间属性修复前的测量记录

本轮数据对应下面记录的旧 SHA，仅保存测量过程。后续 Adobe 实际打开和另存验证发现补间属性应使用 `acceleration`，此前 `tweenEasing` 会被忽略；因此本报告不代表最终功能验收。最终发布结果需要在该修复后重新测量。

2026-11-10 在 Windows 11 x64、16 个逻辑处理器、约 31.4 GiB 内存的同一台机器上测量。每个场景分别预热一次，再按 AB / BA 交替运行各 7 次，以下均为中位数。耗时包含进程启动和输入输出，内存为每 2ms 采样的整个子进程树驻留 working set 峰值。

| 场景 | 旧版耗时 | Rust 耗时 | 加速 | 旧版峰值 working set | Rust 峰值 working set |
|---|---:|---:|---:|---:|---:|
| Chomper，真实 FLA 正向 | 524.8 ms | 81.6 ms | 6.43× | 117.39 MiB | 19.40 MiB |
| Zombie_torch，真实 FLA 正向 | 230.7 ms | 56.3 ms | 4.10× | 71.53 MiB | 12.65 MiB |
| Zombie，真实 FLA 正向 | 2705.2 ms | 138.0 ms | 19.61× | 136.48 MiB | 28.28 MiB |
| Zombie_polevaulter，真实 FLA 正向 | 1905.6 ms | 103.5 ms | 18.42× | 132.62 MiB | 23.79 MiB |
| SelectorScreen，真实 FLA 正向 | 419.3 ms | 75.9 ms | 5.52× | 122.73 MiB | 22.84 MiB |
| 512 帧 × 32 骨骼，增强编译 | 792.9 ms | 186.6 ms | 4.25× | 45.83 MiB | 38.73 MiB |

前五行基线为原 C# / .NET 程序，增强编译行基线为原 Python 后端。五个真实 FLA 的输出 compiled 解压后逐字节一致；增强样本的经典 core 也逐字节一致，RPLS JSON 逐字段相等，部件 PNG 文件内容一致。真实正向场景峰值 working set 降低 79.3%–83.5%；增强合成场景降低 15.5%。这些数据包含启动成本，不能直接等同于纯计算循环的加速比，也不代表任意动画都能获得相同提升。

旧版逆向会生成损坏的 DAT 或错误的颜色和透明度，因此下面只给出原始耗时，不计算成功转换的加速比：

| 逆向场景 | 旧版退出耗时，输出损坏 | Rust 完整转换耗时 | Rust 峰值 working set | Rust 验证 |
|---|---:|---:|---:|---|
| Zombie_boss | 913.5 ms | 363.1 ms | 41.68 MiB | 36 张内嵌位图、1,345,253 个源图像素通过验证；未编辑回编逐字节一致 |
| Zombie_gargantuar | 574.9 ms | 257.5 ms | 19.42 MiB | 34 张内嵌位图、85,005 个源图像素通过验证；未编辑回编逐字节一致 |

逆向像素检查采用独立 Pillow 读取真实源图片及引擎的 alpha mask，再与 Adobe 实际使用的预乘 ARGB DAT 比较。PNG 要求精确相等；JPEG 只允许 RGB 最大 4 级解码舍入差异，alpha 仍精确相等，并记录逐图实际偏差。此轮全部 157 个仓库动画的额外验证见 [corpus-pre-adobe-fix.json](results/corpus-pre-adobe-fix.json)：157/157 成功恢复 compiled 原始字节，1,834 次 PNG 比较精确一致，29 次 JPEG 比较的最大偏差为 3，没有缺图；这些结构和像素检查没有代替后续 Adobe 原生补间解释的实测。

测量用 Rust CLI 为 4,772,864 字节（4.55 MiB），SHA256：

```text
3c2f398f3294ee2096dd3901bb203f0d616ef668337e1f87e7c92bae7692228c
```

这仅是性能测试所用 CLI 的体积。完整 GUI + CLI + 官方 Bootstrap + 许可证发行包的实际大小由 `Packaging/BuildRelease.ps1` 生成的 `release-manifest.json` 记录。旧版测量依赖的 runtime 目录为 194,829,539 字节（185.80 MiB），另有 282,112 字节启动器。

原始每轮 wall time、进程树内存、根进程精确生命周期峰值、退出状态、输出 SHA256、源文件 SHA256、基线实现 DLL / Python 脚本 SHA256 和运行命令均保存在 [process-pre-adobe-fix.json](results/process-pre-adobe-fix.json)。[README.md](README.md) 说明基线历史恢复和复现方法。补间质量和 Adobe 属性修复需要以最终版本报告为准。
