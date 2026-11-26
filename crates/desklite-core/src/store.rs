use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::layout::{CURRENT_SCHEMA_VERSION, NativeLayoutConfig};

static TMP_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O failure at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported layout schema version: {0} (expected {CURRENT_SCHEMA_VERSION})")]
    UnsupportedSchema(u32),
}

/// Writes `bytes` atomically:
/// write temp file in same directory -> flush + FlushFileBuffers (sync_all) -> optional .bak -> rename swap.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8], create_bak: bool) -> Result<(), StoreError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("desklite");
    let seq = TMP_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_file_name(format!("{file_name}.tmp.{}.{seq}", std::process::id()));

    let write_res = (|| -> Result<(), StoreError> {
        let mut f = File::create(&tmp_path).map_err(|source| StoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        f.write_all(bytes).map_err(|source| StoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        f.flush().map_err(|source| StoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        f.sync_all().map_err(|source| StoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if create_bak && path.exists() {
        let bak_path = path.with_file_name(format!("{file_name}.bak"));
        if let Err(source) = fs::copy(path, &bak_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(StoreError::Io {
                path: bak_path,
                source,
            });
        }
    }

    if let Err(source) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        });
    }

    Ok(())
}

pub fn load_native_layout(path: &Path) -> Result<NativeLayoutConfig, StoreError> {
    let raw = fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut cfg: NativeLayoutConfig = serde_json::from_str(raw.trim_start_matches('\u{feff}'))?;
    if cfg.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(cfg.schema_version));
    }
    cfg.sanitize_bounds();
    Ok(cfg)
}

pub fn save_native_layout(path: &Path, cfg: &NativeLayoutConfig) -> Result<(), StoreError> {
    let json = serde_json::to_vec_pretty(cfg)?;
    atomic_write_bytes(path, &json, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FenceKind, FenceSpec};
    use crate::topology::Rect;

    #[test]
    fn atomic_save_and_load_creates_backup() {
        let dir = std::env::temp_dir().join(format!("desklite_core_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("layout.json");

        let mut cfg = NativeLayoutConfig::default();
        cfg.fences.push(FenceSpec {
            id: 101,
            name: "Tools".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 400, 400),
            collapsed: false,
            locked: false,
            role: None,
            portal_dir: None,
            backdrop_alpha: crate::layout::DEFAULT_BACKDROP_ALPHA,
            layout_override: None,
            items: vec![],
        });
        save_native_layout(&target, &cfg).unwrap();
        assert!(target.exists());

        cfg.fences[0].name = "Tools2".into();
        save_native_layout(&target, &cfg).unwrap();
        assert!(dir.join("layout.json.bak").exists());

        let loaded = load_native_layout(&target).unwrap();
        assert_eq!(loaded, cfg);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_atomic_writes_do_not_collide_and_leave_no_tmp() {
        let dir =
            std::env::temp_dir().join(format!("desklite_core_conc_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("concurrent.bin");

        std::thread::scope(|s| {
            for t in 0..8 {
                let target_ref = &target;
                s.spawn(move || {
                    for iter in 0..10 {
                        let payload = format!("thread-{t}-iter-{iter}");
                        atomic_write_bytes(target_ref, payload.as_bytes(), false).unwrap();
                    }
                });
            }
        });

        let final_content = fs::read_to_string(&target).unwrap();
        assert!(final_content.starts_with("thread-"));
        let leftover_tmps: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover_tmps.is_empty(), "leftover tmp files: {leftover_tmps:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_native_layout_sanitizes_out_of_range_bounds() {
        let dir =
            std::env::temp_dir().join(format!("desklite_core_bounds_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("layout.json");

        let bad_json = r#"{
            "schema_version": 1,
            "active_topology": null,
            "preferences": {
                "icon_cell_w": 0,
                "icon_cell_h": -10,
                "snap_threshold_px": -5,
                "list_row_h": 2,
                "list_icon_px": 999
            },
            "fences": [{
                "id": 101,
                "name": "Bad",
                "kind": "icon_grid",
                "rect": { "left": 500, "top": 500, "right": 400, "bottom": 450 },
                "layout_override": {
                    "icon_cell_w": 5,
                    "icon_cell_h": 1000,
                    "list_row_h": 0,
                    "list_icon_px": 1
                },
                "items": []
            }],
            "snapshots": {}
        }"#;
        fs::create_dir_all(&dir).unwrap();
        fs::write(&target, bad_json).unwrap();

        let loaded = load_native_layout(&target).unwrap();
        assert_eq!(loaded.preferences.icon_cell_w, 24);
        assert_eq!(loaded.preferences.icon_cell_h, 24);
        assert_eq!(loaded.preferences.snap_threshold_px, 0);
        assert_eq!(loaded.preferences.list_row_h, 16);
        assert_eq!(loaded.preferences.list_icon_px, 96);
        assert!(loaded.fences[0].rect.width() >= crate::clamp::MIN_FENCE_WIDTH);
        assert!(loaded.fences[0].rect.height() >= crate::clamp::MIN_FENCE_HEIGHT);
        let ov = loaded.fences[0].layout_override.as_ref().unwrap();
        assert_eq!(ov.icon_cell_w, Some(24));
        assert_eq!(ov.icon_cell_h, Some(320));
        assert_eq!(ov.list_row_h, Some(16));
        assert_eq!(ov.list_icon_px, Some(12));

        let _ = fs::remove_dir_all(&dir);
    }
}
