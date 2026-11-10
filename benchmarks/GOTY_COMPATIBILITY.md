# GOTY 格式兼容性修复与验收

本次以用户提供的 GOTY `reanim` 目录、其中的 `FlashReanimExport.jsfl`、
原版 `.reanim` 文本和当前游戏的 C++ 加载器为独立依据。此前的
`corpus-validation.json` 只验证未编辑原始数据恢复和图片像素；它不能证明
FLA 编辑后正常回编能被游戏正确读取。本报告使用正常编译路径重新验收。

## 格式修复

- 舞台不再固定为 800×600。151 个原始 OLE FLA 的文档 RECT 提供原版尺寸，
  9 份 JSFL 实测交叉验证一致。Chomper/Blover 为 80×80，Zombie 为 80×139，
  Boss/SelectorScreen 为 800×600。compiled 本身没有舞台尺寸；自定义文件
  使用正向生成的 `.reanim.authoring.json` 或 `--stage WIDTHxHEIGHT`。
  尺寸仅改变画布，游戏坐标原点始终为 `(0,0)`。
- `anim_` 是普通层名，不从帧标签猜测动作，不自动添加 `anim_full`。
  顶层恰有一个元素的帧活动，否则隐藏；活动的无图标记使用空 locator
  实例。`anim_hair` 等带图层仍绘制。层序从底到顶，`_ground` 保留，普通
  `_` 辅助层跳过；编辑器眼睛状态不决定游戏显隐。
- 补间先插值外层实例，再组合静态子符号矩阵，保留偏移图片的旋转弧线。
  同图不同包装符号的注册坐标转换后再插值；换图不会提前混入下一张图的
  内部缩放。曲线拟合会减少关键帧，但不会声称恢复丢失的作者操作历史。
- 保留原始图片 ID，包括空格、连字符、双下划线。普通位图包装符号命名
  `ReanimBitmap_N`，避免被 `attacher__` 误识为指令。支持单 run 静态文字、
  字体 URL 覆盖及字符串清空哨兵。
- 资源解析依次使用精确 manifest ID、`IMAGE_REANIM_X` 对应的 manifest
  `IMAGE_X`、游戏文件路径 fallback。TreeFood 的 `images` 与 `reanim`
  同名图片按游戏优先级选择。正常回编从当前 DAT 导出实际资源路径下的
  PNG，并同步更新独立 alpha mask 的蓝通道。
- 原始文件名按字面匹配，带尾空格与不带尾空格的 Zamboni6 图片保持独立，
  不让规范化名称或目录枚举顺序决定实际贴图。
- 普通 Adobe FLA 正向编译也导出当前库图像，支持 DAT 头中明确声明的
  未压缩 ARGB、分块 zlib 和直接 JPEG。长度、分块终止和尾随流仍严格校验；
  不把原版未压缩数据误当成损坏压缩流。
- `DOMBitmapItem` 的 `fla-reanim.resource-id` 持久数据保存精确 ID；文档的
  `fla-reanim.compiled-helper-tracks` 白名单只保留 compiled 内原本存在的
  辅助轨，不改变普通作者 `_` 层的跳过规则。

## 三组独立检查

1. **原版 FLA 正向对照**：六份原始 FLA 经 Adobe 保存为 ZIP/XFL，正常编译
   后与独立原版 `.reanim` 比较。轨道数、顺序、帧数、FPS、显隐和活动资源
   全部匹配。最大位置差仍有 1.0px，角度 0.3°、缩放 0.003、alpha 0.005；
   不把这些残差称为完全等价。见 [完整记录](GOTY_FORWARD_VALIDATION.md)。
2. **157 个资源编辑后正常回编**：移除源指纹，实际修改一处 DAT 的颜色和
   透明度，强制走正常编译。独立 Python DefMap 读取器检查有序轨道、重复
   轨名、帧数、显隐、动作范围、精灵格和字面 image/font/text。参数容差
   包含逆向拟合和正向半格量化；隐藏帧不用的缓存字段单列，不冒充活动差异。
   另逐像素检查导出图片、真实部署路径和外部 mask。见
   [原始报告](results/editable-goty-final.json) 与 [验证脚本](validate_editable.py)。
3. **游戏 C++ 加载器核心**：提取游戏原有 26 个函数及 DefMap 表，执行实际
   schema、解压、字段装载、FillMissing、资源查找与 GetFramesForLayer。
   同时验证原资源和正常回编资源，使用双前缀、错误 schema 和空资源目录
   三项负控。见 [方法与复现](game_loader/README.md)、
   [源资源报告](results/game-loader-source.json) 和
   [回编资源报告](results/game-loader-editable.json)。

正常回编语料共 157 个文件，全部通过：轨道/帧数、显隐、动作区间、精灵格、
活动 image/font/text 的差异均为零；150 次实际 DAT 编辑导出、1,863 次 DAT
像素比较和 18 次外部 mask 检查全部通过，部署路径错误为零，原始快照恢复
分支使用次数为零。活动参数最大差为位置约 0.300004px、角度约 0.100037°、
缩放约 0.002001、alpha 约 0.004001，均在预设的拟合加正向量化界限内。
18 次 mask 检查中，4 次对应本轮实际修改 alpha 的图片，其余检查未修改
图片的导出与部署一致性。

同一语料的 411,824 个轨道帧采样重建为 67,655 个关键帧，包含 30,811 段
跨度大于一帧的运动补间。图片或显隐切换及无法满足容差的运动仍保留必要
关键帧；这项统计不把每一个单帧段都称为有效的关键帧压缩。

最终 CLI SHA-256 为
`91279932C35755C71FDAE9F04AF662578D7D9B943E9FE9C2F6D2F99ECF0B4005`。
原 Adobe FLA 的 133 张图片出口另有 [独立验证](results/adobe-bitmap-export.json)。
资源查找现按不同图片 ID 去重，避免对同一图片的每个采样帧重复访问磁盘。
去重优化前后的 157 个 FLA、2,345 个正常编译输出文件、151 个 Adobe 编译
输出和 4 个真实用户样本全部逐字节相同，见
[完整输出等价报告](results/reverse-dedup-output-equivalence.json)。

## 验收范围

游戏加载器测试使用真实文件/manifest 回调，不初始化 SDL、不解码图片、
不构造图集、不渲染游戏画面。像素由另一套独立检查完成。它们不能替代完整
游戏 UI 回放或 Adobe 对所有复杂动画的渲染检查。

此前 [Adobe 实测](ADOBE_VERIFICATION.md) 验证了补间、透明位图、保存与编辑；
本次在用户停止电脑操作后，没有再次控制 Adobe。新加的持久数据在最终
Adobe 另存过程中的保留情况尚未实际验证。若编辑器丢弃这些字段，精确 ID
或原 compiled 辅助轨可能需要显式恢复，不能以快照恢复测试掩盖这一边界。

旧失败报告保留为过程证据：`editable-pre-compat-smoke.json`、
`editable-pre-compat-fix.json` 与 `editable-goty-candidate.json`；后者仍有
11 项语义失败。`editable-goty-before-filename-fix.json` 记录加强路径校验后
发现的 1 个尾空格碰撞。这些历史失败报告不能作为新版通过的证据。
