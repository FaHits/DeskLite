<div align="center">
  <img src="assets/desklite-logo.png" width="96" height="96" alt="DeskLite Logo" />
  <h1>DeskLite</h1>
  <p>面向 Windows 11 的轻量级桌面栅格整理与自由桌面接管工具（Rust Edition 2024）</p>
</div>

---

## 核心特性

- **Route B 全桌面接管**：隐藏原生 Explorer 图标层并由内置 Direct2D + `UpdateLayeredWindow` 自绘 `FreeDesktop` 画布接管，支持任意空白双击隐藏/显示全部格子、自由拖拽与网格吸附、多屏工作区独立自适应。
- **分类格子 & 外部文件夹映射**：
  - **桌面分类格子**：将桌面文件/快捷方式收纳进透明圆角栅格，支持网格视图与紧凑列表视图平滑动画切换（170ms）、抽屉折叠/展开动画（155ms）、单格独立排版覆写或一键同步全局样式。
  - **外部文件夹映射（Portal）**：将任意磁盘目录映射为桌面格子，通过 `ReadDirectoryChangesW` 实时同步文件变动。
- **原生 Windows Explorer 混合右键菜单**：集成系统 `IContextMenu` / `IContextMenu2` / `IContextMenu3`（完整保留 WinRAR、7-Zip、Notepad++、QQ、百度网盘等第三方扩展，支持 Shift 扩展菜单与点击预构建加速），并在顶部无缝融合 `移动到格子`、`移出格子（放回桌面）` 与内联重命名拦截。
- **完整系统级交互与 Win11 层级治理**：
  - 支持多选、框选、格内拖拽排序、跨格子/桌面/资源管理器 OLE 拖放（自带高分辨率缩略图幽灵预览）。
  - 完整键盘快捷键：`Ctrl+C` / `Ctrl+X` / `Ctrl+V` / `Ctrl+Z` / `Ctrl+A`、`F2` 内联重命名、`Alt+Enter` 属性、`Delete` 移至回收站 / `Shift+Delete` 永久删除、`F5` 刷新与拼音/首字母键入定位。
  - 针对 Windows 11 优化 `Win+D` 恢复与单链 Z-order 治理（聚焦/拖动格子自动置顶且不遮挡普通应用窗口）、防焦点闪烁及退出自动恢复原生桌面图标。
- **DeskGo 3.0 配置迁移**：支持解析旧版 `FencesDataFile.dat` / `ConFile.dat`（RC4 + CFB + XML）并一键转换为现代 `layout.json`。

---

## 工作区结构

- `crates/desklite-core`：布局模型、多屏拓扑签名（`TopologyKey`）、工作区边界裁剪与原子 JSON 持久化。
- `crates/desklite-legacy`：DeskGo 3.0 兼容解码与迁移桥接工具。
- `crates/desklite-win32`：Win11 底层引擎（CCD 拓扑检测、Per-Monitor DPIv2、Direct2D `FreeDesktop` 画布、`IContextMenu3` 混合菜单、OLE 拖放、目录监听、系统托盘与 Z-order 治理）。
- `crates/desklite-ui`：格子视图、参数设置面板、撤销栈与桌面状态调度。
- `crates/desklite-app`：桌面端主程序（`desklite-app.exe`，内嵌应用图标、DPI 清单与版本资源）。
- `crates/desklite-cli`：命令行校验、迁移与桌面恢复工具（`desklite.exe`）。

---

## 编译与测试

```powershell
# 代码静态检查与全量测试
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 编译桌面端 Release（产物位于 target\release\desklite-app.exe）
cargo build --release -p desklite-app

# 命令行工具：恢复原生桌面图标（若异常强杀进程导致原生图标未恢复）
cargo run -p desklite-cli -- restore-desktop
```

---

## 后续演进路线

- [WIN11_D2D_MIGRATION_PLAN.md](docs/WIN11_D2D_MIGRATION_PLAN.md)：已规划将剩余格子窗与参数设置面板也迁移为纯 Win32 + Direct2D (`UpdateLayeredWindow`)，彻底移除 `eframe`/`egui`/OpenGL 依赖并将单文件体积进一步压缩至 ~1.5 MB。

## License

[MIT](LICENSE)
