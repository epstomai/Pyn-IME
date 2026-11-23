use std::path::{Path, PathBuf};
use std::process::Command;

fn find_windows_kit_rc(arch_dir: &str) -> Option<PathBuf> {
    let pf86 = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let bin_root = pf86.join("Windows Kits").join("10").join("bin");
    let entries = std::fs::read_dir(&bin_root).ok()?;
    let mut versions: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(arch_dir).join("rc.exe").is_file())
        .collect();
    versions.sort();
    versions.pop().map(|v| v.join(arch_dir).join("rc.exe"))
}

fn compile_ico_resource(manifest_dir: &Path, res_name: &str) -> Option<PathBuf> {
    let ico_path = manifest_dir
        .parent()?
        .parent()?
        .join("assets")
        .join("pyn.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());
    if !ico_path.is_file() {
        return None;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let rc_path = out_dir.join(format!("{res_name}.rc"));
    let res_path = out_dir.join(format!("{res_name}.res"));
    let ico_fwd = ico_path.to_string_lossy().replace('\\', "/");
    std::fs::write(&rc_path, format!("1 ICON \"{ico_fwd}\"\n")).ok()?;

    let arch_dir = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };

    if let Some(rc_exe) = find_windows_kit_rc(arch_dir) {
        let status = Command::new(&rc_exe)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res_path)
            .arg(&rc_path)
            .status()
            .expect("failed to execute rc.exe");
        assert!(status.success(), "rc.exe failed for {}", rc_path.display());
        return Some(res_path);
    }

    let status = Command::new("llvm-rc")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .expect("neither Windows Kits rc.exe nor llvm-rc found");
    assert!(status.success(), "llvm-rc failed for {}", rc_path.display());
    Some(res_path)
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    if let Some(res_path) = compile_ico_resource(&manifest_dir, "ime_settings_icon") {
        println!(
            "cargo:rustc-link-arg-bin=ime-settings={}",
            res_path.display()
        );
    }
}
