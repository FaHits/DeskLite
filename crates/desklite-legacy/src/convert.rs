use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use desklite_core::{
    DisplayTopology, FenceItem, FenceKind, FenceRole, FenceSpec, ItemKind, NativeLayoutConfig,
    Point, Rect, TopologySnapshot, atomic_write_bytes, clamp_rect_to_monitors,
};

use crate::LegacyError;
use crate::con_file::{ConFile, read_con_file, write_con_file};
use crate::fences_xml::{FencesXmlModel, decrypt_fences_bytes, encrypt_fences_bytes};

#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyOptions {
    pub keep_unknown_boxes: bool,
    pub clean_temps: bool,
}

#[derive(Debug, Clone)]
pub struct LegacyBundle {
    pub xml: FencesXmlModel,
    pub con: Option<ConFile>,
}

#[derive(Debug, Clone)]
pub struct LegacyFixReport {
    pub total_boxes: usize,
    pub stripped_unknown_boxes: usize,
    pub clamped_boxes: Vec<(i32, String, Rect, Rect)>,
    pub cleaned_temp_files: Vec<String>,
}

pub fn load_legacy_dir(dir: &Path, opts: &LegacyOptions) -> Result<LegacyBundle, LegacyError> {
    let fences_path = dir.join("FencesDataFile.dat");
    let fences_raw = fs::read(&fences_path)?;
    let mut xml = decrypt_fences_bytes(&fences_raw)?;
    if !opts.keep_unknown_boxes {
        xml.strip_unknown_boxes();
    }

    let con_path = dir.join("ConFile.dat");
    let con = if con_path.exists() {
        let con_raw = fs::read(&con_path)?;
        Some(read_con_file(&con_raw)?)
    } else {
        None
    };

    Ok(LegacyBundle { xml, con })
}

pub fn save_legacy_dir(
    dir: &Path,
    bundle: &LegacyBundle,
    opts: &LegacyOptions,
) -> Result<Vec<String>, LegacyError> {
    let mut xml = bundle.xml.clone();
    if !opts.keep_unknown_boxes {
        xml.strip_unknown_boxes();
    }

    let fences_bytes = encrypt_fences_bytes(&xml);
    atomic_write_bytes(&dir.join("FencesDataFile.dat"), &fences_bytes, true)?;

    if let Some(con) = &bundle.con {
        let mut synced = con.clone();
        synced.sync_boxes_and_items(&xml.boxes, &xml.items);
        let con_bytes = write_con_file(&synced)?;
        atomic_write_bytes(&dir.join("ConFile.dat"), &con_bytes, true)?;
    }

    let mut cleaned = Vec::new();
    if opts.clean_temps {
        for name in [
            "FencesDataFile_temp",
            "FencesDataFile_temp_1",
            "FencesDataFile_temp_2",
            "FencesDataFile_temp_3",
            "FencesDataFile_temp_week",
            "FencesDataFile_temp_month",
            "FencesDataFile.dat.temp",
            "ConFile.dat.temp.temp",
        ] {
            let p = dir.join(name);
            if p.exists() {
                let archived = dir.join(format!("{name}.legacy_archived"));
                fs::rename(&p, &archived)?;
                cleaned.push(name.to_string());
            }
        }
    }

    Ok(cleaned)
}

pub fn repair_legacy_dir(
    dir: &Path,
    topology: &DisplayTopology,
    opts: &LegacyOptions,
    apply: bool,
) -> Result<LegacyFixReport, LegacyError> {
    let raw_count = {
        let fences_raw = fs::read(dir.join("FencesDataFile.dat"))?;
        decrypt_fences_bytes(&fences_raw)?.boxes.len()
    };

    let mut bundle = load_legacy_dir(dir, opts)?;
    let stripped_unknown_boxes = raw_count.saturating_sub(bundle.xml.boxes.len());

    let mut clamped_boxes = Vec::new();
    for b in &mut bundle.xml.boxes {
        let (clamped, changed) = clamp_rect_to_monitors(b.rect, topology.monitors())?;
        if changed {
            clamped_boxes.push((b.id, b.name.clone(), b.rect, clamped));
            b.rect = clamped;
        }
    }

    let cleaned_temp_files = if apply {
        save_legacy_dir(dir, &bundle, opts)?
    } else {
        Vec::new()
    };

    Ok(LegacyFixReport {
        total_boxes: bundle.xml.boxes.len(),
        stripped_unknown_boxes,
        clamped_boxes,
        cleaned_temp_files,
    })
}

pub fn legacy_to_native(
    bundle: &LegacyBundle,
    topology: Option<&DisplayTopology>,
) -> Result<NativeLayoutConfig, LegacyError> {
    let mut cfg = NativeLayoutConfig::default();

    for b in &bundle.xml.boxes {
        // DeskGo stores the entire free-desktop canvas as hidden box id=1 with an empty name,
        // and legacy cloud boxes as Box_ai_cloud. Never turn them into visible fences.
        if b.tag != "Box" || (b.id == 1 && b.name.trim().is_empty()) {
            continue;
        }
        let mut items = Vec::new();
        for it in bundle.xml.items.iter().filter(|it| it.box_id == b.id) {
            items.push(FenceItem {
                display_name: it.display_name.clone(),
                order: it.order,
                kind: ItemKind::from(it.kind),
                path: it.path.clone(),
            });
        }
        items.sort_by_key(|it| it.order);
        cfg.fences.push(FenceSpec {
            id: b.id,
            name: b.name.clone(),
            kind: FenceKind::from(b.kind),
            rect: b.rect,
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: desklite_core::DEFAULT_BACKDROP_ALPHA,
            layout_override: None,
            items,
        });
    }

    cfg.enforce_desktop_single_ownership(&[]);

    let mut free_legacy_items: Vec<FenceItem> = bundle
        .xml
        .items
        .iter()
        .filter(|it| it.box_id == 1)
        .map(|it| FenceItem {
            display_name: it.display_name.clone(),
            order: it.order,
            kind: ItemKind::from(it.kind),
            path: it.path.clone(),
        })
        .collect();
    free_legacy_items.sort_by_key(|it| it.order);

    if let Some(topo) = topology {
        let mut rects = BTreeMap::new();
        for f in &mut cfg.fences {
            let (clamped, _) = clamp_rect_to_monitors(f.rect, topo.monitors())?;
            f.rect = clamped;
            rects.insert(f.id, f.rect);
        }
        let key = topo.key().clone();
        cfg.active_topology = Some(key.clone());
        cfg.snapshots.insert(
            key.clone(),
            TopologySnapshot {
                topology_key: key,
                monitors: topo.monitors().to_vec(),
                fences: rects,
                free_icons: Vec::new(),
            },
        );
        cfg.reconcile_free_icons(
            &free_legacy_items,
            Point { x: 24, y: 24 },
            (cfg.preferences.icon_cell_w, cfg.preferences.icon_cell_h),
            topo.monitors(),
            &[],
        )?;
    }

    Ok(cfg)
}
