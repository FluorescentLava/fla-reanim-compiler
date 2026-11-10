# Adobe Animate 实际渲染与编辑验证

使用 Adobe Animate 2024 打开最终 Rust CLI 生成的 `Motion-final.fla`，通过原生 JSFL API 查询时间轴、导出透明 PNG、另存文档并编辑起点。最终 CLI SHA256 为 `60BC3FFF5BA0B77423E2EC8E8C75442B4D0972798A0A8121F4F2925E3D8F1BF6`。

该测试包含一张 96×96 的透明位图、一个 41 帧的运动轨和一个动画标签轨。运动轨从 `(100,100)` 移动到 `(500,250)`，经典缓动为 50。Animate 识别到第 0 帧 `duration=40, tweenType=motion, tweenEasing=50`，第 40 帧为终点；运动轨只有两个关键帧。

| 帧 | 数值曲线的期望位置 | Adobe 导出图片的 alpha 包围盒 |
|---|---|---|
| 0 | (100, 100) | (104, 104, 191, 191) |
| 10 | (237.5, 151.5625) | (241, 155, 329, 243) |
| 20 | (350, 193.75) | (354, 197, 441, 285) |
| 30 | (437.5, 226.5625) | (441, 230, 529, 318) |
| 40 | (500, 250) | (504, 254, 591, 341) |

源图的 alpha 包围盒是 `(4,4,91,91)`。分数位置的包围盒因光栅覆盖而取整，结果与预期相差不到一个像素。中间帧符合缓动位置，区别于错误的匀速 `(300,175)`。导出的实际 PNG 和记录见 [results/animate/verification.json](results/animate/verification.json)。

Animate 另存后再用 Rust 正常编译，得到的 compiled SHA256 与源完全相同：`48bbf188958e0c8644da06f67dc53ad0058fa34664e5c7ebf3303b8df32ee1c3`。这次走正常编译，不依赖原始快照恢复。把第 0 帧的 x 从 100 改成 110 后另存，Rust 回编的第 0 帧 x 为 110，第 20 帧为 353.8，终点仍为 500；编辑没有被元数据吞掉。

该验证发现并修正了一个真实互操作问题：XFL 的正确属性是 `acceleration=-50`。此前写出的 `tweenEasing=50` 被 Animate 忽略，另存后还会被删除。`tweenEasing` 是 JSFL 的属性名，其符号与 XFL `acceleration` 相反。`tests/fixtures/adobe-classic-ease.fla` 是 Animate 自己保存的正样本，`tests/easing.rs` 直接读取其可编辑 XML 验证经典和增强编译。

复现时，在转换器根目录准备独立 `.qa/animate` 目录，把 `benchmarks/animate_verify.jsfl` 复制进去，并用 CLI 从上述 Adobe 测试样本编译、再逆向为该目录下的 `Motion-final.fla`：

```powershell
New-Item -ItemType Directory -Force .qa/animate
Copy-Item benchmarks/animate_verify.jsfl .qa/animate/
target/release/fla-reanim-cli.exe tests/fixtures/adobe-classic-ease.fla -o .qa/animate/Motion.reanim.compiled
target/release/fla-reanim-cli.exe .qa/animate/Motion.reanim.compiled --resource-root .qa/animate -o .qa/animate/Motion-final.fla
```

在 Animate 的“命令 → 运行命令”中运行复制的 JSFL，或用 Adobe 的 JSFL 文件关联打开，并选择“作为命令运行”。脚本只打开和修改这个测试文档，将 PNG、另存和编辑版写在同一测试目录。再次运行前请使用新的测试目录，避免另存覆盖对话框。随后把 `Motion-final-saved.fla` 和 `Motion-final-edited.fla` 编译到不同的新目录，以免两者导出的 `reanim/TILE.png` 发生输出冲突。

这个应用验证覆盖平移、缓动、位图透明度和保存编辑。它不能单独证明所有复杂旋转、倾斜和光栅化情况；更广的数值曲线误差与独立图像验证分别由 [TWEEN_QUALITY.md](TWEEN_QUALITY.md) 和全量语料检查提供。
