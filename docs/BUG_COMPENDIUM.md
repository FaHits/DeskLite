# DeskLite 技术 Bug 统计与根因复盘全录

> 覆盖范围：`E:\Ai_Agent\DeskLite` 从初始化提交 `5424ebf` 至当前工作分支 `feat/route-b-desktop-takeover` 共计 **46 项**典型故障与架构级缺陷修复（含前置立项阶段逆向确认的原版 DeskGo 核心错位根因）。

---

## 文档内索引

### 1. 按研发阶段与提交索引

- [0. 立项前原版 DeskGo 遗留根因（架构避坑基线）](#sec-legacy)
- [1. M2 PoC 阶段（`5424ebf` → `cab127d`）](#sec-m2)
  - [#01 `poc-fence` 4Hz 窗口闪烁与 DWM 透明底色冲突](#bug-01)
- [2. M3 Direct2D → `egui 0.37` 重构与初版 QA（`40e47c1` → `3c7bbe9` → `c6c8b67`）](#sec-m3)
  - [#02 遗留全屏隐形容器 `Box id=1` 遮挡桌面](#bug-02)
  - [#03 手写 D2D 窗体无法拖动、缺折叠钮、Win95 弹窗粗糙与无法拖出](#bug-03)
  - [#04 `Alt+Tab` 残留、DWM 幽灵 `[×]`、跨线程 WIC 失败、半透明菜单与文字重叠](#bug-04)
  - [#05 右上角缩放热区被标题按钮保留宽度阻断](#bug-05)
  - [#06 网格图标与文字左偏未居中](#bug-06)
  - [#07 右键 / 三点菜单失焦不自动关闭](#bug-07)
  - [#08 解绑 Folder Portal 后旧映射条目残留](#bug-08)
  - [#09 切焦点闪白边、`AlwaysOnBottom` 失效与 `Win+D` Z-Order 掉层](#bug-09)
  - [#10 原地桌面隐藏与 `LVS_OWNERDATA` 级联误删 / 精确坐标回放](#bug-10)
  - [#11 外部编辑带 UTF-8 BOM 导致 `layout.json` 解析失败](#bug-11)
- [3. M4 基线：日常交互完善与图形化参数面板（`36974fb`）](#sec-m4)
  - [#12 `ScrollArea` 灰白渐变阴影与边缘留白偏移](#bug-12)
  - [#13 桌面映射 Portal 格图标未在原桌面隐藏](#bug-13)
  - [#14 拖动格子边缘拉伸概率性卡死](#bug-14)
  - [#15 拖到屏幕边缘回弹并闪出原生 `DeskLite#<id>` 标题框](#bug-15)
  - [#16 折叠后拖动格子概率回弹](#bug-16)
  - [#17 网格条目行内重命名挤偏右侧相邻图标](#bug-17)
  - [#18 桌面映射格重命名/取消导致全部隐藏桌面图标重新显现](#bug-18)
  - [#19 折叠态菜单被 38px 窗体裁切 → 次级 GL 窗闪框及 100% 拖拽卡死](#bug-19)
  - [#20 `Ctrl+C`/`Ctrl+V` 偶发失灵、剪贴板陈旧与需连按两次](#bug-20)
  - [#21 标题多余 `[文件夹]` 后缀、文字下沉与空白区点击/右键失效](#bug-21)
  - [#22 网格图标偏上、固定尺寸不随格宽缩放、全局排版不联动与浅蓝边框](#bug-22)
  - [#23 参数设置面板：点 `×` 假死、半屏裁切与顶部白线、拉伸无实时读数、滑块卡顿、名称禁尾空格、缺单格独立排版](#bug-23)
- [4. Route B 架构重构与桌面全接管（`9b126a3` → `6377c23` → `1673c9e` → `d78aa1e` → `044c902`）](#sec-routeb)
  - [#24 桌面单文件多格子归属冲突与遗留 `BoxId=1` 自由图标丢失](#bug-24)
  - [#25 任务栏自动隐藏 / 调尺寸误触发显示器拓扑切换](#bug-25)
  - [#26 首次切入未见过的多屏 / DPI 拓扑丢失相对位置与缩放](#bug-26)
  - [#27 同进程并发原子写文件冲突、`.tmp` 泄漏与脏边界持久化](#bug-27)
  - [#28 单项 `SHCNE_DELETE` 竞态、对话框 `SHELLDLL_DefView` 误抓、延迟隐藏被覆盖与同步恢复阻塞](#bug-28)
  - [#29 `FreeDesktop` 画布与分组格子 Z-Order 互搏及 `WorkerW` 句柄过期](#bug-29)
  - [#30 全局 `Mutex` 中毒、`IN_SIZE_MOVE_HWND` 卡死与事件监听窗内存泄漏/唤醒丢失](#bug-30)
  - [#31 同步 `DoDragDrop` 阻塞 UI 线程与 `IDataObject` / 按键松开漏检](#bug-31)
  - [#32 CCD 多目标缓冲竞态、`FolderWatcher` 断连、`IconCache` 无限增长与自复制/回收站中断异常](#bug-32)
  - [#33 `explorer.exe` 重启（`TaskbarCreated`）后托盘消失与原生桌面图标重现](#bug-33)
  - [#34 切屏/唤醒抖动期瞬态坐标污染快照与实时窗口未同步（M5 Live Sync）](#bug-34)
  - [#35 `HelperState` 模态文件夹选择器重入风险与全局锁中毒](#bug-35)
  - [#36 `desklite_free_desktop` 全屏 WGL 黑屏遮罩与 `UnfiledDesktop` 图标重复](#bug-36)
  - [#37 内部文件拖拽跨套间 COM `SendMessage` 死锁（Application Hang）](#bug-37)
  - [#38 多 Viewport 串联 `vsync` 导致帧率严重卡顿](#bug-38)
  - [#39 `UpdateLayeredWindow` + Direct2D `FreeDesktop` 彻底根治 WGL 黑屏/闪屏与 `UnfiledDesktop` 冲突](#bug-39)
  - [#40 唤醒未挂载/隐藏 `egui` Viewport 触发窗体闪现](#bug-40)
  - [#41 文件拖出缺预览图（`DragGhost` + `IDragSourceHelper2`）与跨边框缩放光标闪烁](#bug-41)
  - [#42 多区域焦点选中残余与右键/内联编辑/设置窗误清空 + 拖拽边缘阈值偏差](#bug-42)
  - [#43 程序启动/退出瞬间格子闪现纯白矩形框（DWM/WGL 首帧与销毁竞态）](#bug-43)
  - [#44 `FreeDesktop` 自绘桌面图标顶置对齐/毛坯感、失焦不清选、悬停缩放光标残留与缺热键](#bug-44)
  - [#45 `FreeDesktop` 左键单击、空白点击与框选被 `ReleaseCapture()` 同步 `WM_CAPTURECHANGED` 静默吞掉](#bug-45)
  - [#46 `FreeDesktop` 键盘焦点与热键断链、DPI 格距不统一、D2D 渲染粗糙、`vsync` 回归及跨格粘贴/撤销补漏](#bug-46)
- [5. 跨 Bug 提炼的四条核心工程经验](#sec-takeaways)

---

### 2. 按技术域与总览速查表

| 技术域 | 包含 Bug 编号 | 核心共性机制 |
| :--- | :--- | :--- |
| **A. Win32 窗体样式 / DWM / Z-Order** | `#01` `#03` `#04` `#09` `#14` `#15` `#16` `#23` `#29` `#33` `#39` `#43` | `WorkerW`/`Progman` 归属、`WM_WINDOWPOSCHANGING`、原生 `WM_STYLECHANGING` / `WM_NCCALCSIZE`、`TaskbarCreated` |
| **B. 图形渲染 / WGL / Direct2D / 帧率** | `#01` `#04` `#12` `#19` `#22` `#23` `#36` `#38` `#39` `#40` `#43` `#44` `#46` | `eframe` 多 Viewport WGL `make_not_current` 崩溃与 `SwapInterval::Wait(1)` 串联阻塞、`UpdateLayeredWindow` Premultiplied Alpha |
| **C. Win32 Shell / 桌面图标接管 / 监听** | `#08` `#10` `#13` `#18` `#28` `#32` `#33` `#46` | Win11 `SysListView32` `LVS_OWNERDATA` 陷阱、`SHChangeNotify` 级联重载 → 整体隐藏 `SHELLDLL_DefView`、`ReadDirectoryChangesW` |
| **D. OLE / COM 拖放 / 剪贴板 / 热键 / UI 交互** | `#03` `#05` `#06` `#07` `#17` `#19` `#20` `#21` `#31` `#37` `#41` `#42` `#44` `#45` `#46` | 跨 STA 套间 COM `SendMessage` 死锁、OLE `DoDragDrop` 两阶段分流、`WM_KEYDOWN` + `MAPVK_VSC_TO_VK_EX`、全局唯一焦点域 |
| **E. 持久化 / 多屏拓扑 / 并发与容错** | `#02` `#11` `#24` `#25` `#26` `#27` `#30` `#32` `#34` `#35` `#46` | 单一归属模型（`FenceRole`）、CCD `TopologyKey`、原子写序号临时文件、切屏稳态期保护（Settling Guard） |

---

<a id="sec-legacy"></a>
## 0. 立项前原版 DeskGo 遗留根因（架构避坑基线）

> 参考：[bug_mechanism.md](file:///e:/Ai_Agent/FuckDeskGO/decompiled/03_findings/bug_mechanism.md)（基于 DeskGo 3.0.1409.127 `Features64.dll` 逆向分析）。

1. **伪拓扑键（`"%d*%d"`）与无边界写回**：`CDeskDesk::DeskSizeChange`（`FUN_180150b00`）仅用总分辨率字符串做 key，不含显示器数量、排列、主副屏及 DPI，且写回自由图标绝对坐标时不裁剪，导致双屏切单屏或主副互换后错位并被延迟定时器落盘固化。
2. **单屏快速返回跳过校验**：单屏且分辨率未变时直接 `return`，睡眠唤醒 / 远程重连后 Windows 重排的错误位置反向覆盖真实配置。
3. **`WM_POWERBROADCAST` 只打日志不重排 + 0 屏瞬态跳过**：电源唤醒仅广播事件而不触发校验。

DeskLite 在 M1/M2 起即在 `desklite-core`（`TopologyKey`、`clamp.rs`、`store.rs`）与 `desklite-win32`（CCD `QueryDisplayConfig`、`events.rs`）从协议层根除上述问题。

---

<a id="sec-m2"></a>
## 1. M2 PoC 阶段（`5424ebf` → `cab127d`）

<a id="bug-01"></a>
### #01 `poc-fence` 4Hz 窗口闪烁与 DWM 透明底色冲突
- **提交 / 模块**：`cab127d` · [fence_poc.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/fence_poc.rs)
- **现象**：试验格子在屏幕左上角以约 4 次/秒频率持续闪烁。
- **技术根因**：
  1. `poc_wnd_proc` 启动了 250ms 的 `REORDER_TIMER_ID` 定时器周期性调用 `SetWindowPos(..., HWND_BOTTOM, ...)`，与 Windows“owned window 必须高于其 `progman` owner”规则持续互搏。
  2. `ID2D1HwndRenderTarget` 使用 `D2D1_RENDER_TARGET_PROPERTIES::default()`（不透明 `alphaMode`），与 `DwmExtendFrameIntoClientArea(-1)` + Acrylic 冲突。
  3. 未处理 `WM_ERASEBKGND`，导致 Direct2D 绘制前 GDI 先刷白背景。
- **修复方案**：移除 `REORDER_TIMER_ID`/`WM_TIMER`，改用事件驱动 `WM_WINDOWPOSCHANGING` 维持置底与 `WM_MOUSEACTIVATE => MA_NOACTIVATE`；`WM_ERASEBKGND` 返回 `LRESULT(1)`；渲染目标显式配置 `DXGI_FORMAT_B8G8R8A8_UNORM` + `D2D1_ALPHA_MODE_PREMULTIPLIED`；Owner 改为 `t.worker_w.unwrap_or(t.progman)`。

---

<a id="sec-m3"></a>
## 2. M3 Direct2D → `egui 0.37` 重构与初版 QA（`40e47c1` → `3c7bbe9` → `c6c8b67`）

<a id="bug-02"></a>
### #02 遗留全屏隐形容器 `Box id=1` 遮挡桌面
- **提交 / 模块**：`3c7bbe9` · [convert.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-legacy/src/convert.rs)、[layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：从 DeskGo 导入配置启动后出现一个未命名且占满整块屏幕的格子。
- **技术根因**：DeskGo 的 `FencesDataFile.dat` 用 `Box id=1`（空 `name`）表示自由桌面容器、用 `<Box_ai_cloud>` 表示云端盒子；`40e47c1` 未过滤直接导入为可见 `FenceSpec`。
- **修复方案**：在 `legacy_to_native` 跳过 `b.tag != "Box" || (b.id == 1 && b.name.trim().is_empty())`，并在 `NativeLayoutConfig::sanitize_legacy_containers`（由 `load_or_bootstrap_layout` 调用）清理已落盘的 `id == 1` 空名称残留。

<a id="bug-03"></a>
### #03 手写 D2D 窗体无法拖动、缺折叠钮、Win95 弹窗粗糙与无法拖出
- **提交 / 模块**：`3c7bbe9` · 删除 `fence_window.rs`/`rename_dialog.rs`，新增 [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[fonts.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fonts.rs)、[drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)
- **现象**：格子拖不动、折叠只能靠菜单、重命名弹窗呈 Win95 原生控件且符号缺字变方框、文件只能拖入不能拖出。
- **技术根因**：`40e47c1` 自绘 Win32 窗体同时启用 `WS_EX_NOACTIVATE`、`MA_NOACTIVATE` 与 `HTCAPTION` 阻断了系统拖拽/双击消息流，且未实现 OLE `IDataObject`/`IDropSource`。
- **修复方案**：整体迁移至 `eframe 0.37` / `egui 0.37`（`ViewportCommand::StartDrag`/`BeginResize`、矢量绘制折叠/切换/菜单按钮、行内 `TextEdit`、`msyh.ttc` 中文字体），并在 `drag_out.rs` 通过 `start_file_drag_out`（`FileDropDataObject` 封装 `CF_HDROP` `HGLOBAL` + `SimpleDropSource` + `OleGuard`）接入原生 OLE `DoDragDrop`。

<a id="bug-04"></a>
### #04 `Alt+Tab` 残留、DWM 幽灵 `[×]`、跨线程 WIC 失败、半透明菜单与文字重叠
- **提交 / 模块**：`3c7bbe9` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[icons.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/icons.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：`DeskLite Host` / `DeskLite#<id>` 出现在 `Alt+Tab` 切换器、窗体右上角出现 DWM 幽灵关闭按钮发光、跨线程提取图标失败、弹出菜单透明以及网格文件名与下排图标重叠。
- **技术根因**：`eframe`/`winit` 默认保留 `WS_CAPTION | WS_SYSMENU | WS_EX_APPWINDOW`；`IconCache` 跨 COM 套间共享单个 `IWICImagingFactory`；`Popup::menu` 复用透明 `panel_fill`。
- **修复方案**：
  - `strip_alt_tab_from_host` 与 `attach_desktop_owner_and_rounded_corners` 剥离 `WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_EX_APPWINDOW`，设置 `WS_POPUP | WS_EX_TOOLWINDOW` 并挂接 `GWLP_HWNDPARENT` 至 `WorkerW`/`Progman`（`main.rs` 在首帧后 100ms 复核）。
  - `icons.rs` 将 `IWICImagingFactory` 迁入 `TLS_WIC`（`with_thread_wic` + `COINIT_APARTMENTTHREADED`）。
  - `fence_view.rs` 设不透明 `window_fill` 并以 `fit_two_lines` 做两行省略截断。

<a id="bug-05"></a>
### #05 右上角缩放热区被标题按钮保留宽度阻断
- **提交 / 模块**：`c6c8b67` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：拖动格子右上角或靠右的上边缘时无法缩放或明显卡顿。
- **技术根因**：`detect_resize_edge` 中显式排除了 `pos.y - rect.min.y < COLLAPSED_HEIGHT && rect.max.x - pos.x < HEADER_BTN_RESERVE_W`（92px 标题按钮保留区）。
- **修复方案**：移除 `detect_resize_edge` 中的 `HEADER_BTN_RESERVE_W` 排除判断。

<a id="bug-06"></a>
### #06 网格图标与文字左偏未居中
- **提交 / 模块**：`c6c8b67` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：网格模式下的条目文字和图标明显偏左，未沿单元格水平居中。
- **技术根因**：`render_grid_item` 调用 `ui.painter().layout(...)` 未指定居中对齐锚点。
- **修复方案**：改用 `LayoutJob::simple(..., Align::Center)` 并在 `rect.center().x.round()` 处居中绘制。

<a id="bug-07"></a>
### #07 右键 / 三点菜单失焦不自动关闭
- **提交 / 模块**：`c6c8b67` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：在格子上打开 `⋮` 菜单或右键菜单后，点击桌面或其它软件时菜单不会关闭。
- **技术根因**：`AlwaysOnBottom` 次级 Viewport 内的 `egui::Popup` 在 Win32 跨窗口失焦时不自动收到关闭信号。
- **修复方案**：引入 `PopupOwnerState` / `SharedFenceState::popup_owners`，监听 `Event::WindowFocused(false)` 且校验 `foreground_window_handle() != my_hwnd` 时立即关闭菜单。

<a id="bug-08"></a>
### #08 解绑 Folder Portal 后旧映射条目残留
- **提交 / 模块**：`c6c8b67` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：点击“取消文件夹映射”后 `portal_dir` 清空，但格子内仍残留原本映射的文件图标。
- **技术根因**：`render_fence_menu_items` 只置空了 `f.portal_dir = None`。
- **修复方案**：取消映射时同步清空 `f.items.clear()` 与 `guard.selected_item.remove(&fence_id)`，并在 `BindPortalResult` / `RequestDelete` 通过 `restore_desktop_icon_async` 归还原桌面图标。

<a id="bug-09"></a>
### #09 切焦点闪白边、`AlwaysOnBottom` 失效与 `Win+D` Z-Order 掉层
- **提交 / 模块**：`c6c8b67` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)
- **现象**：格子间切换焦点闪现白色边框；格子偶发压在普通窗口之上；按 `Win+D` 后有概率把格子最小化或压入 `WorkerW` 之后。
- **技术根因**：
  1. `bottom_subclass_proc` 无条件传 `HWND_BOTTOM`（与 `WorkerW` / 普通窗口层打架），而 Win11 下 `WorkerW` 位于 Z 序 ~510–518 处，超出最初 512 窗口遍历上限；`Win+D` 会在 `GetDesktopWindow()`（`#32769`）触发 `EVENT_OBJECT_REORDER`。
  2. DWM 默认绘制激活边框色与过渡动画。
- **修复方案**：
  - 设置 `DWMWA_BORDER_COLOR = DWMWA_COLOR_NONE`、`DWMWA_TRANSITIONS_FORCEDISABLED = true`，转发 `WM_NCACTIVATE`（`LPARAM(-1)`），并在 `WM_SYSCOMMAND` 拦截 `SC_MINIMIZE`。
  - 废除盲目 `HWND_BOTTOM`，实现 `find_desktop_layer_anchor` / `is_strictly_in_desktop_layer`（`GetTopWindow` + `GW_HWNDNEXT` 2048 上限遍历，跳过 `DWMWA_CLOAKED` 与系统壳窗口）和 `install_desktop_z_win_event_hooks`（`SetWinEventHook` 捕获 `EVENT_OBJECT_REORDER` 后投递 `WM_APP_ENSURE_DESKTOP_Z`）。

<a id="bug-10"></a>
### #10 原地桌面隐藏与 `LVS_OWNERDATA` 级联误删 / 精确坐标回放
- **提交 / 模块**：`c6c8b67` · [desktop_listview.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/desktop_listview.rs)、[drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)、[shell_ops.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_ops.rs)
- **现象**：文件拖入格子后桌面原位置图标仍同时存在；从格子拖回桌面会落到自动排序槽位而非鼠标松手点；曾尝试用 `LVM_DELETEITEM` + `WM_COMMAND(0x7103)` 刷新隐藏，结果拖入 1 个图标触发全员同时隐藏、拖出 1 个导致全员恢复。
- **技术根因**：Win11 桌面 `SysListView32` 启用了虚拟列表风格 `LVS_OWNERDATA`（`0x1000`），`LVM_DELETEITEM(idx)` 会忽略 `idx` 直接从末尾裁项，而 `0x7103`（F5 刷新）会整表重建；标准 `DoDragDrop` 回桌面时由 Explorer 按自动网格放置。
- **修复方案**：
  - 在 `DESKTOP_LV_SYNC` 互斥保护下，改为对单个路径发送 `SHChangeNotify(SHCNE_DELETE / SHCNE_RMDIR, SHCNF_PATHW | SHCNF_FLUSH, ...)`（避开 >10 项合并为 `UPDATEDIR` 导致重刷）；恢复时发 `SHCNE_CREATE / SHCNE_MKDIR` 并通过 `screen_to_listview_point`（`ScreenToClient` + `LVM_GETORIGIN` + DPI）与 `set_icon_position`（`LVM_SETITEMPOSITION32`）精确落位。
  - `SimpleDropSource::QueryContinueDrag` 通过 `is_desktop_point` + `hit_listview_item_index`（`LVM_HITTEST`）识别桌面空白处释放并返回 `DragOutOutcome::DroppedOnDesktop { x, y }`（落在桌面文件夹或回收站上则不拦截）。

<a id="bug-11"></a>
### #11 外部编辑带 UTF-8 BOM 导致 `layout.json` 解析失败
- **提交 / 模块**：`c6c8b67` · [store.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/store.rs)
- **现象 / 根因**：部分 Windows 工具以带 UTF-8 BOM（`\u{feff}`）写回 `layout.json` 后 `serde_json::from_str` 报错失败。
- **修复方案**：`load_native_layout` 读取后先执行 `raw.trim_start_matches('\u{feff}')`。

---

<a id="sec-m4"></a>
## 3. M4 基线：日常交互完善与图形化参数面板（`36974fb`）

<a id="bug-12"></a>
### #12 `ScrollArea` 灰白渐变阴影与边缘留白偏移
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：半透明格子上下出现发灰渐变遮罩且内容区左右边距与边框不贴合。
- **技术根因**：`egui::style::ScrollStyle` 默认在面板上下绘制渐变 fade，且 `body_frame` 垂直内边距偏大。
- **修复方案**：在 `configure_egui_theme` 配置 `ScrollStyle::floating()`、`fade.strength = 0.0`、`fade.size = 0.0`、`bar_width = 5.0`，收口 `body_frame` 为 `Margin::symmetric(8, 0)` 并增加 1px 标题栏底部分隔线。

<a id="bug-13"></a>
### #13 桌面映射 Portal 格图标未在原桌面隐藏
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[desktop_listview.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/desktop_listview.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：将格子映射为桌面文件夹时，图标在格子内与原生桌面上同时出现。
- **技术根因**：`collect_managed_desktop_paths` 显式用 `.filter(|f| f.portal_dir.is_none())` 跳过了 Portal 格子，绑定/删除 Portal 时也未同步更新隐藏与恢复。
- **修复方案**：去掉 `.filter(|f| f.portal_dir.is_none())`，并补齐 `hide_desktop_icons_for_paths_async` / `hide_desktop_icons_for_paths_delayed` 与 `restore_desktop_icons_async` 联动。

<a id="bug-14"></a>
### #14 拖动格子边缘拉伸概率性卡死
- **提交 / 模块**：`36974fb` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：拖拽格子四边/四角调整尺寸时概率性触发进程无响应（Not Responding）。
- **技术根因**：`ViewportCommand::BeginResize` 进入 Win32 模态 `WM_ENTERSIZEMOVE..WM_EXITSIZEMOVE` 循环后密集触发 `EVENT_OBJECT_REORDER` 与 `WM_WINDOWPOSCHANGING`，UI 线程在其中反复跨进程调用 `DwmGetWindowAttribute(DWMWA_CLOAKED)` 遍历全桌面窗口并因 `SetWindowPos` 自我重入形成消息风暴。
- **修复方案**：新增 `IN_SIZE_MOVE_HWND` 与 `DEFERRED_Z_AFTER_SIZEMOVE`，在 `WM_ENTERSIZEMOVE..WM_EXITSIZEMOVE` 期间延后 Z-Order 重排；加入 `SubclassEntry::pending_z` 合并及 `LAST_SELF_Z_MS`（60ms 自触发抑制）；缩放热区改为原生 DPI 换算 `WM_NCHITTEST`（`hit_test_resize_border`）。

<a id="bug-15"></a>
### #15 拖到屏幕边缘回弹并闪出原生 `DeskLite#<id>` 标题框
- **提交 / 模块**：`36974fb` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：把格子拖到屏幕边缘时发生回弹，且瞬间闪出带系统边框和 `DeskLite#<编号>` 的原生标题框。
- **技术根因**：`ViewportCommand::StartDrag`（`HTCAPTION`）触发 Windows Aero Snap 贴边/最大化状态机，进而促使 `winit` 在内部 `set_window_flags` 重新写入 `WS_CAPTION | WS_BORDER | WS_SYSMENU | WS_MAXIMIZEBOX` 并伴随 `SWP_FRAMECHANGED`。
- **修复方案**：在 `bottom_subclass_proc` 拦截 `WM_STYLECHANGING` 永久剔除 `DISALLOWED_STYLES` / `DISALLOWED_EX_STYLES`；`WM_NCCALCSIZE` / `WM_NCPAINT` 返回 `LRESULT(0)`；拦截 `SC_MAXIMIZE | SC_MINIMIZE | SC_RESTORE`；`WM_MOVING` / `WM_SIZING` 调 `clamp_rect_to_cursor_work_area` 限制在当前屏幕工作区内；标题栏拖拽改用纯光标位移 `move_window_no_z` + `snap_move_rect`。

<a id="bug-16"></a>
### #16 折叠后拖动格子概率回弹
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：点击折叠格子后紧接着拖动位置，概率出现回弹至原处。
- **技术根因**：`title_drag` 结束当帧的矩形同步读取到滞后 1 帧的 `outer_rect`，且 `toggle_collapse` 回写了过时 `builder_rects` 导致 `ViewportBuilder::patch` 补发旧 `OuterPosition`。
- **修复方案**：新增 `title_drag_ended_this_frame` 标记与实时 `window_outer_rect(my_hwnd)` 采样；在 `toggle_collapse` 设置 450ms `pending_collapse_until` 保护窗口并停止写入过期 `builder_rects`。

<a id="bug-17"></a>
### #17 网格条目行内重命名挤偏右侧相邻图标
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：在格子顶部对某图标行内重命名时，右侧同排图标被撑偏错位。
- **技术根因**：`egui::TextEdit` 直接内嵌在 `egui::Grid` 单元格内，文本框撑宽了整列布局宽度。
- **修复方案**：`handle_rename_text_edit` 将 `TextEdit` 迁入独立浮层 `egui::Area::new(...).order(egui::Order::Foreground).fixed_pos(edit_rect.min).constrain_to(clip_rect)`。

<a id="bug-18"></a>
### #18 桌面映射格重命名/取消导致全部隐藏桌面图标重新显现
- **提交 / 模块**：`36974fb` · [shell_ops.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_ops.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：对着桌面映射格内文件触发重命名并取消（或名称未改），桌面上所有已被隐藏的原生图标全部重新释放显现。
- **技术根因**：输入框失焦即使名称未改变仍调用了 `rename_file_in_place`，后者触发 `notify_shell_dir_updated` 向桌面目录发送 `SHCNE_UPDATEDIR`，迫使 Explorer `CDefView` 重建列表从而洗掉已隐藏项。
- **修复方案**：`notify_shell_dir_updated` 当 `is_desktop_directory(dir)` 为真时直接返回；`commit_item_rename` 在新旧文件名一致时短路退出。

<a id="bug-19"></a>
### #19 折叠态菜单被 38px 窗体裁切 → 次级 GL 窗闪框及 100% 拖拽卡死
- **提交 / 模块**：`36974fb` · [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：最初折叠后菜单被 `38px` 高度格子遮挡；改为 `desklite_collapsed_menu` 次级 GL Viewport 后，出现左上角闪白窗、菜单样式与展开态不统一，以及**先折叠再点开菜单不收回直接拖标题栏时 100% 进程卡死**。
- **技术根因**：`desklite_collapsed_menu` 在首帧定位前即显示，且关闭/移出时触发 `eframe` 的 `remove_viewports_not_in` 销毁仍绑定的 WGL Surface，导致后续 `glow_integration.rs` 调用 `make_current`/`make_not_current` 发生 `.unwrap()` panic 并被 `winit` 的 `catch_unwind` 吞掉，彻底锁死事件循环。
- **修复方案**：彻底废弃 `desklite_collapsed_menu` GL 窗体，在 `main.rs` 统一改用原生 Win32 `CreatePopupMenu`/`TrackPopupMenuEx`（`HelperState::show_fence_menu` + `WM_APP_SHOW_FENCE_MENU`/`WM_APP_SHOW_FENCE_MENU_BODY` + `uxtheme.dll` 序号 135/136 深色模式适配），展开与折叠态样式完全一致。

<a id="bug-20"></a>
### #20 `Ctrl+C`/`Ctrl+V` 偶发失灵、剪贴板陈旧与需连按两次
- **提交 / 模块**：`36974fb` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：选中文件按 `Ctrl+C`/`Ctrl+V` 有概率不生效、粘贴成上一次复制的文件，或需要连续按 2–3 次才能成功。
- **技术根因**：
  1. `ViewportCommand::Focus` 会触发 `winit` 注入 `SendInput(VK_LMENU)`（`Alt`），导致 `!i.modifiers.alt` 校验偶发失败。
  2. 中文输入法活跃时快捷键以 `VK_PROCESSKEY`（`0xE5`）下发，被 `winit`/`egui` 忽略。
  3. `OpenClipboard(None)` 单次尝试易与 Windows 剪贴板历史争抢锁失败。
  4. 同帧内 `Select` 选中更新晚于快捷键判定执行。
- **修复方案**：
  - 用 `focus_window_now`（`SetForegroundWindow`）替代 `ViewportCommand::Focus`。
  - 在 `bottom_subclass_proc` 的 `WM_KEYDOWN` 直接拦截 `Ctrl+C/V/Z`（`decode_ctrl_hotkey`，过滤 bit-30 连发，用 `MapVirtualKeyW(..., MAPVK_VSC_TO_VK_EX)` 还原 `VK_PROCESSKEY`，避让 `ACTIVE_TEXT_EDIT_HWND`），通过 `take_win32_hotkeys` + `RedrawWindow` 派发并禁用 `native_options.clipboard_shortcuts`。
  - `set_clipboard_files_for_window(my_hwnd, ...)` 绑定明确窗口句柄并增加 `open_clipboard_with_retry` 退避重试。
  - 确保 `Select`/`ContextSelect` 优先于热键动作执行，并接入 `UndoAction` 撤消栈支持 `Ctrl+Z`。

<a id="bug-21"></a>
### #21 标题多余 `[文件夹]` 后缀、文字下沉与空白区点击/右键失效
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：映射格子标题后附加 `[文件夹]`、标题文字在标题栏内垂直偏下、点击格子内空白处不能取消文件选中、在格子体内空白处右键无法唤出格子菜单。
- **技术根因**：`format_fence_title` 拼接了 `" [文件夹]"`；标题上下内边距不对称；`ScrollArea` 内部吞掉了空白区域的点击与右键事件。
- **修复方案**：`format_fence_title` 直接返回 `spec.name`；调整标题对称边距（`top: 6, bottom: 6`）与 `title_rect.center().y - 0.5`；在 `ScrollArea` **之前**注册覆盖客户区的 `blank_resp = ui.interact(viewport_clip_rect, ("blank_body", fence_id), Sense::click_and_drag())`，统一承担左键清选、右键弹 `WM_APP_SHOW_FENCE_MENU_BODY` 及驱动 `MarqueeState` 框选。

<a id="bug-22"></a>
### #22 网格图标偏上、固定尺寸不随格宽缩放、全局排版不联动与浅蓝边框
- **提交 / 模块**：`36974fb` · [fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[icons.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/icons.rs)
- **现象**：大格宽/格高下图标显著偏上、图标本身维持小尺寸不随格子等比放大显得突兀、调全局宽高时只有当前聚焦格子立即刷新、格子边缘可见一圈浅蓝色描边。
- **技术根因**：`render_grid_item` 写死 `rect.min.y + 6.0` 与 `40.0` 尺寸且图标上限受传统提取限制；修改全局参数时未广播 `request_repaint_all_fences`；Portal 格子沿用蓝色高亮边框。
- **修复方案**：新增 `grid_item_metrics`（按格宽/格高在 `28..=96px` 动态计算图标尺寸并在格子内垂直居中 `icon_sz + gap + text_h`）；`icons.rs` 增加 `SHIL_JUMBO` 高清图标提取（`load_sys_image_list_icon` + `has_pixels_outside_top_left`）；全局参数变更触发 `request_repaint_all_fences`；统一玻璃态中性 `CornerRadius::same(8)` 描边。

<a id="bug-23"></a>
### #23 参数设置面板：点 `×` 假死、半屏裁切与顶部白线、拉伸无实时读数、滑块卡顿、名称禁尾空格、缺单格独立排版
- **提交 / 模块**：`36974fb`（含后续 clippy 清理 `SizeMoveRectEntry`）· [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)
- **现象**：共 6 个并发症状：① 点 `×` 关闭设置页直接导致全进程无响应；② 设置窗口不可缩放、右侧/底部内容被裁切且顶部有 1px 白线；③ 拖动格子边缘时设置页 `X/Y/宽/高` 不实时更新；④ 透明度和排版滑块卡顿无平滑动画；⑤ 格子名称字尾不能输入空格；⑥ 缺少与全局解耦、且能随当前格子网格/列表模式自动切换的单格排版参数。
- **技术根因**：
  1. 用 `ViewportCommand::Close` 关闭 `"desklite_inspector"` 会触发 `eframe` 的 `remove_viewports_not_in` 销毁 GL 上下文死锁（同 `#19`）。
  2. 设置窗未接入 `WM_NCCALCSIZE`/`WM_NCPAINT`/`WM_NCACTIVATE` 子类化且初始 `344x468` 过窄。
  3. `WM_MOVING`/`WM_SIZING` 未向设置窗回传实时矩形。
  4. 多窗开启 `vsync = true` 导致跨窗 `SwapBuffers` 串行阻塞，且每像素拖动都唤醒 `ROOT`。
  5. `name_buf.trim()` 在每次击键（`changed()`）时立即执行。
- **修复方案**：
  - 保持 `"desklite_inspector"` 持久挂载（`inspector_spawned`），`close_inspector_safely` 与 `close_requested()` 改发 `ViewportCommand::Visible(false)` + `CancelClose`。
  - 新增 `style_floating_inspector_window` + `inspector_subclass_proc` 消除顶部白线，扩至 `408×568` 并允许 `.with_resizable(true)`。
  - `WM_MOVING`/`WM_SIZING` 实时写入 `ACTIVE_SIZEMOVE_RECT`（`live_sizemove_rect_for_hwnd`）并 `RedrawWindow` 唤醒 `INSPECTOR_HWND`。
  - 关闭多窗 `vsync`，加入 `ctx.animate_value_with_time`（`0.12s`–`0.14s`）与批量纹理查询。
  - 名称改为仅在 `lost_focus()` / 关闭设置页时执行 `trim()`；`layout.rs` 新增 `FenceLayoutOverride` 与 `resolve_fence_layout` 实现单格独立且随视图模式自动切换的排版卡片。

---

<a id="sec-routeb"></a>
## 4. Route B 架构重构与桌面全接管（`9b126a3` → `6377c23` → `1673c9e` → `d78aa1e` → `044c902`）

<a id="bug-24"></a>
### #24 桌面单文件多格子归属冲突与遗留 `BoxId=1` 自由图标丢失
- **提交 / 模块**：`9b126a3` · [layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)、[convert.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-legacy/src/convert.rs)
- **现象**：同一桌面文件可同时存在于多个分组格子与 `free_icons` 中；导入旧版配置时 `box_id == 1` 的自由桌面图标被丢弃；旧文件归档失败时静默忽略错误。
- **技术根因**：`FenceSpec` 缺少显式角色区分与全局唯一归属不变量，`legacy_to_native` 忽略了 `box_id == 1` 条目。
- **修复方案**：新增 `FenceRole`（`DesktopBucket` / `ExternalPortal` / `UnfiledDesktop`）、`normalize_path_key` 与 `NativeLayoutConfig::{enforce_desktop_single_ownership, assign_desktop_items_to_bucket, release_desktop_items, reconcile_free_icons}`；`legacy_to_native` 通过 `reconcile_free_icons` 导入 `box_id == 1` 并向上传播 `fs::rename` 错误。

<a id="bug-25"></a>
### #25 任务栏自动隐藏 / 调尺寸误触发显示器拓扑切换
- **提交 / 模块**：`9b126a3` · [topology.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/topology.rs)
- **现象**：切换任务栏自动隐藏或调整任务栏高度会导致桌面布局被当成新显示器拓扑重建。
- **技术根因**：`compute_topology_key` 将 `m.work_area.{left, top, right, bottom}` 计入了 FNV-1a 哈希。
- **修复方案**：从 `compute_topology_key` 移除 `work_area`，仅保留 `device_key`、`dpi_x`、`dpi_y`、`is_primary` 与 `rect`。

<a id="bug-26"></a>
### #26 首次切入未见过的多屏 / DPI 拓扑丢失相对位置与缩放
- **提交 / 模块**：`9b126a3` · [layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)
- **现象**：切换到新显示器组合或 DPI 时，格子坐标只被生硬 clamp 且 `free_icons` 全部清空。
- **技术根因**：`apply_topology` 在 `snapshots.get(&key)` 未命中时缺乏从上一活跃快照迁移的逻辑。
- **修复方案**：新增 `migrate_rect_for_unseen_topology`、`migrate_point_for_unseen_topology` 与 `scale_by_dpi`，按 `device_key` 匹配屏幕、按工作区相对位置及 DPI 比例迁移格子和 `free_icons`。

<a id="bug-27"></a>
### #27 同进程并发原子写文件冲突、`.tmp` 泄漏与脏边界持久化
- **提交 / 模块**：`9b126a3` · [store.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/store.rs)、[layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-cli/src/main.rs)
- **现象**：多线程并发写配置碰撞同一 `.tmp.<pid>` 路径、出错后残留 `.tmp`、非法倒置/越界尺寸在加载后持续存在；非正常退出后缺乏手动恢复桌面图标命令。
- **技术根因**：临时文件名仅依赖 PID 且错误路径无清理；加载 JSON 未校验偏好与矩形边界。
- **修复方案**：引入 `TMP_WRITE_SEQ: AtomicU64` 与失败清理 `fs::remove_file(&tmp_path)`；`load_native_layout` 调用 `sanitize_bounds`；CLI 新增 `desklite restore-desktop`。

<a id="bug-28"></a>
### #28 单项 `SHCNE_DELETE` 竞态、对话框 `SHELLDLL_DefView` 误抓、延迟隐藏被覆盖与同步恢复阻塞
- **提交 / 模块**：`6377c23` · [desktop_listview.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/desktop_listview.rs)
- **现象**：单项 `SHCNE_DELETE` 与 Explorer 异步刷新竞争导致图标闪现/漏删；`probe_desktop_window_tree` 回退枚举可能误抓打开的文件夹或系统文件选择对话框；连续触发延迟隐藏时旧批次被丢弃；退出时同步 `SHCNF_FLUSH` 可能卡住进程。
- **技术根因**：在 `LVS_OWNERDATA` 桌面与 Explorer 争夺单个条目状态；`EnumWindows` 未过滤资源管理器/对话框类名；`DELAYED_HIDE_GEN` 仅保留最后一代计数。
- **修复方案**：新增 `set_shell_desktop_icons_visible(visible)` 对桌面 `SHELLDLL_DefView` 整窗 `SW_HIDE`/`SW_SHOW` 彻底避开虚拟列表逐项竞态；树枚举过滤 `"CabinetWClass" | "ExploreWClass" | "#32770"`；以 `DELAYED_HIDE_QUEUE`（`Condvar` 合并路径）取代旧代数；退出恢复使用 `PostMessageW` + `SHCNF_FLUSHNOWAIT`。

<a id="bug-29"></a>
### #29 `FreeDesktop` 画布与分组格子 Z-Order 互搏及 `WorkerW` 句柄过期
- **提交 / 模块**：`6377c23` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)
- **现象**：全屏 `FreeDesktop` 画布可能覆盖分组格子、在 Explorer 重启后丢失桌面附着，或意外响应边缘拖拽缩放。
- **技术根因**：所有子类化窗口共用同一置底层级，且 `resolve_live_owner` 先信缓存 `IsWindow(owner_hwnd)` 而未重探新 `WorkerW`。
- **修复方案**：分离 `FREE_DESKTOP_HWND` 专属层级，严格维持 `普通软件 > DeskLite 分组格 > FreeDesktop 画布 > WorkerW/Progman`；`resolve_live_owner` 优先实探并在变更时补发 `WM_APP_ENSURE_DESKTOP_Z`；对 `is_free` 禁用 `WM_MOVING`/`WM_SIZING`/`WM_ENTERSIZEMOVE`/`WM_NCHITTEST`。

<a id="bug-30"></a>
### #30 全局 `Mutex` 中毒、`IN_SIZE_MOVE_HWND` 卡死与事件监听窗内存泄漏/唤醒丢失
- **提交 / 模块**：`6377c23` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[events.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/events.rs)
- **现象**：任一工作线程 panic 会连带中毒全局互斥锁；取消拖拽后 `IN_SIZE_MOVE_HWND` 未清零导致 Z-Order 永久停摆；`run_event_monitor` 销毁时泄漏 `MonitorState` 且漏接锁屏解锁 / 显示器唤醒信号。
- **技术根因**：直接 `.lock().unwrap()`；缺少 `WM_CAPTURECHANGED | WM_CANCELMODE` 兜底；未注册电源/会话通知及 `WM_NCDESTROY` 回收。
- **修复方案**：统一改为 `lock_unpoisoned`；加入 `WM_CAPTURECHANGED | WM_CANCELMODE` 与 `is_primary_mouse_down()` 纠偏；`events.rs` 补齐 `RegisterPowerSettingNotification`、`WTSRegisterSessionNotification`、`TRANSIENT_RETRY_TIMER_ID` 重试、`Box::into_raw` + `WM_NCDESTROY` 释放与 `UnregisterClassW`。

<a id="bug-31"></a>
### #31 同步 `DoDragDrop` 阻塞 UI 线程与 `IDataObject` / 按键松开漏检
- **提交 / 模块**：`6377c23` · [drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)
- **现象**：文件拖出期间 UI 主线程冻结；部分目标程序因 `GetCanonicalFormatEtc`、`lindex == 0` 校验瑕疵拒收数据，或因鼠标释放消息被吃掉导致拖拽卡住不结束。
- **技术根因**：在 UI 主线程同步调用 OLE `DoDragDrop`；`IDataObject`/`IDropSource` 实现不完整。
- **修复方案**：新增 `start_file_drag_out_async` 迁至独立 `"desklite-drag-out"` STA 线程并通过 `PostMessageW` 回传 `DragTargetOutcome`；完善 `GetCanonicalFormatEtc`、`lindex == 0`、`MAX_GET_DATA_CALLS = 256` 及 `QueryContinueDrag` 对 `!is_primary_mouse_down_async()` 的兜底检测。

<a id="bug-32"></a>
### #32 CCD 多目标缓冲竞态、`FolderWatcher` 断连、`IconCache` 无限增长与自复制/回收站中断异常
- **提交 / 模块**：`6377c23` · [display.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/display.rs)、[folder_watch.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/folder_watch.rs)、[icons.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/icons.rs)、[shell_ops.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_ops.rs)
- **现象**：切屏并发期 `QueryDisplayConfig` 偶发缓冲不足且克隆屏覆盖目标；目录监听断连后无法自愈；图标缓存持续膨胀；文件夹往自身内部粘贴/移动导致无限递归；路径带尾斜杠导致回收站删除失败或误报取消状态。
- **修复方案**：
  - `query_ccd_device_map` 针对 `ERROR_INSUFFICIENT_BUFFER` 最多重试 5 次并合并同源多目标；
  - `FolderWatcher::start_multi` 支持最多 63 目录联合监听与 `250ms..4000ms` 指数退避重连；
  - `IconCache` 增设 LRU 上限淘汰（`DEFAULT_MAX_CACHE_ENTRIES = 384`）与 `invalidate_path`；
  - `shell_ops.rs` 增加 `is_same_or_descendant` 防递归、拒绝 `.`/`..`/尾点重命名、去除尾斜杠并兼容 `FOF_NOERRORUI` 与 `1223`/`0x75` 用户取消码。

<a id="bug-33"></a>
### #33 `explorer.exe` 重启（`TaskbarCreated`）后托盘消失与原生桌面图标重现
- **提交 / 模块**：`1673c9e` · [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：任务管理器重启 `explorer.exe` 后 DeskLite 托盘图标丢失，同时原生桌面图标重新覆盖在桌面之上。
- **技术根因**：未监听 `"TaskbarCreated"` 广播消息，且 `TrayIcon` 生命周期未托管在 `HelperState` 中。
- **修复方案**：注册 `RegisterWindowMessageW(w!("TaskbarCreated"))`，收到消息后重建 `TrayIcon::new`、再次隐藏 `SHELLDLL_DefView` 并启动 `SHELL_VERIFY_TIMER_1/2` 校验，触发 `post_reassert_all_fences_z()`（同步加装 `install_panic_restore_hook` 与 `WM_ENDSESSION` 恢复）。

<a id="bug-34"></a>
### #34 切屏/唤醒抖动期瞬态坐标污染快照与实时窗口未同步（M5 Live Sync）
- **提交 / 模块**：`1673c9e` · [state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：拔插显示器或唤醒去抖的几百毫秒内，Windows 临时将副屏格子挤回主屏，`render_fence_viewport` 误把该瞬态位置反写进旧快照；拓扑切完后部分现存窗口未即时同步到目标新坐标。
- **技术根因**：矩形同步只检查 `!any_pointer_down && !is_in_size_move()`，缺乏显示变更稳态期锁定；`BgEvent::TopologyChanged` 后未主动迁移在屏 HWND。
- **修复方案**：新增 `begin_topology_settling(3500ms)` / `is_topology_settling` 冻结 `live_phys_rect` 反写，并在拓扑生效时调用 `move_live_windows_to_topology`（`set_window_rect_no_z`）即时归位实时窗体。

<a id="bug-35"></a>
### #35 `HelperState` 模态文件夹选择器重入风险与全局锁中毒
- **提交 / 模块**：`1673c9e` · [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象 / 根因**：直接在 `helper_wnd_proc` 内阻塞调用模态 `IFileOpenDialog` 存在向 `&mut HelperState` 重入及界面卡死风险。
- **修复方案**：将 `spawn_folder_picker_for_new_portal` / `spawn_folder_picker_cmd` 拆到独立 STA 线程执行，并全量替换为 `lock_unpoisoned`。

<a id="bug-36"></a>
### #36 `desklite_free_desktop` 全屏 WGL 黑屏遮罩与 `UnfiledDesktop` 图标重复
- **提交 / 模块**：`d78aa1e` · [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)
- **现象**：启用全虚拟桌面尺度的 `"desklite_free_desktop"` 透明 Viewport 后壁纸变黑，且当存在 `UnfiledDesktop` 格子时自由图标重复渲染两次。
- **技术根因**：`main.rs` 无条件创建全屏 WGL 透明画布；同时 `apply_transparent_canvas_chrome` 对全屏 GL 透明窗误调 `DwmExtendFrameIntoClientArea(-1)` 引发 DWM/WGL 合成黑块。
- **修复方案**：在 `d78aa1e` 对 `desklite_free_desktop` 增加存在性与空集门控，并将 `DwmExtendFrameIntoClientArea` 从 `apply_transparent_canvas_chrome` 拆离（随后在 `#39` 彻底重写该画布）。

<a id="bug-37"></a>
### #37 内部文件拖拽跨套间 COM `SendMessage` 死锁（Application Hang）
- **提交 / 模块**：`d78aa1e` · [drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)
- **现象**：在 DeskLite 内部跨格子或向桌面拖动文件时程序直接死锁挂起（Application Hang）。
- **技术根因**：`start_file_drag_out_async` 在后台 STA 线程直接启动 OLE `DoDragDrop`。当鼠标经过主 UI STA 线程拥有的同进程 `winit` `IDropTarget` 窗口时，OLE 内部通过跨套间同步 COM `SendMessage` 回调被阻塞中的主线程，形成典型跨 STA 互等死锁。
- **修复方案**：将拖拽拆为两阶段——**Phase 1** 在指针仍位于 DeskLite 格子 / `free_desktop_hwnd` / 原生桌面范围内时使用纯 Win32 轮询（`GetAsyncKeyState`、`GetCursorPos`、`WindowFromPoint`、`GetAncestor`）内部直投完成；仅当光标移出至外部软件窗口（`enter_external_ole`）时才进入 **Phase 2** 初始化 OLE `DoDragDrop`。

<a id="bug-38"></a>
### #38 多 Viewport 串联 `vsync` 导致帧率严重卡顿
- **提交 / 模块**：`d78aa1e`（注：最早亦在 M4 参数面板发现，后被 `1673c9e` 改回 `true` 复发并再次定位修复）· [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：多个格子同时绘制或动画滑动时整体帧率暴跌、拖影明显。
- **技术根因**：`eframe` 在单 UI 线程中轮流为 `ROOT` + 所有格子 Viewport 执行 WGL `SwapBuffers`；开启 `vsync: true` 时每个窗口串行阻塞一个 VBlank（~16.6ms），N 个窗口同帧重绘即跌至 `60 / N` FPS。
- **修复方案**：在 `main()` 中显式配置 `native_options.glow_options.vsync = false;`。

<a id="bug-39"></a>
### #39 `UpdateLayeredWindow` + Direct2D `FreeDesktop` 彻底根治 WGL 黑屏/闪屏与 `UnfiledDesktop` 冲突
- **提交 / 模块**：`044c902` · [free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[layout.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-core/src/layout.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：基于 `eframe`/WGL 的全屏自由桌面画布在创建与改尺寸时依然脆弱且开销大；同时保留 `UnfiledDesktop` 格子与桌面 Portal 导致与自由桌面画布双源冲突。
- **技术根因**：全屏 WGL SwapChain 存在冷启动未首帧闪白与 DWM 合成不确定性；`FenceRole::UnfiledDesktop` 与 `portal_dir == Desktop` 在模型层和自由桌面功能重叠。
- **修复方案**：
  - 用独立 STA Win32 窗口（`WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`）+ Direct2D `ID2D1DCRenderTarget` 离屏 32bpp 预乘 DIB + `UpdateLayeredWindow(..., ULW_ALPHA)` 替代 `eframe` 自由桌面画布，并在 `ShowWindow(SW_SHOWNOACTIVATE)` 之前先提交 `state.repaint()` 消除首帧闪烁，背景填 `HITTEST_ALPHA` 确保透明区域不穿透失焦。
  - 在 `layout.rs` / `main.rs` 将旧 `UnfiledDesktop` 与绑定桌面的 Portal 统一归并为 `DesktopBucket`，移除创建入口并弹窗禁止将桌面绑定为普通 Portal（`show_desktop_portal_forbidden_dialog`）。

<a id="bug-40"></a>
### #40 唤醒未挂载/隐藏 `egui` Viewport 触发窗体闪现
- **提交 / 模块**：`044c902` · [state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[actions.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/actions.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[inspector.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/inspector.rs)、[undo.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/undo.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：在全部隐藏格子、设置页关闭或格子刚删除时，后台事件触发刷新会令已隐藏/卸载的 Viewport 短暂闪出屏幕。
- **技术根因**：无条件调用 `ctx.request_repaint_of(ViewportId::from_hash_of(...))` 唤醒了未挂载的 `eframe` deferred viewport。
- **修复方案**：实现 `SharedFenceState::{fence_viewport_is_mounted, request_repaint_fence, request_repaint_inspector, mark_free_desktop_dirty}`，仅当格子已挂载（`!hide_all_fences && fences.contains(fid)`）或 `inspector_spawned == true` 时才派发重绘，自由桌面脏标记改用 `ViewportId::ROOT` + `WM_APP_REFRESH_FREE_DESKTOP`。

<a id="bug-41"></a>
### #41 文件拖出缺预览图（`DragGhost` + `IDragSourceHelper2`）与跨边框缩放光标闪烁
- **提交 / 模块**：`044c902` · [drag_ghost.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_ghost.rs)、[drag_out.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/drag_out.rs)、[icons.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/icons.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)
- **现象**：内部文件拖拽时鼠标旁没有图标缩略图（多文件也没有数量角标），且拖拽经过格子边框时鼠标闪烁为双向缩放箭头。
- **技术根因**：Phase 1 内部拖拽未走 Explorer OLE 绘制逻辑；Phase 2 `FileDropDataObject::SetData` 直接返回 `E_NOTIMPL` 阻断 `IDragSourceHelper2::InitializeFromBitmap`；`hit_test_resize_border` / `resize_dir` 未感知拖拽状态。
- **修复方案**：新增分层置顶穿透预览窗 `DragGhost`（`render_drag_preview_dib` + `extract_file_icon_pbgra`）用于 Phase 1，同时为 Phase 2 补齐 `FormatListEnum`、`FileDropDataObject::SetData`（托管 `extra_data` 释放）与 `initialize_drag_source_helper`；引入 `FILE_DRAG_ACTIVE` 在文件拖拽期屏蔽 `hit_test_resize_border` / `resize_dir` 并在 `WM_SETCURSOR` 锁定 `IDC_ARROW`。

<a id="bug-42"></a>
### #42 多区域焦点选中残余与右键/内联编辑/设置窗误清空 + 拖拽边缘阈值偏差
- **提交 / 模块**：`044c902` · [state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：多个格子与自由桌面可同时残留选中态；点击外部应用或空白处时未统一清选，但右键菜单、重命名或点开设置窗反而意外清除了选中；条目拖到边框内 `2px` 处才触发跨格拖出，左右窄留白区点不中空白区。
- **技术根因**：各视图各自维护孤立的选中集合且缺少全局焦点域豁免判据；`inside_fence` 与 `blank_hit_rect` 几何阈值过窄。
- **修复方案**：引入 `SelectionScope`、`clear_all_selections_except` 与 `is_focus_clear_exempt`（通过 `MenuPopupActiveGuard`、`is_popup_menu_hwnd`、内联重命名及 `inspector_hwnd` 豁免清选，跨窗口/桌面 `WM_ACTIVATE`/`WM_KILLFOCUS` 统一清选）；将 `inside_fence` 收紧至 `win_rect.shrink(RESIZE_EDGE + 1.0)` 并向左右各扩展 `8.0` 空白点击区。

<a id="bug-43"></a>
### #43 程序启动/退出瞬间格子闪现纯白矩形框（DWM/WGL 首帧与销毁竞态）
- **提交 / 模块**：`feat/route-b-desktop-takeover` · [main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)、[fence_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/fence_view.rs)、[state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)
- **现象**：打开程序及关闭退出程序瞬间，所有格子先以不透明纯白矩形框闪出原型或卡一下再消失。
- **技术根因**：
  1. 启动时 `eframe` 仅对 `ROOT` 做了首帧隐藏，子 `Viewport` 在未执行 `ensure_fence_attached`（`DWMWA_SYSTEMBACKDROP_TYPE` / `DWMWA_TRANSITIONS_FORCEDISABLED`）及首帧 OpenGL `swap_buffers` 前就已 `with_visible(true)` 直接显示在屏幕目标坐标，导致 DWM 先画出原生白底窗口。
  2. 退出时 `begin_shutdown` / `on_exit` / `WM_ENDSESSION` 先执行布局存盘与恢复原生桌面图标，再在 `WS_VISIBLE` 状态下销毁 GL 上下文与关闭 Viewport，GL Surface 卸载瞬间退化为纯白背景框。
- **修复方案**：
  - 引入 `SharedFenceState::pending_reveal_fences` 离屏首帧护栏：新建格子初始以 `(-32000, -32000)` 离屏创建并屏蔽期间 `live_phys_rect -> spec.rect` 回写，待 `ensure_fence_attached` 挂载 DWM Acrylic Chrome 且完成首帧 `swap_buffers` 后再以 `set_window_rect_no_z` 无动画归位并恢复 Z-Order。
  - 新增 `hide_all_fences_immediately()` 与 `ShowWindowAsync(..., SW_HIDE)`，在 `begin_shutdown` / `on_exit` / `WM_ENDSESSION` 最前端同步隐去所有格子、检查面板与 `FreeDesktop` 画布，再执行存盘与退出。

<a id="bug-44"></a>
### #44 `FreeDesktop` 自绘桌面图标顶置对齐/毛坯感、失焦不清选、悬停缩放光标残留与缺热键
- **提交 / 模块**：`feat/route-b-desktop-takeover` · [free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：与此前格子内 `#06`/`#20`/`#22`/`#41`/`#42` 高度同源的自绘桌面回归——图标贴紧格子上沿（未垂直居中）、选中态呈生硬纯蓝方框与文字毛糙、点外部软件后桌面上选中的图标不取消聚焦、鼠标移入桌面概率残留双向拉伸光标、桌面选中后快捷键未接管。
- **技术根因**：
  1. `draw_icon_cell` 硬编码 `icon_top = ly + 6.0` 且固定 `4.0` 圆角/高饱和边框，未使用 `IDWriteTextLayout` 实测两行文本高度进行内容块垂直居中，且层叠透明目标未设 `D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE`。
  2. `WNDCLASSEXW.hCursor` 为 `NULL` 且 `free_desktop_wnd_proc` 未处理 `WM_SETCURSOR`，从格子 6px 缩放边缘移出到 `FreeDesktop` 时系统沿用上一个拉伸光标。
  3. `CreateWindowExW` 携带 `WS_EX_NOACTIVATE` 且未主动同步前台焦域，导致点击桌面后其它软件或桌面切焦不会触发 `WM_ACTIVATE(WA_INACTIVE)` / `FocusLost`；同时 `EVENT_SYSTEM_FOREGROUND` 未向上层派发外部切焦事件。
- **修复方案**：
  - 按 `item_grid::grid_item_metrics` 等比尺度重做 `draw_icon_cell`（`free_cell_metrics` + `fit_two_lines_dwrite` 实测 `lineCount <= 2` 与文本高度），垂直居中 `icon + gap + text` 内容块，采用 `6px` 圆角柔和玻璃高亮、`0.5px` 像素对齐描边与灰度文本抗锯齿；
  - 为 `WNDCLASSEXW` 配置 `IDC_ARROW`，并在 `free_desktop_wnd_proc` 与 `bottom_subclass_proc` 显式响应 `WM_SETCURSOR`；
  - 移除 `WS_EX_NOACTIVATE` 并在点击时调用 `SetForegroundWindow`，叠加 `EVENT_SYSTEM_FOREGROUND` → `WM_APP_EXTERNAL_FOREGROUND` → `BgEvent::ExternalForeground` 跨进程外部切焦清选，并在 `clear_all_selections_except` 清理 `free_selected_paths` 时同步置脏并投递 `WM_APP_REFRESH_FREE_DESKTOP`；同时为 `free_desktop_wnd_proc` 补齐 `WM_KEYDOWN`（含 `MAPVK_VSC_TO_VK_EX` IME 兼容与 `Ctrl+A/C/Shift+C/X/V/Z`、`F2`/`F5`/`Delete`/`Esc`）。

<a id="bug-45"></a>
### #45 `FreeDesktop` 左键单击、空白点击与框选被 `ReleaseCapture()` 同步 `WM_CAPTURECHANGED` 静默吞掉
- **提交 / 模块**：`feat/route-b-desktop-takeover` · [free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)
- **现象**：在自绘桌面中左键单击图标仅显示悬停态而无法选中、左键点桌面空白不触发清选、左键拉出框选框松手后不提交选区（只有右键能触发选中）。
- **技术根因**：`handle_lbutton_up` 先调用了 `ReleaseCapture()` 再 `std::mem::replace(&mut state.drag_state, MouseDragState::None)`；而 `ReleaseCapture()` 在返回前会同步向同线程窗口派发 `WM_CAPTURECHANGED`，将 `state.drag_state` 清为 `MouseDragState::None`，导致 `handle_lbutton_up` 取到空状态并直接跳过事件派发。
- **修复方案**：在 `handle_lbutton_up` 中先 `std::mem::replace(&mut state.drag_state, MouseDragState::None)` 取出待处理状态，再调用 `ReleaseCapture()`，确保左键点击、空白清选与框选提交稳定生效。

<a id="bug-46"></a>
### #46 `FreeDesktop` 键盘焦点与热键断链、DPI 格距不统一、D2D 渲染粗糙、`vsync` 回归及跨格粘贴/撤销补漏
- **提交 / 模块**：`feat/route-b-desktop-takeover` · [window_attach.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/window_attach.rs)、[shell_ops.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/shell_ops.rs)、[free_desktop_wnd.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-win32/src/free_desktop_wnd.rs)、[state.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/state.rs)、[inspector.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/inspector.rs)、[free_desktop_view.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/free_desktop_view.rs)、[actions.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/actions.rs)、[undo.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-ui/src/undo.rs)、[main.rs](file:///E:/Ai_Agent/DeskLite/crates/desklite-app/src/main.rs)
- **现象**：
  1. 自绘桌面点击后仅调用 `SetForegroundWindow` 未调用 `SetFocus`，且 `bottom_subclass_proc` 的 `WM_KEYDOWN` 拦截了 `Ctrl+A/C/X/V/Z`、`WM_CHAR` 误调 `DefSubclassProc`，导致桌面热键、`Enter`/`Alt+Enter`/`Shift+Del`/方向键/`Home`/`End`/首字母定位及右键菜单关闭后键盘焦点断链；
  2. 高 DPI 下 `FreeDesktop` 未乘主屏缩放比例导致与格子图标大小不一致，拖动参数面板滑块时不联动缩放桌面格距且 `vsync = true` 意外回归引发多 Viewport 卡顿（`#38`）；
  3. 桌面图标 256px 一次性下采样有锯齿、选框在亮壁纸下偏生硬、Win95 白色边框 `EDIT` 与无 Toast/不换图标缓存（`icon_epoch`）；
  4. `drain_events_and_commands` 仅挂在 `App::ui()` 在 `PassMode::LogicOnly` 下跳过；外部文件复制到 `DesktopBucket` 未进桌面目录、从 `ExternalPortal` 剪切未刷源格、`UndoAction::PasteFiles` 撤销未归位桌面 `free_icons`。
- **技术根因**：`bottom_subclass_proc` 缺少 `!is_free` 守卫；`free_desktop_wnd_proc` 缺少 `WM_ACTIVATE`/`WM_SYSKEYDOWN`/`WM_CTLCOLOREDIT` 与菜单关闭焦域归还；`FreeDesktopSnapshot` 缺少 `toast`/`icon_epoch`；`eframe 0.37` 的 `PassMode::LogicOnly` 只调 `App::logic()` 不调 `App::ui()`。
- **修复方案**：
  - `bottom_subclass_proc` 放行 `FreeDesktop` 的 `WM_KEYDOWN`/`WM_SYSKEYDOWN`；`free_desktop_wnd_proc` 补齐 `WM_ACTIVATE` + `SetFocus`、`WM_SYSKEYDOWN`（`Alt+Enter` 属性）、`Enter` 打开、`Shift+Delete` 永久删除（`delete_files_permanently`）、方向键/`Home`/`End` 空间导航与 `WM_CHAR`（修为 `DefWindowProcW`）850ms 首字母定位，并在右键菜单关闭后归还 `focus_window_now(free_desktop_hwnd)`；
  - 新增 `SharedFenceState::free_desktop_cell_size()` 与 `rescale_free_icons_for_cell_size()` 统一物理像素尺寸与实时缩放联动，恢复 `native_options.glow_options.vsync = false`；
  - `free_desktop_wnd.rs` 实现 `downsample_pbgra_hq`、磨砂钴蓝选框与双层文字阴影、暗色圆角无边框行内重命名、`draw_toast_pill`（1200ms 定时自动消隐）及 `icon_epoch` 缓存失效重载；
  - 将 `drain_events_and_commands` 迁入 `eframe::App::logic()`，同时补全 `actions.rs`/`undo.rs` 跨格粘贴与撤销归位闭环（`cut_paths` 统一 `normalize_path_key`）。

---

<a id="sec-takeaways"></a>
## 5. 跨 Bug 提炼的四条核心工程经验

1. **`eframe` 多 Viewport 的两大地雷**：销毁仍绑定的 GL Viewport（`remove_viewports_not_in`）会触发 `make_not_current` 死锁/崩溃，多窗开启 `vsync = true` 会把主 UI 线程逐窗串联堵成 `60 / N` FPS。解法是持久化挂载 + `Visible(false)`，以及全屏纯桌面背景直接脱离 `eframe`/WGL 改用 `UpdateLayeredWindow` + Direct2D。
2. **Win11 `SysListView32` (`LVS_OWNERDATA`) 的边界**：虚拟列表桌面环境下逐项 `LVM_DELETEITEM` 与整目录 `SHCNE_UPDATEDIR` 极易与 Explorer 异步重绘竞态并触发整表重显；整窗隐藏 `SHELLDLL_DefView`（并严格过滤 `CabinetWClass` / `#32770`）加 `TaskbarCreated` 兜底才是零闪烁稳态方案。
3. **同进程 COM/OLE 跨套间互等死锁**：OLE `DoDragDrop` 位于背景 STA 线程却扫过主 UI STA 线程托管的 `IDropTarget` 窗口时必死锁；必须先用无 COM 依赖的 Win32 指针命中完成内部拖拽，离开本进程范围再降级切入 OLE。
4. **Windows 无边框置底窗口的系统干涉**：Windows Aero Snap、`WM_WINDOWPOSCHANGING` 与 `winit` 的标志重同步随时可能改回 `WS_CAPTION`、触发原生边框或改变 Z-Order，必须依靠 `WM_STYLECHANGING`、`WM_NCCALCSIZE`、`IN_SIZE_MOVE_HWND` 抑制重入以及 `is_strictly_in_desktop_layer` 自上而下锚定 `WorkerW`。
