# DeskLite

Lightweight, low-footprint Windows 11 desktop fence & icon layout engine written in Rust (Edition 2024).

## Architecture (Phase 1: M1 + M2)

- `crates/desklite-core`: OS-agnostic domain model, display topology signature (`TopologyKey`), translate-first work-area clamping, and atomic JSON store (`layout.json`, `schema_version = 1`).
- `crates/desklite-legacy`: Isolated DeskGo 3.0 (`FencesDataFile.dat` / `ConFile.dat`) RC4+CFB+XML compatibility and one-shot migration bridge.
- `crates/desklite-win32`: Modern Windows 11 engine (`QueryDisplayConfig` CCD topology probe, Per-Monitor DPIv2, `Progman`/`WorkerW`/`SysListView32` cross-process icon read/write, event-driven `GetMessageW` monitor loop, and Direct2D + DWM Acrylic bottom-most fence PoC).
- `crates/desklite-cli`: Verification and migration CLI (`desklite.exe`).

## Build & Test

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p desklite-cli
```
