"""Build release packages for GitHub: Windows (x64 + x86 TSF) zip and Android arm64 APK.

    .venv/Scripts/python tools/release/package_release.py [--version 0.1.0] [--skip-windows] [--skip-android]

Outputs go to dist/:
  Pyn-<ver>-windows.zip       ime_tsf.dll (x64), ime_tsf_x86.dll, ime-server.exe, ime-settings.exe,
                              models/, install.cmd / uninstall.cmd (+ .ps1)
  Pyn-<ver>-android-arm64.apk release (non-debuggable) build, slim mobile model bundled
  SHA256SUMS.txt

Windows binaries are linked with the static CRT (no VC++ redistributable needed) into a separate
target dir (target/dist) so day-to-day dev builds are not invalidated.

APK signing: env PYN_KEYSTORE / PYN_KEYSTORE_PASS / PYN_KEY_ALIAS / PYN_KEY_PASS, defaulting to the
local Android debug keystore (so it upgrades in place over `build_install.py` dev installs).
"""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "android"))
import build_install as android  # noqa: E402

DIST = ROOT / "dist"
DIST_TARGET = ROOT / "target" / "dist"
WIN_SCRIPTS = ROOT / "tools" / "windows" / "release"


def run(cmd: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None) -> None:
    print(">>>", " ".join(cmd), flush=True)
    subprocess.run(cmd, cwd=str(cwd), env=env, check=True)


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'\[workspace\.package\][^\[]*?version\s*=\s*"([^"]+)"', text, re.S)
    if not m:
        sys.exit("cannot read [workspace.package] version from Cargo.toml")
    return m.group(1)


def build_windows(version: str) -> Path:
    env = os.environ.copy()
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C target-feature=+crt-static").strip()
    common = ["--release", "--target-dir", str(DIST_TARGET)]
    run(["cargo", "build", *common, "--target", "x86_64-pc-windows-msvc",
         "-p", "ime-tsf", "-p", "ime-server", "-p", "ime-settings"], env=env)
    run(["cargo", "build", *common, "--target", "i686-pc-windows-msvc", "-p", "ime-tsf"], env=env)

    x64 = DIST_TARGET / "x86_64-pc-windows-msvc" / "release"
    x86 = DIST_TARGET / "i686-pc-windows-msvc" / "release"
    name = f"Pyn-{version}-windows"
    stage = DIST / name
    if stage.exists():
        shutil.rmtree(stage)
    (stage / "models").mkdir(parents=True)
    files = {
        x64 / "ime_tsf.dll": "ime_tsf.dll",
        x86 / "ime_tsf.dll": "ime_tsf_x86.dll",
        x64 / "ime-server.exe": "ime-server.exe",
        x64 / "ime-settings.exe": "ime-settings.exe",
        ROOT / "models" / "ime_model.bin": "models/ime_model.bin",
        ROOT / "models" / "reranker.bin": "models/reranker.bin",
    }
    for script in ("install.cmd", "install.ps1", "uninstall.cmd", "uninstall.ps1", "README.txt"):
        files[WIN_SCRIPTS / script] = script
    for src, rel in files.items():
        if not src.is_file():
            sys.exit(f"missing {src}")
        shutil.copy2(src, stage / rel)

    out = DIST / f"{name}.zip"
    out.unlink(missing_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for p in sorted(stage.rglob("*")):
            if p.is_file():
                z.write(p, f"{name}/{p.relative_to(stage).as_posix()}")
    shutil.rmtree(stage)
    return out


def sdk_dir() -> Path:
    for key in ("ANDROID_HOME", "ANDROID_SDK_ROOT"):
        if os.environ.get(key):
            return Path(os.environ[key])
    props = android.ANDROID_DIR / "local.properties"
    for line in props.read_text(encoding="utf-8").splitlines():
        if line.startswith("sdk.dir="):
            return Path(line.split("=", 1)[1].replace("\\:", ":").replace("\\\\", "\\"))
    sys.exit("Android SDK not found (ANDROID_HOME / local.properties)")


def build_android(version: str) -> Path:
    android.ensure_mobile_model(False)
    run(["cargo", "ndk", "-t", "arm64-v8a", "-p", "29", "-o", str(android.JNI_OUT),
         "build", "--release", "-p", "ime-android"])

    env = os.environ.copy()
    java_home = android.find_java_home()
    if java_home:
        env["JAVA_HOME"] = java_home
        env["PATH"] = f"{java_home}\\bin;{env.get('PATH', '')}"
    gradlew = android.ANDROID_DIR / ("gradlew.bat" if os.name == "nt" else "gradlew")
    run([str(gradlew), "assembleRelease"], cwd=android.ANDROID_DIR, env=env)
    unsigned = android.ANDROID_DIR / "app" / "build" / "outputs" / "apk" / "release" / "app-release-unsigned.apk"

    tools = sorted((sdk_dir() / "build-tools").iterdir(), key=lambda p: p.name)[-1]
    ext = ".exe" if os.name == "nt" else ""
    aligned = DIST / "app-release-aligned.apk"
    out = DIST / f"Pyn-{version}-android-arm64.apk"
    run([str(tools / f"zipalign{ext}"), "-f", "-p", "4", str(unsigned), str(aligned)])
    keystore = os.environ.get("PYN_KEYSTORE", str(Path.home() / ".android" / "debug.keystore"))
    run([
        str(tools / ("apksigner.bat" if os.name == "nt" else "apksigner")), "sign",
        "--ks", keystore,
        "--ks-pass", "pass:" + os.environ.get("PYN_KEYSTORE_PASS", "android"),
        "--key-pass", "pass:" + os.environ.get("PYN_KEY_PASS", os.environ.get("PYN_KEYSTORE_PASS", "android")),
        "--ks-key-alias", os.environ.get("PYN_KEY_ALIAS", "androiddebugkey"),
        "--out", str(out), str(aligned),
    ])
    aligned.unlink(missing_ok=True)
    Path(str(out) + ".idsig").unlink(missing_ok=True)
    return out


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--version", default=None)
    p.add_argument("--skip-windows", action="store_true")
    p.add_argument("--skip-android", action="store_true")
    args = p.parse_args()
    version = args.version or workspace_version()
    DIST.mkdir(exist_ok=True)

    outputs: list[Path] = []
    if not args.skip_windows:
        outputs.append(build_windows(version))
    if not args.skip_android:
        outputs.append(build_android(version))

    sums = DIST / "SHA256SUMS.txt"
    lines = [f"{sha256(o)}  {o.name}" for o in outputs]
    sums.write_text("\n".join(lines) + "\n", encoding="utf-8")
    for o in outputs:
        print(f"{o}  {o.stat().st_size / (1024 * 1024):.1f} MiB")
    print(sums.read_text(encoding="utf-8"))


if __name__ == "__main__":
    main()
