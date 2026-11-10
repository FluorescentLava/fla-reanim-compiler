# 补间重建质量审计

逆向默认使用 `editable` 补间容差；`--strict-tweens` 使用严格容差。原始 compiled 记录保存在 FLA 的校验元数据中，未编辑文件可精确恢复；本审计检查可编辑补间的数值重建质量，不以元数据回放作为补间正确性的证据。

| 参数 | strict | editable（逆向默认） |
|---|---:|---:|
| x、y 平移 | 0.05 px | 0.25 px |
| kx、ky 角度 | 0.02° | 0.1° |
| sx、sy 缩放 | 0.0005 | 0.002 |
| alpha | 0.001 | 0.004 |

这里是各参数的拟合误差，不能当作整张贴图的像素误差。图像越大，角度及缩放偏差引起的角点位移越大。正向重新编译还沿用经典格式的 0.1 px、0.1°、0.001 缩放及透明度量化；这部分量化不计入本表的补间拟合误差。

## 复现

`examples/tween_quality.rs` 直接调用当前 Rust 库读取 compiled、填充省略值、生成两种补间计划，再用 `Resources/resources.xml` 和实际 PNG/JPEG 尺寸计算矩阵角点误差。无需生成中间 JSON，也不需要 Python。以下命令在 `tools/fla_to_compiled/src` 目录运行，路径相对于仓库：

```powershell
cargo run --locked --release --example tween_quality -- `
  --resource-root ../../../Resources `
  --output benchmarks/results/tween-quality.jsonl `
  ../../../Resources/compiled/reanim/Zombie_boss.reanim.compiled `
  ../../../Resources/compiled/reanim/Zombie.reanim.compiled `
  ../../../Resources/compiled/reanim/Chomper.reanim.compiled `
  ../../../Resources/compiled/reanim/Zombie_polevaulter.reanim.compiled
```

也可传入 compiled 目录，对目录直属的全部 `.reanim.compiled` 审计；省略输入时使用资源根目录下的 `compiled/reanim`。`--subframes 16` 为默认采样密度，`--preset strict` 或 `--preset editable` 只生成一种结果。JSONL 记录每个输入的 SHA-256、容差、缺图清单和排除的区间数量；使用不同语料或不同图片尺寸时结果会相应改变。

## 检查范围

- 关键帧统计排除纯 `anim_*` 标记轨，保留其他轨道的全部原采样与输出关键帧。多帧补间要求 `duration > 1`，不会把每帧设置一个补间标签算作关键帧压缩。
- 拟合器对每个帧间区间检查二次残差的端点和内部导数零点。因此 `max_parameter_errors` 是接受区间的连续参数误差界限，不仅是整帧采样结果。
- `sampled_corner_max_px` 用引擎在相邻采样间的线性参数插值作参考，在每个帧间区间的 0、1/16、…、1 位置检查实际贴图四个角点。这个值是密集采样实测，不能单独证明连续最大值。
- `continuous_corner_bound_px` 是矩阵角点位移的保守连续上界。每轨采用 `hypot(ex,ey) + W*(esx + 2*max|sx|*sin(ekx/2)) + H*(esy + 2*max|sy|*sin(eky/2))`，角度使用弧度，W/H 使用该轨资源的最大真实宽高。它由已检查的连续参数误差推得，通常比实际角点位移大。
- 图片、字体、文本及 frame/visibility 切换必须断开，不纳入连续拟合误差。无法可靠表达的单帧大角度跳转、缩放翻转等列入 `unfitted_intervals`；隐藏、无图及缺图区间也分别计数，不以零误差代替缺失检查。
- 此工具检查数值补间模型和变换后的图像几何。它不运行 Adobe Animate，不比较 Adobe 实际插值的截图，也不验证图像解码、颜色、遮罩或光栅采样；这些由位图和应用验证分别覆盖。

## 本次真实资源结果

结果保存在 [results/tween-quality.jsonl](results/tween-quality.jsonl)。关键帧与角点结果如下，取到小数点后三位：

| 动画 | 原采样数 | strict 关键帧 | editable 关键帧 | editable 多帧补间 |
|---|---:|---:|---:|---:|
| Zombie_boss | 38,808 | 10,596 | 6,814 | 2,511 |
| Zombie | 11,088 | 4,661 | 1,992 | 1,345 |
| Chomper | 3,120 | 2,183 | 987 | 647 |
| Zombie_polevaulter | 3,933 | 2,816 | 1,607 | 864 |

| 动画 | strict 角点采样最大值 | editable 角点采样最大值 | editable 保守连续上界 |
|---|---:|---:|---:|
| Zombie_boss | 0.379 px | 1.033 px | 2.776 px |
| Zombie | 0.112 px | 0.366 px | 0.720 px |
| Chomper | 0.091 px | 0.367 px | 0.591 px |
| Zombie_polevaulter | 0.144 px | 0.366 px | 0.829 px |

Boss 最差位置来自 522×479 的身体部件，说明大图会放大角度与缩放误差。Zombie 有 2 个、Zombie_polevaulter 有 3 个未拟合转变；这些转变已排除在误差表之外，FLA 中保留切换，原始数据仍在元数据和 XML 中。严格模式使用更多关键帧，编辑模式在所列容差内减少关键帧。
