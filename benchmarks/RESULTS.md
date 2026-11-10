# Rust 转换器最终测量与验收

2026-11-10，最终 CLI SHA-256 `91279932c35755c71fdae9f04af662578d7d9b943e9fe9c2f6d2f99ecf0b4005`。本轮包含原版舞台、JSFL 层规则、
嵌套矩阵、精确资源 ID/文件名和 Adobe 原始 DAT 修复。生产代码与开发验证
单独提交到 `FluorescentLava/fla-reanim-compiler`；LawnMower 只跟踪
`tools/fla_to_compiled` 外层发行文件，整个内层 `src` 是独立 Git 工程并被
LawnMower 忽略。

输出等价的 512 帧／32 骨骼增强场景加速 **4.22 倍**。
发行载荷从 **185.98 MiB** 降至 **10.75 MiB**，
减少 **94.22%**；ZIP 为 **4.71 MiB**。
经典转换修复后与旧版输出不同，下面保留实际耗时，不计算等价加速比。
此前 `process-comparison.json` 的 3.99–18.89 倍数字属于旧 SHA，不能替代本轮结果。

## 进程耗时和内存

相同机器、每个后端预热一次，AB/BA 交替各 7 次，以下为中位数。
耗时包含进程启动、解析、压缩和文件写出，缓存已预热。内存是每 2ms 采样
的进程树 working set 峰值，不表示整个系统的增量内存。旧正向与逆向基线
是 C#/.NET，增强基线是 Python。机器、逐轮数据、命令和全部输入 SHA 见
[原始测量](results/process-goty-fixed.json)。

| 场景 | 旧版耗时 | Rust 耗时 | 旧版峰值 | Rust 峰值 | 等价加速比 |
|---|---:|---:|---:|---:|---:|
| Chomper 正向 | 599.1 ms | 303.4 ms | 118.38 MiB | 23.56 MiB | — |
| Zombie_torch 正向 | 243.4 ms | 180.7 ms | 71.48 MiB | 17.05 MiB | — |
| Zombie 正向 | 2941.9 ms | 485.2 ms | 136.73 MiB | 34.74 MiB | — |
| Zombie_polevaulter 正向 | 2080.3 ms | 344.0 ms | 132.82 MiB | 27.14 MiB | — |
| SelectorScreen 正向 | 460.3 ms | 433.3 ms | 122.61 MiB | 37.37 MiB | — |
| Zombie_boss 逆向 | 1102.1 ms | 399.6 ms | 138.77 MiB | 44.34 MiB | — |
| Zombie_gargantuar 逆向 | 630.8 ms | 291.0 ms | 125.66 MiB | 22.08 MiB | — |
| 512 帧 × 32 骨骼增强编译 | 890.3 ms | 211.1 ms | 45.48 MiB | 41.40 MiB | 4.22× |

增强场景的经典 core 解压字节、RPLS 图和图片字节全部一致。它是确定性
合成负载，不代表所有实际骨骼项目。经典五个正向样本因补间矩阵、缓动、
层规则或多余 anim_full 的修复而与旧版不同；Rust 正向还会从当前库导出
实际可编辑贴图、资源映射和舞台 sidecar，额外 IO 已计入。两项逆向旧版
仍有 DAT 截断／缺失和像素错误，Rust 两项均通过独立位图与像素检查。
这些场景的 `speedup` 如实为 `null`。

资源查找改为在不同图片 ID 上执行，避免同一图片在每帧重复访问磁盘。
[优化等价检查](results/reverse-dedup-output-equivalence.json) 确认 157 个
逆向 FLA、2,345 个正常编译输出、151 个 Adobe 编译输出及4个真实用户样本
全部逐字节相同，FLA 的全部 ZIP 成员也相同。

## 正常编辑兼容性

[157 文件正常回编报告](results/editable-goty-final.json) 全部通过：移除
原始指纹后真正修改 DAT 的颜色和透明度，150次修改验证、1,863次 DAT 像素
比较、18次外部 mask 检查均通过。轨道顺序、帧数、显隐、动画范围、精灵格、
活动 image/font/text 和实际部署路径差异为零，快照恢复使用次数为零。
18次 mask 检查中4次对应实际 alpha 编辑。

[原游戏 C++ 加载器](results/game-loader-editable.json) 接受所有157个
正常回编输出，并逐个验证与源资源的轨道、引擎动作范围和可见帧资源路径
一致；全部输入和输出 SHA 绑定独立编辑报告。三项负控均被拒绝。
该检查不初始化 SDL、不构造图集、不渲染完整游戏。

[原 Adobe 位图出口](results/adobe-bitmap-export.json) 覆盖5个真实 FLA：
24张未压缩 DAT、106张 zlib DAT 的导出 RGBA 精确一致，3张直接 JPEG 的
最大 RGB 差为3，低于固定容差4，透明度全部精确。

[六份原版 FLA 对照](GOTY_FORWARD_VALIDATION.md) 的结构、资源和显隐
一致，仍有最大1px位置、0.3°角度、0.003缩放及0.005 alpha 参数残差。
独立源码缓动检查器已改为外层实例插值后组合内部矩阵，Chomper 123区间／
591采样及 Polevaulter 42区间／146采样全部通过，无遗漏。
舞台151项来源、关键帧压缩、容差和验收边界见
[GOTY_COMPATIBILITY.md](GOTY_COMPATIBILITY.md)。

此前 Adobe 实测覆盖41帧缓动、透明位图、保存和编辑，见
[ADOBE_VERIFICATION.md](ADOBE_VERIFICATION.md)。本次用户停止电脑操作后
未重新控制 Adobe；新增持久数据在最终 Adobe 另存中的保留情况尚未实测。
旧 `corpus-validation.json` 的157个快照恢复不作为正常编辑兼容性的证据。

## 发行体积

载荷为解压后 GUI、CLI、官方 Bootstrap、README、LICENSE 和依赖声明的
总和；ZIP、校验清单和本地源码另列，不通过把源码算进发行包改变比较。
系统 Windows App Runtime 的已安装体积不计入分发文件。

| 内容 | 字节 | MiB |
|---|---:|---:|
| 旧启动器 + runtime | 195,011,983 | 185.98 |
| Rust WinUI 3 GUI | 4,999,168 | 4.77 |
| Rust CLI | 4,872,192 | 4.65 |
| 完整原生发行载荷 | 11,272,808 | 10.75 |
| 发行 ZIP | 4,936,187 | 4.71 |

GUI SHA-256：`75110042ccb950d8b6932c79c3b82806eef0718f4c4cbf549a39f24f5943b1ae`。
ZIP SHA-256：`c4bdb5338512d362754ee1d153876d997af777c86d7ada1f7fff1c36d1cce0b5`。

生产转换不使用 C#、.NET 或 Python；GUI 保留 WinUI 3，使用系统安装的
Windows App Runtime，缺失时由官方 Bootstrap 显示获取提示。
本地79项 Rust 测试、格式检查、Clippy（警告视为错误）和原生 PE 导入检查
均通过。ZIP 的204个条目中没有源码、`.git`、QA 数据或捆绑 WinUI runtime。
性能结果不能等同于冷缓存、游戏帧率或 Animate UI 速度。
