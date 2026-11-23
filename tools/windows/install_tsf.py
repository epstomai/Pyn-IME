"""Deploy, ACL, register, hot-swap, and manage Input Engine Windows TSF service."""
from __future__ import annotations

import argparse
import ctypes
import os
import re
import shutil
import subprocess
import sys
import time
import winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "InputEngine"
LEGACY_CLSID = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}"
LEGACY_PROFILE = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}"
CLSID_STR = LEGACY_CLSID
PROFILE_STR = LEGACY_PROFILE
TIP_ID = f"0804:{CLSID_STR}{PROFILE_STR}"
LANGID_STR = "0x00000804"
ZH_CN_ASSEMBLY_KEY = rf"Software\Microsoft\CTF\Assemblies\{LANGID_STR}\{{34745C63-B2F0-4784-8B67-5E12C8701A31}}"
GEN_FILE = "generation.txt"
CREATE_NO_WINDOW = 0x08000000
MOVEFILE_DELAY_UNTIL_REBOOT = 0x00000004
_GEN_DLL_RE = re.compile(r"^ime_tsf(?:_x86)?_g([0-9a-f]{4})\.dll$", re.IGNORECASE)


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


def guids_for_generation(gen: int) -> tuple[str, str]:
    if gen == 0:
        return (LEGACY_CLSID, LEGACY_PROFILE)
    if not (1 <= gen <= 0xFFFF):
        raise ValueError(f"generation out of range 1..=0xFFFF: {gen}")
    return (
        f"{{A94E2C1B-7D3F-4E8A-9B20-6C5F8A3D{gen:04X}}}",
        f"{{A94E2C1B-7D3F-4E8A-9B21-6C5F8A3D{gen:04X}}}",
    )


def parse_generation_from_clsid(clsid: str) -> int | None:
    norm = clsid.strip().upper()
    if norm == LEGACY_CLSID:
        return 0
    prefix = "{A94E2C1B-7D3F-4E8A-9B20-6C5F8A3D"
    if len(norm) == 38 and norm.startswith(prefix) and norm.endswith("}"):
        try:
            gen = int(norm[len(prefix) : 37], 16)
            return gen if 1 <= gen <= 0xFFFF else None
        except ValueError:
            return None
    return None


def parse_generation_from_dll_name(name: str) -> int | None:
    low = name.strip().lower()
    if low in ("ime_tsf.dll", "ime_tsf_x86.dll"):
        return 0
    m = _GEN_DLL_RE.match(low)
    if not m:
        return None
    gen = int(m.group(1), 16)
    return gen if 1 <= gen <= 0xFFFF else None


def tip_id_for_generation(gen: int) -> str:
    clsid, prof = guids_for_generation(gen)
    return f"0804:{clsid}{prof}"


def dll_names_for_generation(gen: int) -> tuple[str, str]:
    if gen == 0:
        return ("ime_tsf.dll", "ime_tsf_x86.dll")
    return (f"ime_tsf_g{gen:04x}.dll", f"ime_tsf_x86_g{gen:04x}.dll")


def install_layout_or_tip(tip_id: str, flags: int) -> bool:
    input_dll = ctypes.WinDLL("input.dll")
    fn = input_dll.InstallLayoutOrTip
    fn.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32]
    fn.restype = ctypes.c_int
    return bool(fn(tip_id, flags))


def set_default_layout_or_tip(tip_id: str, flags: int = 0) -> bool:
    input_dll = ctypes.WinDLL("input.dll")
    fn = input_dll.SetDefaultLayoutOrTip
    fn.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32]
    fn.restype = ctypes.c_int
    return bool(fn(tip_id, flags))


def _read_generation_file(target_dir: Path) -> int | None:
    gen_path = target_dir / GEN_FILE
    if not gen_path.exists():
        return None
    try:
        raw = gen_path.read_text(encoding="utf-8").strip().lower()
        if not raw:
            return None
        if raw.startswith("g"):
            val = int(raw[1:], 16)
        elif raw.startswith("0x"):
            val = int(raw, 16)
        else:
            val = int(raw, 16)
        return val if 0 <= val <= 0xFFFF else None
    except (OSError, ValueError):
        return None


def _write_generation_file(target_dir: Path, gen: int) -> None:
    gen_path = target_dir / GEN_FILE
    gen_path.write_text(f"0x{gen:04x}\n", encoding="utf-8")


def _enum_subkeys(root: int, subkey: str, wow64_flag: int = 0) -> list[str]:
    names: list[str] = []
    try:
        with winreg.OpenKey(root, subkey, 0, winreg.KEY_READ | wow64_flag) as k:
            i = 0
            while True:
                try:
                    names.append(winreg.EnumKey(k, i))
                    i += 1
                except OSError:
                    break
    except OSError:
        pass
    return names


def discover_registered_generations() -> list[int]:
    found: set[int] = set()
    for sub in _enum_subkeys(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\CTF\TIP"):
        gen = parse_generation_from_clsid(sub)
        if gen is not None:
            found.add(gen)
    for flag in (winreg.KEY_WOW64_64KEY, winreg.KEY_WOW64_32KEY):
        for sub in _enum_subkeys(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes\CLSID", flag):
            gen = parse_generation_from_clsid(sub)
            if gen is not None:
                found.add(gen)
    return sorted(found)


def _try_remove_file(path: Path) -> bool:
    if not path.exists():
        return True
    try:
        path.unlink()
        return True
    except OSError:
        return False


def choose_next_generation(target_dir: Path, registered_gens: list[int]) -> int:
    used = set(registered_gens)
    highest = max(used, default=0)
    file_gen = _read_generation_file(target_dir)
    if file_gen is not None:
        used.add(file_gen)
        highest = max(highest, file_gen)
    if target_dir.exists():
        for p in target_dir.iterdir():
            if not p.is_file():
                continue
            g = parse_generation_from_dll_name(p.name)
            if g is not None and g >= 1:
                used.add(g)
                highest = max(highest, g)

    start = (highest % 0xFFFF) + 1
    for offset in range(0xFFFF):
        cand = ((start - 1 + offset) % 0xFFFF) + 1
        if cand in used:
            continue
        x64_name, x86_name = dll_names_for_generation(cand)
        p64 = target_dir / x64_name
        p32 = target_dir / x86_name
        if (not p64.exists() or _try_remove_file(p64)) and (
            not p32.exists() or _try_remove_file(p32)
        ):
            return cand
    raise SystemExit("unable to allocate free TSF generation slot")


def _get_current_default_zh_cn_tip() -> tuple[str | None, str | None]:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, ZH_CN_ASSEMBLY_KEY) as k:
            default_clsid, _ = winreg.QueryValueEx(k, "Default")
            profile, _ = winreg.QueryValueEx(k, "Profile")
            return (str(default_clsid).strip().upper(), str(profile).strip().upper())
    except OSError:
        return (None, None)


def _delete_key_tree(root: int, subkey: str, wow64_flag: int = 0) -> None:
    children = _enum_subkeys(root, subkey, wow64_flag)
    for child in children:
        _delete_key_tree(root, f"{subkey}\\{child}", wow64_flag)
    try:
        winreg.DeleteKeyEx(root, subkey, wow64_flag, 0)
    except OSError:
        pass


def check_hklm_clsid(
    wow64_flag: int = winreg.KEY_WOW64_64KEY, clsid: str = CLSID_STR
) -> str | None:
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            rf"SOFTWARE\Classes\CLSID\{clsid}\InprocServer32",
            0,
            winreg.KEY_READ | wow64_flag,
        ) as k:
            val, _ = winreg.QueryValueEx(k, "")
            return str(val)
    except OSError:
        return None


def check_hklm_language_profile(
    clsid: str = CLSID_STR, profile: str = PROFILE_STR
) -> str | None:
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            rf"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\{LANGID_STR}\{profile}",
        ) as k:
            val, _ = winreg.QueryValueEx(k, "Description")
            return str(val)
    except OSError:
        return None


def _unregister_generation(gen: int) -> None:
    clsid, _ = guids_for_generation(gen)
    install_layout_or_tip(tip_id_for_generation(gen), 1)  # ILOT_UNINSTALL

    for wow64_flag, is_x86 in (
        (winreg.KEY_WOW64_32KEY, True),
        (winreg.KEY_WOW64_64KEY, False),
    ):
        dll_str = check_hklm_clsid(wow64_flag, clsid)
        if dll_str and Path(dll_str).exists():
            subprocess.run(
                [str(_regsvr32_path(x86=is_x86)), "/u", "/s", dll_str],
                check=False,
            )

    _delete_key_tree(
        winreg.HKEY_LOCAL_MACHINE,
        rf"SOFTWARE\Classes\CLSID\{clsid}",
        winreg.KEY_WOW64_64KEY,
    )
    _delete_key_tree(
        winreg.HKEY_LOCAL_MACHINE,
        rf"SOFTWARE\Classes\CLSID\{clsid}",
        winreg.KEY_WOW64_32KEY,
    )
    _delete_key_tree(winreg.HKEY_LOCAL_MACHINE, rf"SOFTWARE\Microsoft\CTF\TIP\{clsid}", 0)

    cat_root = r"SOFTWARE\Microsoft\CTF\Category\Category"
    for cat_guid in _enum_subkeys(winreg.HKEY_LOCAL_MACHINE, cat_root):
        item_key = rf"{cat_root}\{cat_guid}\Item\{clsid}"
        _delete_key_tree(winreg.HKEY_LOCAL_MACHINE, item_key, 0)


def _schedule_delete_on_reboot(path: Path) -> bool:
    try:
        return bool(
            ctypes.windll.kernel32.MoveFileExW(str(path), None, MOVEFILE_DELAY_UNTIL_REBOOT)
        )
    except Exception:
        return False


def _cleanup_obsolete_dlls(target_dir: Path, keep_names: set[str]) -> list[Path]:
    keep_lower = {k.lower() for k in keep_names}
    locked: list[Path] = []
    if not target_dir.exists():
        return locked
    for p in target_dir.iterdir():
        if not p.is_file():
            continue
        low = p.name.lower()
        if low in keep_lower:
            continue
        if not low.startswith("ime_tsf") or (".dll" not in low):
            continue
        if not _try_remove_file(p):
            _schedule_delete_on_reboot(p)
            locked.append(p)
    return locked


def _activate_session(dll_x64: Path) -> bool:
    res = subprocess.run(
        [str(_regsvr32_path(x86=False)), "/n", "/i:activate", "/s", str(dll_x64)],
        check=False,
        capture_output=True,
    )
    return res.returncode == 0


def _copy_replace_locked(src: Path, dst: Path) -> bool:
    """Copy `src` to `dst`, renaming an in-use target to `.old.<pid>.<ts>` if locked."""
    for old_path in dst.parent.glob(f"{dst.name}.old.*"):
        _try_remove_file(old_path)
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
    gui_src = release / "ime-settings.exe"
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

    old_gens = discover_registered_generations()
    new_gen = choose_next_generation(target_dir, old_gens)
    x64_name, x86_name = dll_names_for_generation(new_gen)
    dst_x64 = target_dir / x64_name
    dst_x86 = target_dir / x86_name

    shutil.copy2(dll_src, dst_x64)
    shutil.copy2(dll_x86_src, dst_x86)
    _copy_replace_locked(srv_src, dst_srv)
    if gui_src.exists():
        _copy_replace_locked(gui_src, target_dir / "ime-settings.exe")
        _create_settings_shortcut(target_dir / "ime-settings.exe")
    _copy_if_changed(mod_src, models_dir / "ime_model.bin")
    if rer_src.exists():
        _copy_if_changed(rer_src, models_dir / "reranker.bin")
    _write_generation_file(target_dir, new_gen)

    # Grant ALL APPLICATION PACKAGES (*S-1-15-2-1) RX access so AppContainer apps can load ime_tsf_gNNNN.dll.
    subprocess.run(
        ["icacls", str(target_dir), "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/Q"],
        check=True,
    )

    if old_gens and not args.no_hotswap:
        new_tip_id = tip_id_for_generation(new_gen)
        cur_def_clsid, cur_def_prof = _get_current_default_zh_cn_tip()
        was_default = any(
            cur_def_clsid == guids_for_generation(g)[0]
            and cur_def_prof == guids_for_generation(g)[1]
            for g in old_gens
        )

        subprocess.run([str(_regsvr32_path(x86=True)), "/s", str(dst_x86)], check=True)
        subprocess.run([str(_regsvr32_path(x86=False)), "/s", str(dst_x64)], check=True)
        ok = install_layout_or_tip(new_tip_id, 0)
        assert ok, f"InstallLayoutOrTip({new_tip_id}, 0) failed"

        for og in old_gens:
            install_layout_or_tip(tip_id_for_generation(og), 1)

        if was_default:
            set_default_layout_or_tip(new_tip_id, 0)

        activated = _activate_session(dst_x64)

        for og in old_gens:
            _unregister_generation(og)

        locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
        old_str = ", ".join(f"g{g:04x}" for g in old_gens)
        print(
            f"hot-swapped TSF {old_str} -> g{new_gen:04x} ({tip_id_for_generation(new_gen)}) "
            f"in {target_dir} (no app restart required)"
        )
        if not activated:
            print(
                "note: session-wide ActivateProfile returned non-zero (e.g. current keyboard is EN-US); "
                "InstallLayoutOrTip has already switched active TIP so background apps load the new generation on focus/typing."
            )
        if locked:
            print(
                f"note: {len(locked)} unreferenced old DLL file(s) still mapped by existing processes "
                "scheduled for automatic deletion on next reboot."
            )
    else:
        locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
        print(
            f"deployed generation g{new_gen:04x} ({x64_name}, {x86_name}) to {target_dir} "
            "(with ALL APPLICATION PACKAGES RX ACL)"
        )
        if not old_gens:
            print("note: run `install_tsf.py register` to register COM + TSF profile.")


def cmd_register(args) -> None:
    if not is_admin():
        raise SystemExit("regsvr32 HKLM registration requires Administrator privileges")

    if args.dll:
        dll = Path(args.dll)
        dll_x86 = Path(args.dll_x86) if args.dll_x86 else dll.with_name(
            dll.name.replace("ime_tsf", "ime_tsf_x86", 1)
        )
    else:
        gen = _read_generation_file(DEFAULT_DIR)
        if gen is None:
            gen = 0
        x64_name, x86_name = dll_names_for_generation(gen)
        dll = DEFAULT_DIR / x64_name
        dll_x86 = Path(args.dll_x86) if args.dll_x86 else (DEFAULT_DIR / x86_name)

    if not dll.exists():
        raise SystemExit(f"DLL not found: {dll}")

    target_gen = parse_generation_from_dll_name(dll.name)
    if target_gen is None:
        target_gen = 0
    tip_id = tip_id_for_generation(target_gen)
    old_gens = [g for g in discover_registered_generations() if g != target_gen]

    if dll_x86.exists():
        subprocess.run([str(_regsvr32_path(x86=True)), "/s", str(dll_x86)], check=True)
    subprocess.run([str(_regsvr32_path(x86=False)), "/s", str(dll)], check=True)
    ok = install_layout_or_tip(tip_id, 0)
    assert ok, f"InstallLayoutOrTip({tip_id}, 0) failed"

    for og in old_gens:
        _unregister_generation(og)

    activated = _activate_session(dll)
    x86_msg = f" + 32-bit ({dll_x86})" if dll_x86.exists() else ""
    print(f"registered COM + TSF TIP ({tip_id}): {dll}{x86_msg}")
    if not activated:
        print(
            "note: session-wide ActivateProfile returned non-zero; "
            "InstallLayoutOrTip has registered the profile for active apps."
        )


def cmd_unregister(args) -> None:
    if not is_admin():
        raise SystemExit("regsvr32 HKLM unregistration requires Administrator privileges")

    base_dir = Path(args.dll).parent if args.dll else DEFAULT_DIR
    srv = base_dir / "ime-server.exe"
    if srv.exists():
        _shutdown_server_and_wait(srv)

    gens = discover_registered_generations()
    if not gens:
        dll = Path(args.dll) if args.dll else (DEFAULT_DIR / "ime_tsf.dll")
        dll_x86 = Path(args.dll_x86) if args.dll_x86 else (DEFAULT_DIR / "ime_tsf_x86.dll")
        install_layout_or_tip(TIP_ID, 1)
        if dll_x86.exists():
            subprocess.run([str(_regsvr32_path(x86=True)), "/u", "/s", str(dll_x86)], check=False)
        if dll.exists():
            subprocess.run([str(_regsvr32_path(x86=False)), "/u", "/s", str(dll)], check=False)
        print("unregistered COM + TSF TIP (legacy check)")
        return

    for g in gens:
        _unregister_generation(g)
    _cleanup_obsolete_dlls(base_dir, set())
    gen_str = ", ".join(f"g{g:04x}" for g in gens)
    print(f"unregistered all Input Engine TSF generations: {gen_str}")


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


def _create_settings_shortcut(target_exe: Path) -> None:
    appdata = os.environ.get("APPDATA", "")
    if not appdata or not target_exe.exists():
        return
    programs_dir = Path(appdata) / "Microsoft" / "Windows" / "Start Menu" / "Programs"
    if not programs_dir.exists():
        return
    lnk = programs_dir / "Input Engine 设置.lnk"
    ps = (
        f"$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{str(lnk)}');"
        f"$s.TargetPath='{str(target_exe)}';"
        f"$s.WorkingDirectory='{str(target_exe.parent)}';"
        f"$s.IconLocation='{str(target_exe)},0';"
        "$s.Description='Input Engine 外观与输入设置';"
        "$s.Save()"
    )
    subprocess.run(
        ["powershell", "-NoProfile", "-Command", ps],
        check=False,
        capture_output=True,
    )
    try:
        ctypes.windll.shell32.SHChangeNotify(0x08000000, 0x0000, None, None)
    except Exception:
        pass


def cmd_settings(args) -> None:
    exe = Path(args.dir) / "ime-settings.exe"
    if not exe.exists():
        exe = ROOT / "target" / "release" / "ime-settings.exe"
    if not exe.exists():
        raise SystemExit(f"ime-settings.exe not found in {args.dir} or target/release")
    subprocess.Popen([str(exe)])
    print(f"launched settings GUI: {exe}")


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


def cmd_status(args) -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    target_dir = Path(args.dir)
    gen_file_val = _read_generation_file(target_dir)
    print(
        f"Deployed generation ({GEN_FILE}): "
        + (f"g{gen_file_val:04x}" if gen_file_val is not None else "none")
    )

    gens = discover_registered_generations()
    if not gens:
        print("Registered TSF TIP generations: none")
    else:
        for g in gens:
            clsid, prof = guids_for_generation(g)
            x64_path = check_hklm_clsid(winreg.KEY_WOW64_64KEY, clsid)
            x86_path = check_hklm_clsid(winreg.KEY_WOW64_32KEY, clsid)
            lp_desc = check_hklm_language_profile(clsid, prof)
            print(f"[g{g:04x}] HKLM CLSID x64 {clsid}: {x64_path or 'not registered'}")
            print(f"[g{g:04x}] HKLM CLSID x86 {clsid}: {x86_path or 'not registered'}")
            print(f"[g{g:04x}] HKLM LanguageProfile {LANGID_STR} {prof}: {lp_desc or 'not registered'}")

    srv = target_dir / "ime-server.exe"
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
    p_dep.add_argument("--no-hotswap", action="store_true")

    p_reg = sub.add_parser("register")
    p_reg.add_argument("--dll", default=None)
    p_reg.add_argument("--dll-x86", default=None)

    p_unreg = sub.add_parser("unregister")
    p_unreg.add_argument("--dll", default=None)
    p_unreg.add_argument("--dll-x86", default=None)

    p_start = sub.add_parser("start")
    p_start.add_argument("--dir", default=str(DEFAULT_DIR))

    p_stop = sub.add_parser("stop")
    p_stop.add_argument("--dir", default=str(DEFAULT_DIR))

    p_stat = sub.add_parser("status")
    p_stat.add_argument("--dir", default=str(DEFAULT_DIR))

    p_set = sub.add_parser("settings")
    p_set.add_argument("--dir", default=str(DEFAULT_DIR))

    args = p.parse_args()
    {
        "deploy": cmd_deploy,
        "register": cmd_register,
        "unregister": cmd_unregister,
        "start": cmd_start,
        "stop": cmd_stop,
        "status": cmd_status,
        "settings": cmd_settings,
    }[args.cmd](args)


if __name__ == "__main__":
    main()
