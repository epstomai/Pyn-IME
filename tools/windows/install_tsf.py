"""Deploy, ACL, register, and manage Input Engine Windows TSF service."""
from __future__ import annotations

import argparse
import ctypes
import os
import shutil
import subprocess
import sys
import time
import winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "InputEngine"
CLSID_STR = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}"
PROFILE_STR = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}"
TIP_ID = f"0804:{CLSID_STR}{PROFILE_STR}"
CREATE_NO_WINDOW = 0x08000000


def is_admin() -> bool:
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def _regsvr32_path(x86: bool = False) -> Path:
    sys_root = Path(os.environ.get("SystemRoot", r"C:\Windows"))
    sub = "SysWOW64" if x86 else "System32"
    path = sys_root / sub / "regsvr32.exe"
    return path if path.exists() else Path("regsvr32")


def install_layout_or_tip(flags: int) -> bool:
    input_dll = ctypes.WinDLL("input.dll")
    fn = input_dll.InstallLayoutOrTip
    fn.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32]
    fn.restype = ctypes.c_int
    return bool(fn(TIP_ID, flags))


def _cleanup_old_backups(dst: Path) -> None:
    for old_path in dst.parent.glob(f"{dst.name}.old.*"):
        try:
            old_path.unlink()
        except OSError:
            pass


def _copy_replace_locked(src: Path, dst: Path) -> bool:
    """Copy `src` to `dst`, renaming an in-use target to `.old.<pid>.<ts>` if locked."""
    _cleanup_old_backups(dst)
    try:
        shutil.copy2(src, dst)
        return False
    except PermissionError:
        if not dst.exists():
            raise
        backup = dst.with_name(f"{dst.name}.old.{os.getpid()}.{int(time.time())}")
        dst.replace(backup)
        shutil.copy2(src, dst)
        return True


def _copy_if_changed(src: Path, dst: Path) -> bool:
    if dst.exists():
        s_stat = src.stat()
        d_stat = dst.stat()
        if s_stat.st_size == d_stat.st_size and int(s_stat.st_mtime) == int(d_stat.st_mtime):
            return False
    shutil.copy2(src, dst)
    return True


def _shutdown_server_and_wait(srv: Path, timeout_sec: float = 3.0) -> None:
    if not srv.exists():
        return
    subprocess.run([str(srv), "--shutdown"], capture_output=True, text=True, check=False)
    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        try:
            with open(srv, "rb+"):
                return
        except PermissionError:
            time.sleep(0.1)
        except OSError:
            return


def cmd_deploy(args) -> None:
    if not is_admin() and str(args.dir).lower().startswith(
        os.environ.get("ProgramFiles", r"C:\Program Files").lower()
    ):
        raise SystemExit("deploy to Program Files requires Administrator privileges")

    release = ROOT / "target" / "release"
    dll_src = release / "ime_tsf.dll"
    dll_x86_src = ROOT / "target" / "i686-pc-windows-msvc" / "release" / "ime_tsf.dll"
    srv_src = release / "ime-server.exe"
    mod_src = ROOT / "models" / "ime_model.bin"
    rer_src = ROOT / "models" / "reranker.bin"
    for p in (dll_src, srv_src, mod_src):
        if not p.exists():
            raise SystemExit(f"missing build artifact: {p} (run cargo build --release first)")
    if not dll_x86_src.exists():
        raise SystemExit(
            f"missing 32-bit build artifact: {dll_x86_src} "
            "(run cargo build -p ime-tsf --release --target i686-pc-windows-msvc first)"
        )

    target_dir = Path(args.dir)
    models_dir = target_dir / "models"
    models_dir.mkdir(parents=True, exist_ok=True)

    dst_srv = target_dir / "ime-server.exe"
    _shutdown_server_and_wait(dst_srv)

    dll_hot_replaced = _copy_replace_locked(dll_src, target_dir / "ime_tsf.dll")
    dll_x86_hot_replaced = _copy_replace_locked(dll_x86_src, target_dir / "ime_tsf_x86.dll")
    _copy_replace_locked(srv_src, dst_srv)
    _copy_if_changed(mod_src, models_dir / "ime_model.bin")
    if rer_src.exists():
        _copy_if_changed(rer_src, models_dir / "reranker.bin")

    # Grant ALL APPLICATION PACKAGES (*S-1-15-2-1) RX access so AppContainer apps can load ime_tsf.dll.
    subprocess.run(
        ["icacls", str(target_dir), "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/Q"],
        check=True,
    )
    print(f"deployed x64+x86 to {target_dir} (with ALL APPLICATION PACKAGES RX ACL)")
    if dll_hot_replaced or dll_x86_hot_replaced:
        print(
            "note: ime_tsf DLL was in use and hot-renamed to *.old.*; newly opened apps use the new DLL, "
            "and already open apps will pick it up after reopening or restarting."
        )


def cmd_register(args) -> None:
    if not is_admin():
        raise SystemExit("regsvr32 HKLM registration requires Administrator privileges")
    dll = Path(args.dll)
    dll_x86 = Path(args.dll_x86) if args.dll_x86 else dll.with_name("ime_tsf_x86.dll")
    if not dll.exists():
        raise SystemExit(f"DLL not found: {dll}")
    if dll_x86.exists():
        subprocess.run([str(_regsvr32_path(x86=True)), "/s", str(dll_x86)], check=True)
    subprocess.run([str(_regsvr32_path(x86=False)), "/s", str(dll)], check=True)
    ok = install_layout_or_tip(0)
    assert ok, f"InstallLayoutOrTip({TIP_ID}, 0) failed"
    x86_msg = f" + 32-bit ({dll_x86})" if dll_x86.exists() else ""
    print(f"registered COM + TSF TIP ({TIP_ID}): {dll}{x86_msg}")


def cmd_unregister(args) -> None:
    if not is_admin():
        raise SystemExit("regsvr32 HKLM unregistration requires Administrator privileges")
    install_layout_or_tip(1)  # ILOT_UNINSTALL
    dll = Path(args.dll)
    dll_x86 = Path(args.dll_x86) if args.dll_x86 else dll.with_name("ime_tsf_x86.dll")
    srv = dll.parent / "ime-server.exe"
    if srv.exists():
        subprocess.run([str(srv), "--shutdown"], check=False)
    if dll_x86.exists():
        subprocess.run([str(_regsvr32_path(x86=True)), "/u", "/s", str(dll_x86)], check=False)
    subprocess.run([str(_regsvr32_path(x86=False)), "/u", "/s", str(dll)], check=True)
    print(f"unregistered COM + TSF TIP: {dll}")


def cmd_start(args) -> None:
    srv = Path(args.dir) / "ime-server.exe"
    if not srv.exists():
        srv = ROOT / "target" / "release" / "ime-server.exe"
    if not srv.exists():
        raise SystemExit(f"ime-server.exe not found in {args.dir} or target/release")
    subprocess.Popen(
        [str(srv)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=CREATE_NO_WINDOW,
    )
    print(f"started background daemon: {srv}")


def cmd_stop(args) -> None:
    srv = Path(args.dir) / "ime-server.exe"
    if not srv.exists():
        srv = ROOT / "target" / "release" / "ime-server.exe"
    if not srv.exists():
        raise SystemExit("ime-server.exe not found")
    res = subprocess.run([str(srv), "--shutdown"], capture_output=True, text=True)
    if res.returncode == 0:
        print(res.stdout.strip())
    else:
        print(f"server not running or unreachable ({res.stderr.strip()})")


def check_hklm_clsid(wow64_flag: int = winreg.KEY_WOW64_64KEY) -> str | None:
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            rf"SOFTWARE\Classes\CLSID\{CLSID_STR}\InprocServer32",
            0,
            winreg.KEY_READ | wow64_flag,
        ) as k:
            val, _ = winreg.QueryValueEx(k, "")
            return str(val)
    except OSError:
        return None


def check_hklm_language_profile() -> str | None:
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            rf"SOFTWARE\Microsoft\CTF\TIP\{CLSID_STR}\LanguageProfile\0x00000804\{PROFILE_STR}",
        ) as k:
            val, _ = winreg.QueryValueEx(k, "Description")
            return str(val)
    except OSError:
        return None


def cmd_status(args) -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    clsid_dll_x64 = check_hklm_clsid(winreg.KEY_WOW64_64KEY)
    clsid_dll_x86 = check_hklm_clsid(winreg.KEY_WOW64_32KEY)
    lp_desc = check_hklm_language_profile()
    print(f"HKLM CLSID x64 {CLSID_STR}: {clsid_dll_x64 or 'not registered'}")
    print(f"HKLM CLSID x86 {CLSID_STR}: {clsid_dll_x86 or 'not registered'}")
    print(f"HKLM LanguageProfile 0x00000804: {lp_desc or 'not registered'}")
    srv = Path(args.dir) / "ime-server.exe"
    if not srv.exists():
        srv = ROOT / "target" / "release" / "ime-server.exe"
    if srv.exists():
        res = subprocess.run([str(srv), "--ping"], capture_output=True, text=True)
        if res.returncode == 0:
            print(f"ime-server ping: {res.stdout.strip()}")
        else:
            print("ime-server ping: not running")
    else:
        print("ime-server.exe: not built")


def main() -> None:
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)

    p_dep = sub.add_parser("deploy")
    p_dep.add_argument("--dir", default=str(DEFAULT_DIR))

    p_reg = sub.add_parser("register")
    p_reg.add_argument("--dll", default=str(DEFAULT_DIR / "ime_tsf.dll"))
    p_reg.add_argument("--dll-x86", default=None)

    p_unreg = sub.add_parser("unregister")
    p_unreg.add_argument("--dll", default=str(DEFAULT_DIR / "ime_tsf.dll"))
    p_unreg.add_argument("--dll-x86", default=None)

    p_start = sub.add_parser("start")
    p_start.add_argument("--dir", default=str(DEFAULT_DIR))

    p_stop = sub.add_parser("stop")
    p_stop.add_argument("--dir", default=str(DEFAULT_DIR))

    p_stat = sub.add_parser("status")
    p_stat.add_argument("--dir", default=str(DEFAULT_DIR))

    args = p.parse_args()
    {
        "deploy": cmd_deploy,
        "register": cmd_register,
        "unregister": cmd_unregister,
        "start": cmd_start,
        "stop": cmd_stop,
        "status": cmd_status,
    }[args.cmd](args)


if __name__ == "__main__":
    main()
