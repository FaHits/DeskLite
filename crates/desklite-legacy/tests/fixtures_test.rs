use std::path::PathBuf;

use desklite_core::{DisplayTopology, FenceRole, MonitorDescriptor, Rect};
use desklite_legacy::{
    LegacyBundle, LegacyOptions, build_fences_xml, legacy_to_native, load_legacy_dir,
    repair_legacy_dir, save_legacy_dir,
};
use desklite_legacy::fences_xml::FencesXmlModel;
use desklite_legacy::records::{LegacyBoxRecord, LegacyItemRecord};

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

#[test]
fn legacy_to_native_imports_free_desktop_items_and_clamps_boxes() {
    let topo = demo_primary_topology();
    let bundle = LegacyBundle {
        xml: FencesXmlModel {
            boxes: vec![
                LegacyBoxRecord {
                    tag: "Box".into(),
                    id: 1,
                    name: String::new(),
                    rect: Rect::new(0, 0, 2560, 1440),
                    reserved_6_7: [0, 0],
                    kind: 3,
                    reserved_9_22: [0; 14],
                    tail: String::new(),
                },
                LegacyBoxRecord {
                    tag: "Box".into(),
                    id: 101,
                    name: "Work".into(),
                    rect: Rect::new(2700, 100, 3000, 400),
                    reserved_6_7: [0, 0],
                    kind: 3,
                    reserved_9_22: [0; 14],
                    tail: String::new(),
                },
            ],
            screens: vec![],
            items: vec![
                LegacyItemRecord {
                    tag: "Item".into(),
                    display_name: "Bucketed.lnk".into(),
                    box_id: 101,
                    order: 0,
                    kind: 2,
                    path: r"C:\Users\Alice\Desktop\Bucketed.lnk".into(),
                    reserved: 0,
                },
                LegacyItemRecord {
                    tag: "Item".into(),
                    display_name: "FreeOne.lnk".into(),
                    box_id: 1,
                    order: 1,
                    kind: 2,
                    path: r"C:\Users\Alice\Desktop\FreeOne.lnk".into(),
                    reserved: 0,
                },
                LegacyItemRecord {
                    tag: "Item".into(),
                    display_name: "FreeTwo.txt".into(),
                    box_id: 1,
                    order: 0,
                    kind: 4,
                    path: r"C:\Users\Alice\Desktop\FreeTwo.txt".into(),
                    reserved: 0,
                },
            ],
        },
        con: None,
    };

    let native = legacy_to_native(&bundle, Some(&topo)).unwrap();
    assert_eq!(native.fences.len(), 1);
    assert_eq!(native.fences[0].role, Some(FenceRole::DesktopBucket));
    // Box 101 was off-screen to the right and clamped onto demo_primary_topology.
    assert_eq!(native.fences[0].rect, Rect::new(2260, 100, 2560, 400));

    let free_icons = native.active_free_icons();
    assert_eq!(free_icons.len(), 2);
    // Sorted by XML order: FreeTwo.txt (order 0) then FreeOne.lnk (order 1).
    assert_eq!(free_icons[0].display_name, "FreeTwo.txt");
    assert_eq!(free_icons[1].display_name, "FreeOne.lnk");
    assert_ne!(free_icons[0].pos, free_icons[1].pos);
}
