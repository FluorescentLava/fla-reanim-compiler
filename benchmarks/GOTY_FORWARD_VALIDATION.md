正常正向编译对照使用两个独立来源：原版 GOTY 的 FLA 经 Adobe Animate 24
保存成 ZIP FLA，作为输入；同目录原版 `.reanim` 文本，作为期望输出。脚本
明确拒绝包含 `META-INF/reanim-source.compiled` 的输入，每次都执行正常原生
编译，不会通过恢复原始 compiled 验证自身。

可复现工具是 `audit_goty_forward.py`，只用于开发验证，使用 Python 标准库；
应用本体没有 Python 依赖。运行方式：

```powershell
cargo build --locked --release --bin fla-reanim-cli
python benchmarks/audit_goty_forward.py `
  --cli target/release/fla-reanim-cli.exe `
  --fla-dir "<存放六个 *-reference.fla 的目录>" `
  --reanim-dir "<原版 GOTY/reanim 目录>" `
  --resource-root "<游戏目录>/Resources" `
  --output benchmarks/results/goty-forward-audit.json
```

报告保存原始 FLA、原始 `.reanim` 和实际 CLI 的 SHA-256，来源说明使用相对
资源名称，不包含本机绝对路径。输入文件名为 Blover、Chomper、Zombie、
Zombie_boss、Zombie_polevaulter、SelectorScreen 加上 `-reference.fla`。

当前报告由迁移后的独立工程中最终冻结的 Release CLI 重新运行生成，大小为
4,872,192 bytes，SHA-256 为
`91279932C35755C71FDAE9F04AF662578D7D9B943E9FE9C2F6D2F99ECF0B4005`。
运行时显式传入当前游戏的 `Resources`，报告另记录 `resources.xml` 的 SHA-256。

本次六例的轨道数、轨道顺序、总帧数、FPS、每帧显隐，以及可见帧的图片、
字体和文本引用均与原版 `.reanim` 一致。下表同时报告仍存在的数值差异，
不会用结构匹配代替运动数值比较。

| 输入 | 轨道数 | 总帧数 | 最大 x 差异 | 最大 y 差异 |
|---|---:|---:|---:|---:|
| Blover | 11 | 62 | 0.4 px | 0.4 px |
| Chomper | 30 | 94 | 0.4 px | 0.3 px |
| Zombie | 42 | 504 | 0.4 px | 0.4 px |
| Zombie_boss | 72 | 792 | 0.7 px | 0.8 px |
| Zombie_polevaulter | 28 | 193 | 0.5 px | 1.0 px |
| SelectorScreen | 48 | 706 | 0.7 px | 0.8 px |

角度按模 360 比较，最大差异 0.3°；缩放最大差异 0.003，alpha 最大差异
0.005。每一项最差样本的轨道名、帧号、原生值和原版值均保存在 JSON 中。
这些是整数帧的参数比较，不是渲染后的像素误差，也不是逐字节相等验证。
旧 Flash 导出的 `.reanim` 与现代 Animate 保存的 XFL 仍有上述差异，当前
资料不足以把全部残差归因于重存、固定点精度或补间求值的某一项。

补间已改为先插值最外层实例，再组合静态子符号的矩阵。原先先组合后插值，
会把偏移子图片的圆弧路径变成直线，Blover 的误差达到 28 px 以上。
同一张底图在相邻关键帧更换包装符号时，终点会先转换到起点符号的注册
坐标系，避免 Polevaulter 的 `Tween 1` 包装偏移造成错误。切换成不同图片时，
不把目标图片的内部缩放提前混入当前图片。

便携回归测试 `tests/jsfl_semantics.rs` 不依赖本机原版资源路径。其中六个
原版动画的 61 条无图 `anim_` 轨使用真实 XFL 子树和独立 `.reanim` 活动范围；
Blover 三条花瓣轨使用原版位置数据。另有旋转子偏移、注册坐标系改变、
不同图片切换、辅助层与隐藏层、标签、attacher 指令、单 run 文本、资源
ID 字符保持与 manifest 别名的回归测试。

图片身份优先保留原始 `IMAGE_*` ID。普通作者文件按照 JSFL 的名字规则生成，
不会因为 manifest 有同名 basename 就改为另一资源。TreeFood 的
`reanim/TreeFood.png` 与 `images/TreeFood.png` 是两个可有不同像素的资源；
只有原始 ID 无声明及 reanim 文件 fallback，且库名含完整资源路径并精确
匹配 manifest 时，才允许改用该 manifest ID。Native 反解还在每个
`DOMBitmapItem` 的持久数据中保存 `fla-reanim.resource-id`，编辑回编直接
保留该值。原 compiled 内实际存在的辅助轨使用 document 持久数据
`fla-reanim.compiled-helper-tracks` JSON 白名单保留；普通作者的 `_` 辅助层
仍按 JSFL 规则跳过。
