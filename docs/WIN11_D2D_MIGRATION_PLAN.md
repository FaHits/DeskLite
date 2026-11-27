# DeskLite Win11 Pure Win32 + Direct2D (`UpdateLayeredWindow`) 轻量化迁移方案

> 状态：**已定型设计、暂缓动工**（当前基线提交 `c36854a`，分支 `feat/route-b-desktop-takeover`）。  
> 目标：彻底移除 `eframe` / `egui` / `egui_glow` / `winit` / `glutin` / `accesskit` 与多窗口 OpenGL 上下文，使全部窗体（`FreeDesktop`、所有桌面格子 `Fence`、`参数设置悬浮窗`、主循环）统一为纯 Win32 + Direct2D + DirectWrite + `UpdateLayeredWindow` 逐像素半透明方案。

---

## 1. 架构决策（已确认）

- **绘制路径**：`ID2D1Factory` + `ID2D1DCRenderTarget` / `CreateDIBSection`（32-bit `PBGRA`）+ `UpdateLayeredWindow`（与已稳定落地的 [free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs) 完全同构）。
  - 创建窗口即先提交首帧 DIB 位图再 `ShowWindow`，天然 **0 白帧 / 0 黑闪 / 0 `WM_ERASEBKGND` 闪烁**，无需 `WH_CBT` / `WH_CALLWNDPROC` 隐藏 `winit` 初始矩形的补丁。
  - 背景采用逐像素预乘 Alpha 半透明深色玻璃圆角矩形 + 细描边（严格遵循 `spec.backdrop_alpha`），不受 DWM `Acrylic` 焦点变色闪烁影响。
- **文本与图标**：
  - DirectWrite（`Segoe UI` / `Microsoft YaHei UI`）直接渲染标题、两行截断标签与单行截断列表，复用现有 `IconCache`（PBGRA）转 `ID2D1Bitmap` 并按需 LRU 缓存。
- **窗口级架构**：
  - 删除隐藏的 1×1 `DeskLite Host` OpenGL 根视口；`main.rs` 退化为单条 Win32 消息循环 + Helper/Desktop 协调器，可直接承接或合并原 Helper 线程消息。
  - 每个格子 `Fence` 是一个轻量 `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_POPUP` HWND（挂载到 `WorkerW`/`Progman` Owner 下，继续复用 [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs) 的单链 Z-order 治理）。

---

## 2. 模块复用与重构边界盘点

### A. 100% 原样保留的底座
- `crates/desklite-core`（布局配置、多屏拓扑匹配、WorkArea 裁剪、原子持久化）
- `crates/desklite-legacy`（DeskGo 导入/修复）
- `crates/desklite-win32` 全部系统能力：
  - [desktop_listview.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/desktop_listview.rs)：Explorer 原生图标显示/隐藏与刷新
  - [shell_ops.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_ops.rs)、[shell_menu.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_menu.rs)：Shell 文件操作与混合 `IContextMenu`/`IContextMenu3` 右键菜单（含左键单击 STA 预取缓存）
  - [drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)、[drag_ghost.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_ghost.rs)、[folder_watch.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/folder_watch.rs)、[tray.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/tray.rs)、[events.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/events.rs)、[icons.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/icons.rs)
  - [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)：保留 Z-order 链协调、边缘调整框与快捷键解码；后期移除 `winit` 专用的 `install_ui_thread_cloak_hook` / `strip_alt_tab_from_host` / `post_uncloak_after_swap`
  - [free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)：完整保留，并把其 `LayeredSurface` + D2D/DWrite + Win32 `EDIT` 重命名能力抽取为共享 Win32 D2D 基础组件

### B. 需要抽象解耦（无 UI 重写）
- [state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[actions.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/actions.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[undo.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/undo.rs)、[hit_test.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/hit_test.rs)、[snap.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/snap.rs)
- **改动点仅限**：
  1. 用轻量的 `RepaintTarget` / Win32 `PostMessage` / `invalidate_window_no_erase` 替换 `egui::Context::request_repaint*` 与 `ViewportId::ROOT`。
  2. 移除 `SharedFenceState` 中的 `textures: HashMap<String, Option<TextureHandle>>`（改为各 D2D HWND 本地或共享 PBGRA `IconCache` -> `ID2D1Bitmap` 映射）。
  3. 保持 `crates/desklite-ui` 内 20 个业务单元测试可不依赖 GPU/窗口上下文直接运行。

### C. 需要纯 Win32 + D2D 重写的 3 块模块
1. **共享 D2D 渲染基建 (`crates/desklite-win32/src/d2d_layered.rs`)**：
   - 从 `free_desktop_wnd.rs` 抽出 `LayeredSurface`（`CreateCompatibleDC` + `CreateDIBSection` + `UpdateLayeredWindow`）、`ID2D1Factory` / `IDWriteFactory` / `ID2D1DCRenderTarget` 管理、`IconBitmapCache`（PBGRA -> `ID2D1Bitmap`）、常用圆角卡片/文字截断/平滑插值工具与原生浮层 `EDIT` 重命名控件封装。
2. **原生格子窗体 (`crates/desklite-win32/src/fence_wnd.rs`)**：
   - 对标替代 `fence_view.rs` + `item_grid.rs`：
     - 标题栏（折叠箭嘴、视图切换变形图标、菜单按钮 `...`、锁图标、双击折叠/展开 155ms `ease_out_cubic`、标题拖拽置顶与边缘吸附）。
     - 网格/列表视图（170ms 布局过渡与透明度切换、自适应列数、`hit_test.rs` 复用、滚轮与自定义细滚动条）。
     - 交互与快捷键（左键单选/`Ctrl` 加选/`Shift` 连选 + 触发右键菜单预取；右键直接在 `WM_RBUTTONUP` 走混合 Shell 菜单；`F2`/慢双击内联 `EDIT` 重命名；空白框选；格内拖拽排序与拖出启动 `start_file_drag_out_async`；注册 OLE `IDropTarget` 接收桌面/资源管理器拖入；`Ctrl+C/X/V/Z/A`、`Alt+Enter`、`Delete`、`Type-ahead` 拼音/首字母定位）。
     - 动画计时：仅在折叠/展开、视图切换、悬停淡入、Toast 倒计时进行期间启用 `SetTimer(..., 16ms)`，动画结束立即 `KillTimer` 回归 **0% 静止 CPU**。
3. **原生参数设置窗 (`crates/desklite-win32/src/inspector_wnd.rs`)**：
   - 对标替代 `inspector.rs`（966 行）：
     - 顶栏与“全局默认设置 / 单格独立覆写”标签及一键“全部跟随全局”。
     - 自定义 D2D 控件：分段选择按钮（预设密度 / 视图模式）、滑块（格宽、格高、列表行高、图标尺寸、背景透明度、窗口 X/Y/W/H）、开关按钮、滚动容器，实时直接驱动 `SharedFenceState` 并广播脏格子重绘。

---

## 3. 分阶段实施路线（每阶段可独立验证）

| 阶段 | 目标 | 交付与验收标准 |
| :--- | :--- | :--- |
| **Phase 1** | **逻辑与 `egui` 解耦** | 抽离 `state.rs`/`actions.rs`/`free_desktop_view.rs` 中的 `egui::Context` 与 `TextureHandle` 依赖；补齐几何/布局纯逻辑测试；`cargo test --workspace` 全绿 |
| **Phase 2** | **抽出 `d2d_layered.rs` 共享层** | 从 `free_desktop_wnd.rs` 提取公共 DIB + D2D + DWrite + `EDIT` 控件层，`FreeDesktop` 零回归 |
| **Phase 3** | **实现原生 `fence_wnd.rs`** | 完成全部格子视图、动画、OLE 拖放、框选、排序、混合右键、快捷键；实机对照逐条验证交互一致性 |
| **Phase 4** | **实现 `inspector_wnd.rs` 并拔除 `egui`** | 完成参数悬浮窗；`main.rs` 切为纯 Win32 主消息循环；清理 `Cargo.toml` 中的 `eframe`/`egui`/`window-vibrancy`/`raw-window-handle` |
| **Phase 5** | **全量验收与裁剪基准对比** | `cargo clippy --all-targets -- -D warnings` + `cargo test --workspace` + Release 体积/启动耗时/空闲内存与 `Win+D` 实测 |

---

## 4. 预期收益

- **安装包体积**：预计从当前 **~5.0 MB (`5,245,952 B`)** 降至 **~1.2–1.8 MB**（单文件静态 Release，去掉整个 OpenGL/`eframe`/`egui`/`winit`/`accesskit` 依赖树与内置字体拷贝）。
- **内存与显存**：去掉所有 OpenGL 上下文与字形图集纹理，空闲内存显著下降，多格子同时展开无 GPU 上下文切换成本。
- **启动与响应**：无 `DeskLite Host` 隐藏视口冷启动与跨视口唤醒延迟，首帧即可精准按目标尺寸带透明通道呈现。
