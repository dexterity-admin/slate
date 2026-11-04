//! Compiles res/slate.rc (icon, manifest, version info) with the Windows SDK
//! resource compiler and links the .res into the executable.

use std::path::PathBuf;
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SLATE_RC") {
        return Some(PathBuf::from(p));
    }
    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits).ok()?.flatten().map(|e| e.path()).collect();
    versions.sort();
    versions.into_iter().rev().map(|v| v.join(r"x64\rc.exe")).find(|p| p.exists())
}

fn main() {
    println!("cargo:rerun-if-changed=res/slate.rc");
    println!("cargo:rerun-if-changed=res/slate.ico");
    println!("cargo:rerun-if-changed=res/slate.manifest");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found; building without icon and manifest");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("slate.res");
    let include = rc.parent().and_then(|p| p.parent()).and_then(|p| p.parent()).and_then(|p| p.parent()).map(|kits| {
        let ver = rc.parent().unwrap().parent().unwrap().file_name().unwrap().to_owned();
        kits.join("Include").join(ver)
    });
    let mut cmd = Command::new(&rc);
    cmd.arg("/nologo");
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    for (name, var) in [("VER_MAJOR", "CARGO_PKG_VERSION_MAJOR"), ("VER_MINOR", "CARGO_PKG_VERSION_MINOR"), ("VER_PATCH", "CARGO_PKG_VERSION_PATCH")] {
        cmd.arg(format!("/D{name}={}", std::env::var(var).unwrap()));
    }
    cmd.arg(format!("/DVER_STR=\"{ver}\""));
    if let Some(inc) = include {
        for sub in ["um", "shared"] {
            cmd.arg(format!("/I{}", inc.join(sub).display()));
        }
    }
    cmd.arg(format!("/fo{}", out.display())).arg("res/slate.rc");
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=rc.exe failed; building without icon and manifest");
    }
}
