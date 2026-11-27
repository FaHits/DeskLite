use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_windows_sdk_rc() -> Option<PathBuf> {
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let kits_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if !kits_bin.is_dir() {
        return None;
    }

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };

    let mut versions: Vec<PathBuf> = std::fs::read_dir(kits_bin)
        .ok()?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| p.is_dir() && p.join(arch).join("rc.exe").is_file())
        .collect();
    versions.sort();
    versions.pop().map(|v| v.join(arch).join("rc.exe"))
}

fn main() {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets_dir = manifest_dir.join("../../assets");
    let rc_path = assets_dir.join("desklite.rc");
    let ico_path = assets_dir.join("desklite.ico");
    let app_manifest = assets_dir.join("desklite.manifest");

    println!("cargo:rerun-if-changed={}", rc_path.display());
    println!("cargo:rerun-if-changed={}", ico_path.display());
    println!("cargo:rerun-if-changed={}", app_manifest.display());

    let Some(rc_exe) = find_windows_sdk_rc() else {
        println!("cargo:warning=Windows SDK rc.exe not found; building without embedded icon");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let res_path = out_dir.join("desklite.res");
    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();

    match status {
        Ok(st) if st.success() => {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
        }
        Ok(st) => {
            panic!("rc.exe failed with exit code {:?}", st.code());
        }
        Err(err) => {
            panic!("failed to invoke {}: {err}", rc_exe.display());
        }
    }
}
