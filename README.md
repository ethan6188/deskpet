# deskpet

用 Rust 写的 macOS 3D 桌宠：透明置顶窗口、Retina 原生分辨率实时渲染、可拖动可交互，目标是全天常驻也不耗电。

## 当前状态

原型阶段，宠物是程序生成的占位几何体（身体、头、耳朵）。

- 渲染：wgpu（Metal 后端），4x MSAA，sRGB，透明背景预乘 alpha 合成。
- 窗口：winit，无边框、透明、始终置顶。
- 动画：呼吸缩放。
- 交互：左键拖动窗口，右键拖动旋转。
- 省电：优先集成显卡（`PowerPreference::LowPower`），vsync，空闲时降到 30 fps，交互后 2 秒内满帧，窗口被遮挡时停止渲染。

## 运行

```bash
cargo run --release
```

需要 Rust 稳定版（edition 2024）和 macOS。

## 路线

1. 资产管线：加载 glTF 模型、骨骼蒙皮动画、表情 blendshape。
2. 毛发与材质：PBR 加毛发着色（shell/fin 或发丝卡片）。
3. 行为：状态机驱动的待机小动作、视线跟随鼠标、点击反馈。
4. 点击穿透：透明区域鼠标事件穿透到下层窗口。
5. 功耗：空闲帧率进一步下调、无变化时跳帧，并用 Instruments 量化。
