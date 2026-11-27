"""Build Rust arm64-v8a JNI library, assemble Android APK, and optionally install via adb."""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ANDROID_DIR = ROOT / "android"
JNI_OUT = ANDROID_DIR / "app" / "src" / "main" / "jniLibs"
FULL_MODEL = ROOT / "models" / "ime_model.bin"
MOBILE_MODEL = ROOT / "models" / "ime_model_mobile.bin"
# Chosen by eval sweep: on par with the full model on all suites, 0 gate failures, 830/830 pollution.
MOBILE_SLIM_ARGS = ["--max-bigrams", "2.5M", "--max-trigrams", "2.5M", "--protect-weight", "5"]


def find_java_home() -> str:
    for cand in (
        r"C:\Program Files\Eclipse Adoptium\jdk-21.0.12.101-hotspot",
        r"C:\Program Files\Java\jdk-17.0.5",
        os.environ.get("JAVA_HOME", ""),
    ):
        if cand and Path(cand).is_dir():
            return cand
    return ""


def run(cmd: list[str], cwd: Path, env: dict[str, str] | None = None) -> None:
    print(">>>", " ".join(cmd))
    subprocess.run(cmd, cwd=str(cwd), env=env, check=True)


def ensure_mobile_model(force: bool) -> None:
    """(Re)build the slim mobile model when missing or older than the full model."""
    if (
        not force
        and MOBILE_MODEL.is_file()
        and (not FULL_MODEL.is_file() or MOBILE_MODEL.stat().st_mtime >= FULL_MODEL.stat().st_mtime)
    ):
        return
    if not FULL_MODEL.is_file():
        sys.exit(f"missing full model {FULL_MODEL}")
    run(["cargo", "build", "--release", "-p", "ime-train", "--bin", "ime-slim"], cwd=ROOT)
    exe = ROOT / "target" / "release" / ("ime-slim.exe" if os.name == "nt" else "ime-slim")
    run([str(exe), "--in", str(FULL_MODEL), "--out", str(MOBILE_MODEL), *MOBILE_SLIM_ARGS], cwd=ROOT)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--skip-rust", action="store_true")
    p.add_argument("--rebuild-model", action="store_true", help="force regenerating the slim mobile model")
    p.add_argument("--install", action="store_true", help="adb install -r after build")
    p.add_argument("--launch", action="store_true", help="launch SettingsActivity after install")
    args = p.parse_args()

    ensure_mobile_model(args.rebuild_model)

    if not args.skip_rust:
        run(
            [
                "cargo",
                "ndk",
                "-t",
                "arm64-v8a",
                "-p",
                "29",
                "-o",
                str(JNI_OUT),
                "build",
                "--release",
                "-p",
                "ime-android",
            ],
            cwd=ROOT,
        )

    env = os.environ.copy()
    java_home = find_java_home()
    if java_home:
        env["JAVA_HOME"] = java_home
        env["PATH"] = f"{java_home}\\bin;{env.get('PATH', '')}"

    gradlew = ANDROID_DIR / ("gradlew.bat" if os.name == "nt" else "gradlew")
    run([str(gradlew), "assembleDebug"], cwd=ANDROID_DIR, env=env)

    apk = ANDROID_DIR / "app" / "build" / "outputs" / "apk" / "debug" / "app-debug.apk"
    print(f"APK built: {apk} ({apk.stat().st_size / (1024 * 1024):.1f} MB)")

    if args.install:
        run(["adb", "install", "-r", str(apk)], cwd=ROOT)
        if args.launch:
            run(
                ["adb", "shell", "am", "start", "-n", "com.pyn.ime/.SettingsActivity"],
                cwd=ROOT,
            )


if __name__ == "__main__":
    main()
