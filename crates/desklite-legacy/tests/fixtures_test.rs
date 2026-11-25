use std::path::PathBuf;

use desklite_core::{DisplayTopology, MonitorDescriptor, Rect};
use desklite_legacy::{
    LegacyOptions, build_fences_xml, legacy_to_native, load_legacy_dir, repair_legacy_dir,
    save_legacy_dir,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("anonymized")
}

fn demo_primary_topology() -> DisplayTopology {
    DisplayTopology::try_from_monitors(vec![MonitorDescriptor {
        device_key: "DP-DEMO".into(),
        friendly_name: "Demo 2K".into(),
        rect: Rect::new(0, 0, 2560, 1440),
        work_area: Rect::new(0, 0, 2560, 1392),
        dpi_x: 96,
        dpi_y: 96,
        is_primary: true,
    }])
    .unwrap()
}

#[test]
fn anonymized_fixtures_roundtrip_and_strip_cloud_box() {
    let dir = fixture_dir();
    let keep_opts = LegacyOptions {
        keep_unknown_boxes: true,
        clean_temps: false,
    };
    let full = load_legacy_dir(&dir, &keep_opts).unwrap();
    assert_eq!(full.xml.boxes.len(), 4);
    assert_eq!(full.xml.items.len(), 3);

    let default_opts = LegacyOptions::default();
    let stripped = load_legacy_dir(&dir, &default_opts).unwrap();
    assert_eq!(stripped.xml.boxes.len(), 3);
    assert_eq!(stripped.xml.items.len(), 2);

    let xml = build_fences_xml(&stripped.xml);
    assert!(xml.ends_with("</root>\n\n\0"));

    let con = stripped.con.as_ref().unwrap();
    assert_eq!(con.get_int("CFG_FENCES_CONFIGVERSION", 0), 30);
    assert_eq!(
        con.get_raw("CFG_SETTING_PERIOD_REPORT"),
        Some([1, 2, 3, 4, 5, 6, 7, 8].as_slice())
    );
    assert_eq!(con.get_raw("TIPS_POPUP_TIME_002006").unwrap().len(), 112);

    let topo = demo_primary_topology();
    let native = legacy_to_native(&stripped, Some(&topo)).unwrap();
    // Hidden container box id=1 with empty name must not become a visible fence.
    assert_eq!(native.fences.len(), 2);
    assert!(native.fences.iter().all(|f| f.id != 1));
    assert_eq!(native.active_topology.as_ref(), Some(topo.key()));
}

#[test]
fn legacy_repair_clamps_out_of_bounds_box_and_writes_consistent_files() {
    let tmp = std::env::temp_dir().join(format!("desklite_legacy_fix_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    for name in ["ConFile.dat", "FencesDataFile.dat"] {
        std::fs::copy(fixture_dir().join(name), tmp.join(name)).unwrap();
    }

    let topo = demo_primary_topology();
    let opts = LegacyOptions::default();
    let report = repair_legacy_dir(&tmp, &topo, &opts, true).unwrap();
    assert_eq!(report.stripped_unknown_boxes, 1);
    assert_eq!(report.clamped_boxes.len(), 1);
    assert_eq!(report.clamped_boxes[0].0, 102);
    assert_eq!(
        report.clamped_boxes[0].3,
        Rect::new(2260, 120, 2560, 420)
    );

    // Reload and verify XML + ConFile box prefixes stay synchronized.
    let reloaded = load_legacy_dir(&tmp, &opts).unwrap();
    let con = reloaded.con.as_ref().unwrap();
    let con_boxes = con.boxes().unwrap();
    assert_eq!(reloaded.xml.boxes.len(), 3);
    assert_eq!(con_boxes.len(), 3);
    for (xb, cb) in reloaded.xml.boxes.iter().zip(con_boxes.iter()) {
        assert_eq!(xb.id, cb.id);
        assert_eq!(xb.name, cb.name);
        assert_eq!(xb.rect, cb.rect);
    }

    // Also test direct save_legacy_dir idempotence.
    save_legacy_dir(&tmp, &reloaded, &opts).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
}
