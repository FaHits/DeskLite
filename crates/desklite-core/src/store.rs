use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::layout::{CURRENT_SCHEMA_VERSION, NativeLayoutConfig};

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
    let tmp_path = path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()));

    {
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
    }

    if create_bak && path.exists() {
        let bak_path = path.with_file_name(format!("{file_name}.bak"));
        fs::copy(path, &bak_path).map_err(|source| StoreError::Io {
            path: bak_path,
            source,
        })?;
    }

    fs::rename(&tmp_path, path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

pub fn load_native_layout(path: &Path) -> Result<NativeLayoutConfig, StoreError> {
    let raw = fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: NativeLayoutConfig = serde_json::from_str(&raw)?;
    if cfg.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(cfg.schema_version));
    }
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
            portal_dir: None,
            backdrop_alpha: crate::layout::DEFAULT_BACKDROP_ALPHA,
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
}
