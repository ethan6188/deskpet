# deskpet

用 Rust 写的 macOS 3D 桌宠：透明置顶窗口、Retina 原生分辨率实时渲染、可拖动可交互，目标是全天常驻也不耗电。

## 当前状态

原型阶段，宠物是程序生成的占位几何体（身体、头、耳朵）。

- 渲染：wgpu（Metal 后端），4x MSAA，sRGB，透明背景预乘 alpha 合成。
- 窗口：winit，无边框、透明、始终置顶。
- 动画：呼吸缩放。
- 交互：左键拖动窗口，右键拖动旋转，Esc 退出。透明区域点击穿透到下层窗口，只有点在宠物身上才算。
- 取景：窗口和相机按宠物包围盒自动贴合，少画透明像素。
- 省电：优先集成显卡（`PowerPreference::LowPower`），vsync；只在帧到期时重绘，按交互中、空闲（2 秒后）、打盹（60 秒后）三档降帧；窗口被遮挡时停止渲染。

功耗档位（帧率：交互中 / 空闲 / 打盹）：

| 档位 | 帧率 | 实测空闲 CPU（M 系列，release） |
|---|---|---|
| `--power eco` | 30 / 12 / 4 | 1.6% |
| `--power balanced`（默认） | 60 / 24 / 8 | 4.6%，打盹 1.2% |
| `--power smooth` | 120 / 30 / 10 | 4.5% |

## 运行

```bash
cargo run --release
cargo run --release -- --power eco
```

需要 Rust 稳定版（edition 2024）和 macOS。

## 路线

完整方案见 [docs/ROADMAP.md](docs/ROADMAP.md)。

1. 资产管线：加载 glTF 模型、骨骼蒙皮动画、表情 blendshape。
2. 毛发与材质：PBR 加毛发着色（shell/fin 或发丝卡片）。
3. 行为：状态机驱动的待机小动作、视线跟随鼠标、点击反馈。
4. 点击穿透：透明区域鼠标事件穿透到下层窗口。
5. 功耗：空闲帧率进一步下调、无变化时跳帧，并用 Instruments 量化。
