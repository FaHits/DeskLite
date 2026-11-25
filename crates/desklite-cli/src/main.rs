use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use desklite_core::{
    Point, Rect, clamp_point_to_monitors, load_native_layout, save_native_layout,
};
use desklite_legacy::{
    LegacyOptions, legacy_to_native, load_legacy_dir, repair_legacy_dir,
};
use desklite_win32::{
    DisplayEventKind, FencePocConfig, enable_per_monitor_dpi_v2, find_desktop_listview,
    probe_desktop_window_tree, probe_display_topology, run_bottom_fence_poc, run_event_monitor,
};

#[derive(Parser)]
#[command(name = "desklite", version, about = "DeskLite M1/M2 CLI & Win11 probe")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect legacy DeskGo FencesDataFile.dat and ConFile.dat
    LegacyInspect {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        keep_unknown_boxes: bool,
    },
    /// Check or repair out-of-bounds boxes in legacy DeskGo files
    LegacyFix {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        stop_deskgo: bool,
        #[arg(long)]
        clean_temps: bool,
        #[arg(long)]
        keep_unknown_boxes: bool,
    },
    /// Convert legacy DeskGo config into modern DeskLite layout.json
    ImportDeskgo {
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        keep_unknown_boxes: bool,
    },
    /// Validate and optionally clamp a native layout.json against current displays
    ValidateNative {
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
    },
    /// Probe active display topology via CCD QueryDisplayConfig + DPIv2
    ProbeDisplays,
    /// Listen to Win32 display/DPI/power events (0% idle CPU loop)
    WatchEvents {
        #[arg(long)]
        layout: Option<PathBuf>,
    },
    /// Enumerate desktop SysListView32 icons and check work-area bounds
    ProbeIcons,
    /// Safely move one desktop icon by (dx, dy), verify, wait, and restore it
    TestIconMove {
        #[arg(long)]
        index: usize,
        #[arg(long, default_value_t = 80)]
        dx: i32,
        #[arg(long, default_value_t = 0)]
        dy: i32,
        #[arg(long, default_value_t = 1000)]
        hold_ms: u64,
    },
    /// Launch a Direct2D + Win11 DWM Acrylic bottom-most PoC fence window
    PocFence {
        #[arg(long, default_value = "DeskLite PoC Fence (Alt+F4 / Close to exit)")]
        title: String,
        #[arg(long, default_value_t = 80)]
        left: i32,
        #[arg(long, default_value_t = 80)]
        top: i32,
        #[arg(long, default_value_t = 340)]
        width: i32,
        #[arg(long, default_value_t = 240)]
        height: i32,
    },
}

fn default_deskgo_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("Tencent").join("DeskGo")
}

fn default_desklite_layout() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("DeskLite").join("layout.json")
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let dpi_v2 = enable_per_monitor_dpi_v2();
    let cli = Cli::parse();

    match cli.command {
        Commands::LegacyInspect {
            dir,
            keep_unknown_boxes,
        } => {
            let dir = dir.unwrap_or_else(default_deskgo_dir);
            let opts = LegacyOptions {
                keep_unknown_boxes,
                clean_temps: false,
            };
            let bundle = load_legacy_dir(&dir, &opts)?;
            println!("Legacy dir: {}", dir.display());
            println!(
                "Boxes: {} | Screens: {} | Items: {}",
                bundle.xml.boxes.len(),
                bundle.xml.screens.len(),
                bundle.xml.items.len()
            );
            for b in &bundle.xml.boxes {
                println!(
                    "  [{}] id={} name={:?} kind={} rect=({}, {}, {}, {}) {}x{}",
                    b.tag,
                    b.id,
                    b.name,
                    b.kind,
                    b.rect.left,
                    b.rect.top,
                    b.rect.right,
                    b.rect.bottom,
                    b.rect.width(),
                    b.rect.height()
                );
            }
            if let Some(con) = &bundle.con {
                println!(
                    "ConFile streams: {} (box_count={}, item_count={}, version={})",
                    con.streams.len(),
                    con.get_int("CFG_FENCESBOX_COUNT", -1),
                    con.get_int("CFG_FENCESITEM_COUNT", -1),
                    con.get_int("CFG_FENCES_CONFIGVERSION", -1)
                );
            }
        }
        Commands::LegacyFix {
            dir,
            apply,
            stop_deskgo,
            clean_temps,
            keep_unknown_boxes,
        } => {
            let dir = dir.unwrap_or_else(default_deskgo_dir);
            let topo = probe_display_topology()?;
            if apply && stop_deskgo {
                let _ = Command::new("taskkill")
                    .args(["/IM", "DesktopMgr64.exe", "/F"])
                    .output();
                thread::sleep(Duration::from_millis(300));
            }
            let opts = LegacyOptions {
                keep_unknown_boxes,
                clean_temps,
            };
            let report = repair_legacy_dir(&dir, &topo, &opts, apply)?;
            println!(
                "Topology: {} | Apply: {} | Boxes: {} | Stripped unknown: {} | Clamped: {}",
                topo.key(),
                apply,
                report.total_boxes,
                report.stripped_unknown_boxes,
                report.clamped_boxes.len()
            );
            for (id, name, before, after) in &report.clamped_boxes {
                println!(
                    "  clamped id={id} ({name:?}): ({},{},{},{}) -> ({},{},{},{})",
                    before.left,
                    before.top,
                    before.right,
                    before.bottom,
                    after.left,
                    after.top,
                    after.right,
                    after.bottom
                );
            }
            if !report.cleaned_temp_files.is_empty() {
                println!("Archived temp files: {:?}", report.cleaned_temp_files);
            }
        }
        Commands::ImportDeskgo {
            from,
            out,
            keep_unknown_boxes,
        } => {
            let from = from.unwrap_or_else(default_deskgo_dir);
            let out = out.unwrap_or_else(default_desklite_layout);
            let opts = LegacyOptions {
                keep_unknown_boxes,
                clean_temps: false,
            };
            let bundle = load_legacy_dir(&from, &opts)?;
            let topo = probe_display_topology().ok();
            let mut native = legacy_to_native(&bundle, topo.as_ref())?;
            if let Some(ref t) = topo {
                let mig = native.apply_topology(t, (72, 72))?;
                println!(
                    "Applied live topology {} (clamped_fences={})",
                    mig.topology_key, mig.clamped_fences
                );
            }
            save_native_layout(&out, &native)?;
            println!(
                "Exported modern layout (schema v{}, {} fences) -> {}",
                native.schema_version,
                native.fences.len(),
                out.display()
            );
        }
        Commands::ValidateNative { file, apply } => {
            let file = file.unwrap_or_else(default_desklite_layout);
            let mut native = load_native_layout(&file)?;
            let topo = probe_display_topology()?;
            let mig = native.apply_topology(&topo, (72, 72))?;
            println!(
                "Native layout {} -> topology={} restored={} clamped_fences={} clamped_icons={}",
                file.display(),
                mig.topology_key,
                mig.restored_from_snapshot,
                mig.clamped_fences,
                mig.clamped_icons
            );
            if apply {
                save_native_layout(&file, &native)?;
                println!("Saved updated snapshot atomically.");
            }
        }
        Commands::ProbeDisplays => {
            let topo = probe_display_topology()?;
            let vb = topo.virtual_bounds();
            println!("Per-Monitor DPIv2 enabled: {dpi_v2}");
            println!("TopologyKey: {}", topo.key());
            println!(
                "VirtualBounds: ({}, {}, {}, {}) {}x{}",
                vb.left,
                vb.top,
                vb.right,
                vb.bottom,
                vb.width(),
                vb.height()
            );
            for (i, m) in topo.monitors().iter().enumerate() {
                println!(
                    "  [{i}] primary={} name={:?} dpi={}x{} rect=({},{},{},{}) work=({},{},{},{}) key={}",
                    m.is_primary,
                    m.friendly_name,
                    m.dpi_x,
                    m.dpi_y,
                    m.rect.left,
                    m.rect.top,
                    m.rect.right,
                    m.rect.bottom,
                    m.work_area.left,
                    m.work_area.top,
                    m.work_area.right,
                    m.work_area.bottom,
                    m.device_key
                );
            }
        }
        Commands::WatchEvents { layout } => {
            println!("Listening for display/DPI/power events (Ctrl+C to exit)...");
            let mut native_cfg = layout
                .as_ref()
                .map(|p| load_native_layout(p))
                .transpose()?;
            run_event_monitor(move |ev| {
                match ev {
                    DisplayEventKind::InitialTopology(topo) => {
                        println!("[INIT] topology={} monitors={}", topo.key(), topo.monitors().len());
                        if let (Some(cfg), Some(path)) = (&mut native_cfg, &layout)
                            && let Ok(rep) = cfg.apply_topology(&topo, (72, 72))
                        {
                            let _ = save_native_layout(path, cfg);
                            println!("  synced layout: {:?}", rep);
                        }
                    }
                    DisplayEventKind::TopologyChanged { reason, topology } => {
                        println!(
                            "[EVENT:{reason}] topology={} monitors={}",
                            topology.key(),
                            topology.monitors().len()
                        );
                        if let (Some(cfg), Some(path)) = (&mut native_cfg, &layout)
                            && let Ok(rep) = cfg.apply_topology(&topology, (72, 72))
                        {
                            let _ = save_native_layout(path, cfg);
                            println!("  migrated & flushed layout: {:?}", rep);
                        }
                    }
                    DisplayEventKind::ZeroMonitorTransientIgnored { reason } => {
                        println!("[IGNORED] 0-monitor transient during {reason}");
                    }
                    DisplayEventKind::PowerSuspend => {
                        println!("[POWER] system suspending");
                    }
                    DisplayEventKind::SessionEnding => {
                        println!("[SESSION] ending - flushing state");
                        if let (Some(cfg), Some(path)) = (&native_cfg, &layout) {
                            let _ = save_native_layout(path, cfg);
                        }
                    }
                }
                true
            })?;
        }
        Commands::ProbeIcons => {
            let topo = probe_display_topology()?;
            let tree = probe_desktop_window_tree()
                .ok_or("failed to locate desktop Progman/SHELLDLL_DefView/SysListView32")?;
            println!(
                "Desktop HWND tree: progman={:?} worker_w={:?} def_view={:?} list_view={:?}",
                tree.progman.0,
                tree.worker_w.map(|w| w.0),
                tree.shell_def_view.0,
                tree.list_view.0
            );
            let lv = find_desktop_listview()?;
            let icons = lv.enumerate_icons()?;
            println!(
                "Explorer PID={} class={} count={}",
                lv.explorer_pid(),
                lv.listview_class_name(),
                icons.len()
            );
            let mut oob = 0usize;
            for ic in &icons {
                let (_, changed) = clamp_point_to_monitors(ic.pos, (32, 32), topo.monitors())?;
                if changed {
                    oob += 1;
                }
                println!(
                    "  [{:>3}] ({:>5}, {:>5}) {}{}",
                    ic.index,
                    ic.pos.x,
                    ic.pos.y,
                    ic.name,
                    if changed { " [OUT_OF_WORK_AREA]" } else { "" }
                );
            }
            println!("Out-of-work-area icons: {oob}");
        }
        Commands::TestIconMove {
            index,
            dx,
            dy,
            hold_ms,
        } => {
            let lv = find_desktop_listview()?;
            let orig = lv.get_icon_position(index)?;
            let moved = Point {
                x: orig.x + dx,
                y: orig.y + dy,
            };
            println!("Icon[{index}] original=({}, {})", orig.x, orig.y);
            lv.set_icon_position(index, moved)?;
            let after_move = lv.get_icon_position(index)?;
            println!(
                "Icon[{index}] moved=({}, {}) -> readback=({}, {})",
                moved.x, moved.y, after_move.x, after_move.y
            );
            thread::sleep(Duration::from_millis(hold_ms));
            lv.set_icon_position(index, orig)?;
            let restored = lv.get_icon_position(index)?;
            println!(
                "Icon[{index}] restored=({}, {})",
                restored.x, restored.y
            );
        }
        Commands::PocFence {
            title,
            left,
            top,
            width,
            height,
        } => {
            let cfg = FencePocConfig {
                title,
                rect: Rect::new(left, top, left + width, top + height),
            };
            println!(
                "Launching bottom-most Direct2D + DWM Acrylic PoC fence at ({}, {}, {}, {})...",
                cfg.rect.left, cfg.rect.top, cfg.rect.right, cfg.rect.bottom
            );
            run_bottom_fence_poc(cfg)?;
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
