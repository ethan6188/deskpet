# deskpet

用 Rust 写的 macOS 3D 桌宠引擎，也给 agent 提供制作桌宠的工具。透明置顶窗口、Retina 原生分辨率实时渲染、可交互，目标是全天常驻也不耗电。完整方案见 [docs/ROADMAP.md](docs/ROADMAP.md)。

![同一只狐狸的两种风格：实物、动漫](docs/styles.png)

## 运行

```bash
cargo run --release                                  # 默认 pets/fox，实物风格
cargo run --release -- --style anime                 # real | anime | splat
cargo run --release -- --pet pets/fox --power eco    # eco | balanced | smooth
```

## 给 agent 用的工具

```bash
deskpet new pets/cat --model cat.glb --name 猫     # 从 glb 生成宠物包，猜测项标 GUESS
deskpet check pets/cat                           # 检查资产和预算；--json 给程序读，出错退出码 1
deskpet render pets/cat                          # 渲染联系表 pets/cat/contact.png
deskpet bake-splats pets/cat --count 60000       # 用模型贴图铺一层高斯点，先试 splat 风格
```

联系表每种风格一组：一圈 360° 转台、每个动画一行（首帧到末帧）、头部朝四个方向看和眨眼。`check` 检查清单名字、授权、面数和贴图、蒙皮权重、待机循环接缝、动作切换跳变、每帧 GPU 耗时、宠物在窗口里的占比。

[skills/deskpet/SKILL.md](skills/deskpet/SKILL.md) 是给 Claude Code 的 skill，教 agent 按"起稿、渲染、看图、改进"做宠物。复制到 `~/.claude/skills/deskpet/` 即可使用。

## 交互

左键点一下有反应，按住拖动挪位置；右键拖动旋转；Esc 退出。宠物以外的透明区域点击穿透到下层窗口。

## 宠物包

一个目录就是一只宠物：`pet.toml` 加 glTF 模型（`.glb` 或 `.gltf`）和授权文件。示例见 [pets/fox/pet.toml](pets/fox/pet.toml)。

| 字段 | 含义 |
|---|---|
| `name`、`model` | 名字和模型文件 |
| `facing` | 绕 Y 轴旋转多少度朝向镜头 |
| `height` | 窗口高度（点） |
| `style` | 默认风格名 |
| `[animations]` | `idle` 待机循环、`fidget` 空闲时随机穿插的小动作、`react` 点击反应，填 glTF 动画名 |
| `[rig]` | `head` 跟随鼠标转动的骨骼、`look_limit` 最大转角、`blink` 闭眼用的 morph target |
| `[styles.<名字>]` | `kind`（real / anime / splat）、`tint`、`outline`、`outline_color`、`fur`、`glow`、`bands`；splat 另有 `splats`、`splat_scale`、`splat_rotate`、`splat_offset` |

名字写错（动画、骨骼、morph、风格）启动时就会报错并指出是哪一项。

## 引擎

- **模型**：glTF 网格、贴图（带 mipmap 和各向异性过滤）、骨骼蒙皮、morph target（最多 32 个）、动画剪辑（线性、阶跃、三次样条）。蒙皮和 morph 都在 GPU 上算。
- **风格**：一个着色器两种着色。
  - 实物：包裹漫反射模拟毛发透光、天空和桌面两色环境光、GGX 高光、轮廓光，外加 16 层壳体毛发（shell fur）。
  - 动漫：分层卡通着色、阴影偏冷色、硬边高光，加反向外壳描边。
- **行为**：待机循环，空闲时随机穿插小动作，点击反应，动作之间 0.35 秒平滑过渡；头部跟随鼠标；有闭眼 morph 时随机眨眼，偶尔连眨两下；打盹时呼吸放慢、不再做小动作。
- **点击穿透**：光标射线和当前姿势的网格逐三角形求交，腿缝之间也能点穿。
- **取景**：按所有动画、任意旋转角度下的包围盒定窗口大小，宠物不会走出窗口。
- **省电**：优先集成显卡，vsync；只在帧到期时重绘，按交互中、空闲（2 秒后）、打盹（60 秒后）三档降帧，播放动作期间临时满帧；窗口被遮挡时停止渲染。

| 档位 | 帧率：交互中 / 空闲 / 打盹 |
|---|---|
| `eco` | 30 / 12 / 4 |
| `balanced`（默认） | 60 / 24 / 8 |
| `smooth` | 120 / 30 / 10 |

实测（Apple M3，release，狐狸 1728 顶点，空闲 15 秒平均）：balanced 下 real 和 anime 约 1.9% CPU，splat（6 万点）约 3.7%，eco 约 1.2%。

## 高斯泼溅（splat 风格）

`kind = "splat"` 的风格不画网格，改画 3D 高斯点（标准 3DGS 二进制 PLY，取 0 阶球谐颜色）。glTF 仍提供骨骼、动画和点击穿透用的网格：

- 加载时按 `splat_scale`、`splat_rotate`、`splat_offset` 把点云对齐到模型的静止姿势，再把每个点绑到最近的顶点，继承它的骨骼和权重，GPU 上和顶点一样蒙皮。
- 每帧在 CPU 上按深度做基数排序（6 万点约 0.5 ms），骨骼没动时跳过排序；GPU 上把协方差投影成屏幕椭圆（EWA），四边形大小按不透明度截到 1/255，从后往前预乘混合。splat 风格不开 MSAA。
- 真实拍摄的点云可以用 [Brush](https://github.com/ArthurBrussee/brush) 等工具重建，裁掉背景、对齐到模型后放进宠物包。`check` 会报告点数、排序耗时和多少点离网格太远（对齐错了或带了背景）。
- 示例狐狸的 `splats.ply` 是 `bake-splats` 从低多边形贴图生成的，只用来验证管线。
- `photo` 风格用一张 4K 照片生成（[来源和流程](pets/fox/reference/README.md)）：Vision 抠图 → Apple SHARP 单图生成 → `tools/crop_splats.py` 裁背景 → `deskpet align-splats` 自动对齐到骨骼。单图看不到另一侧，转到背面会露馅；SHARP 权重仅限研究用途，这份点云不能随产品发布。

![照片点云绑到狐狸骨骼上的联系表](docs/photo-splats.png)

## 素材授权

`pets/fox` 的狐狸模型来自 [Khronos glTF Sample Assets](https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/Fox)，CC0 / CC-BY 4.0，见 [pets/fox/LICENSE.md](pets/fox/LICENSE.md)。
