"""Deploy, ACL, register, hot-swap, and manage Pyn Windows TSF service."""
from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import hashlib
import os
import re
import shutil
import subprocess
import sys
import time
import winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Pyn"
LEGACY_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "InputEngine"
LEGACY_CLSID = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}"
LEGACY_PROFILE = "{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}"
CLSID_STR = LEGACY_CLSID
PROFILE_STR = LEGACY_PROFILE
TIP_ID = f"0804:{CLSID_STR}{PROFILE_STR}"
LANGID_STR = "0x00000804"
ZH_CN_ASSEMBLY_KEY = rf"Software\Microsoft\CTF\Assemblies\{LANGID_STR}\{{34745C63-B2F0-4784-8B67-5E12C8701A31}}"
GEN_FILE = "generation.txt"
CREATE_NO_WINDOW = 0x08000000
DETACHED_PROCESS = 0x00000008
CREATE_NEW_PROCESS_GROUP = 0x00000200
CREATE_BREAKAWAY_FROM_JOB = 0x01000000
MOVEFILE_DELAY_UNTIL_REBOOT = 0x00000004
LOTP_DISABLED = 0x00000002
TH32CS_SNAPPROCESS = 0x00000002
TH32CS_SNAPMODULE = 0x00000008
TH32CS_SNAPMODULE32 = 0x00000010
ZH_CN_SORTORDER_ITEM0_KEY = rf"Software\Microsoft\CTF\SortOrder\AssemblyItem\{LANGID_STR}\{{34745C63-B2F0-4784-8B67-5E12C8701A31}}\00000000"
RUN_REG_PATH = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_VALUE_NAME = "PynServer"
_GEN_DLL_RE = re.compile(r"^ime_tsf(?:_x86)?_g([0-9a-f]{4})\.dll$", re.IGNORECASE)
_TIP_ID_RE = re.compile(r"^0804:(\{[0-9A-F-]+\})(\{[0-9A-F-]+\})$", re.IGNORECASE)


class _GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", ctypes.c_uint32),
        ("Data2", ctypes.c_uint16),
        ("Data3", ctypes.c_uint16),
        ("Data4", ctypes.c_ubyte * 8),
    ]


class _LAYOUTORTIPPROFILE(ctypes.Structure):
    _fields_ = [
        ("dwProfileType", ctypes.c_uint32),
        ("langid", ctypes.c_uint16),
        ("clsid", _GUID),
        ("guidProfile", _GUID),
        ("catid", _GUID),
        ("dwSubstituteLayout", ctypes.c_uint32),
        ("dwFlags", ctypes.c_uint32),
        ("szId", ctypes.c_wchar * 260),
    ]


class _PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wt.DWORD),
        ("cntUsage", wt.DWORD),
        ("th32ProcessID", wt.DWORD),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", wt.DWORD),
        ("cntThreads", wt.DWORD),
        ("th32ParentProcessID", wt.DWORD),
        ("pcPriClassBase", wt.LONG),
        ("dwFlags", wt.DWORD),
        ("szExeFile", wt.WCHAR * 260),
    ]


class _MODULEENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wt.DWORD),
        ("th32ModuleID", wt.DWORD),
        ("th32ProcessID", wt.DWORD),
        ("GlblcntUsage", wt.DWORD),
        ("ProccntUsage", wt.DWORD),
        ("modBaseAddr", ctypes.c_void_p),
        ("modBaseSize", wt.DWORD),
        ("hModule", wt.HMODULE),
        ("szModule", wt.WCHAR * 256),
        ("szExePath", wt.WCHAR * 260),
    ]


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


def _get_primary_language() -> str | None:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\CTF\SortOrder\Language") as k:
            val, _ = winreg.QueryValueEx(k, "00000000")
            return str(val).strip().lower()
    except OSError:
        return None


def _enum_layout_or_tips() -> list[tuple[int, int, int, int, str]]:
    items: list[tuple[int, int, int, int, str]] = []
    try:
        input_dll = ctypes.WinDLL("input.dll")
        enum_fn = input_dll.EnumEnabledLayoutOrTip
        enum_fn.argtypes = [
            ctypes.c_wchar_p,
            ctypes.c_wchar_p,
            ctypes.c_wchar_p,
            ctypes.POINTER(_LAYOUTORTIPPROFILE),
            ctypes.c_uint32,
        ]
        enum_fn.restype = ctypes.c_uint32
        cnt = enum_fn(None, None, None, None, 0)
        if cnt == 0:
            return items
        buf = (_LAYOUTORTIPPROFILE * cnt)()
        got = enum_fn(None, None, None, buf, cnt)
        for i in range(got):
            p = buf[i]
            items.append(
                (p.dwProfileType, p.langid, p.dwSubstituteLayout, p.dwFlags, p.szId.strip())
            )
    except Exception:
        pass
    return items


def _enum_enabled_zh_cn_tips() -> set[tuple[str, str]]:
    enabled: set[tuple[str, str]] = set()
    for _, langid, _, flags, sz_id in _enum_layout_or_tips():
        if langid != 0x0804 or (flags & LOTP_DISABLED) != 0:
            continue
        m = _TIP_ID_RE.match(sz_id)
        if m:
            enabled.add((m.group(1).upper(), m.group(2).upper()))
    return enabled


def _cleanup_stale_hkcu_tips(keep_gen: int | None) -> None:
    tip_root = r"Software\Microsoft\CTF\TIP"
    for sub in _enum_subkeys(winreg.HKEY_CURRENT_USER, tip_root):
        g = parse_generation_from_clsid(sub)
        if g is not None and g != keep_gen:
            _delete_key_tree(winreg.HKEY_CURRENT_USER, rf"{tip_root}\{sub}", 0)


def _ensure_zh_cn_default_tip(new_gen: int, old_gens: list[int]) -> bool:
    new_clsid, new_prof = guids_for_generation(new_gen)
    new_tip_id = tip_id_for_generation(new_gen)

    # Mark this generation's zh-CN language profile explicitly enabled in HKCU.
    hkcu_prof_key = rf"Software\Microsoft\CTF\TIP\{new_clsid}\LanguageProfile\{LANGID_STR}\{new_prof}"
    try:
        with winreg.CreateKeyEx(
            winreg.HKEY_CURRENT_USER, hkcu_prof_key, 0, winreg.KEY_WRITE
        ) as k:
            winreg.SetValueEx(k, "Enable", 0, winreg.REG_DWORD, 1)
    except OSError:
        pass

    cur_clsid, cur_prof = _get_current_default_zh_cn_tip()
    was_old_pyn = any(
        cur_clsid == guids_for_generation(g)[0] and cur_prof == guids_for_generation(g)[1]
        for g in old_gens
    )
    enabled_zh = _enum_enabled_zh_cn_tips()
    cur_missing_or_disabled = (
        not cur_clsid
        or not cur_prof
        or (cur_clsid, cur_prof) not in enabled_zh
    )

    updated = False
    if was_old_pyn or cur_missing_or_disabled:
        try:
            with winreg.CreateKeyEx(
                winreg.HKEY_CURRENT_USER, ZH_CN_ASSEMBLY_KEY, 0, winreg.KEY_WRITE
            ) as k:
                winreg.SetValueEx(k, "Default", 0, winreg.REG_SZ, new_clsid)
                winreg.SetValueEx(k, "Profile", 0, winreg.REG_SZ, new_prof)
            updated = True
        except OSError:
            pass
        if was_old_pyn and _get_primary_language() == "00000804":
            set_default_layout_or_tip(new_tip_id, 0)
    return updated


def _set_startup_run_entry(srv_exe: Path) -> None:
    try:
        with winreg.CreateKeyEx(
            winreg.HKEY_CURRENT_USER, RUN_REG_PATH, 0, winreg.KEY_WRITE
        ) as k:
            winreg.SetValueEx(k, RUN_VALUE_NAME, 0, winreg.REG_SZ, f'"{srv_exe}"')
    except OSError:
        pass


def _remove_startup_run_entry() -> None:
    try:
        with winreg.OpenKey(
            winreg.HKEY_CURRENT_USER, RUN_REG_PATH, 0, winreg.KEY_SET_VALUE
        ) as k:
            winreg.DeleteValue(k, RUN_VALUE_NAME)
    except OSError:
        pass


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
    _delete_key_tree(winreg.HKEY_CURRENT_USER, rf"Software\Microsoft\CTF\TIP\{clsid}", 0)

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
        is_obsolete_tsf = low.startswith("ime_tsf") and (".dll" in low)
        is_exe_old = low.startswith("ime-server.exe.old.") or low.startswith(
            "ime-settings.exe.old."
        )
        if not (is_obsolete_tsf or is_exe_old):
            continue
        if not _try_remove_file(p):
            _schedule_delete_on_reboot(p)
            locked.append(p)
    return locked


def _file_sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    h = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(65536), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return None


def _norm_win_path(p: str | Path) -> str:
    return os.path.normcase(os.path.normpath(str(p)))


def _is_stable_tip_registered(target_dir: Path) -> bool:
    if discover_registered_generations() != [0]:
        return False
    x64_reg = check_hklm_clsid(winreg.KEY_WOW64_64KEY, LEGACY_CLSID)
    x86_reg = check_hklm_clsid(winreg.KEY_WOW64_32KEY, LEGACY_CLSID)
    if not x64_reg or not x86_reg:
        return False
    if _norm_win_path(x64_reg) != _norm_win_path(target_dir / "ime_tsf.dll"):
        return False
    if _norm_win_path(x86_reg) != _norm_win_path(target_dir / "ime_tsf_x86.dll"):
        return False
    if check_hklm_language_profile(LEGACY_CLSID, LEGACY_PROFILE) is None:
        return False
    if (LEGACY_CLSID.upper(), LEGACY_PROFILE.upper()) not in _enum_enabled_zh_cn_tips():
        return False
    if _get_current_default_zh_cn_tip() != (LEGACY_CLSID.upper(), LEGACY_PROFILE.upper()):
        return False
    return True


def _grant_appcontainer_acl(target_dir: Path) -> None:
    # Grant ALL APPLICATION PACKAGES (*S-1-15-2-1) RX access so AppContainer apps can load TSF DLLs.
    subprocess.run(
        ["icacls", str(target_dir), "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/T", "/Q"],
        check=True,
    )


def _activate_session(dll_x64: Path) -> tuple[bool, int]:
    res = subprocess.run(
        [str(_regsvr32_path(x86=False)), "/n", "/i:activate", "/s", str(dll_x64)],
        check=False,
        capture_output=True,
    )
    return (res.returncode == 0, res.returncode)


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
        if backup.exists():
            backup = dst.with_name(f"{dst.name}.old.{os.getpid()}.{time.time_ns()}")
        dst.replace(backup)
        _schedule_delete_on_reboot(backup)
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


def _migrate_legacy_user_data() -> None:
    local_app_data = os.environ.get("LOCALAPPDATA", "")
    if not local_app_data:
        return
    legacy_dir = Path(local_app_data) / "InputEngine"
    new_dir = Path(local_app_data) / "Pyn"
    if not legacy_dir.is_dir():
        return
    migrated: list[str] = []
    for name in ("config.json", "user_dict.tsv"):
        src = legacy_dir / name
        dst = new_dir / name
        if src.is_file() and not dst.exists():
            new_dir.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
            migrated.append(name)
    if migrated:
        print(f"migrated user data from {legacy_dir} -> {new_dir}: {', '.join(migrated)}")


def _register_tip_and_migrate(
    target_gen: int,
    dll_x64: Path,
    dll_x86: Path | None,
    remove_gens: list[int],
) -> tuple[bool, int]:
    tip_id = tip_id_for_generation(target_gen)
    if dll_x86 is not None and dll_x86.exists():
        subprocess.run([str(_regsvr32_path(x86=True)), "/s", str(dll_x86)], check=True)
    subprocess.run([str(_regsvr32_path(x86=False)), "/s", str(dll_x64)], check=True)
    ok = install_layout_or_tip(tip_id, 0)
    assert ok, f"InstallLayoutOrTip({tip_id}, 0) failed"

    # Enable, set default zh-CN TIP, and activate BEFORE uninstalling old generations
    # so running apps never observe a gap with zero valid Pyn TIPs.
    _ensure_zh_cn_default_tip(target_gen, remove_gens)
    _activate_session(dll_x64)

    for og in remove_gens:
        _unregister_generation(og)
    _cleanup_stale_hkcu_tips(target_gen)

    # Re-confirm after removing old generations in case ILOT_UNINSTALL cleared defaults.
    _ensure_zh_cn_default_tip(target_gen, remove_gens)
    return _activate_session(dll_x64)


def cmd_deploy(args) -> None:
    if not is_admin() and str(args.dir).lower().startswith(
        os.environ.get("ProgramFiles", r"C:\Program Files").lower()
    ):
        raise SystemExit("deploy to Program Files requires Administrator privileges")

    _migrate_legacy_user_data()

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
    if target_dir.resolve() != LEGACY_DIR.resolve():
        _shutdown_server_and_wait(LEGACY_DIR / "ime-server.exe")

    _copy_replace_locked(srv_src, dst_srv)
    if gui_src.exists():
        _copy_replace_locked(gui_src, target_dir / "ime-settings.exe")
        _create_settings_shortcut(target_dir / "ime-settings.exe")
    _copy_if_changed(mod_src, models_dir / "ime_model.bin")
    if rer_src.exists():
        _copy_if_changed(rer_src, models_dir / "reranker.bin")
    _set_startup_run_entry(dst_srv)

    old_gens = discover_registered_generations()

    if not args.hotswap_clsid:
        x64_name, x86_name = dll_names_for_generation(0)
        dst_x64 = target_dir / x64_name
        dst_x86 = target_dir / x86_name
        tsf_unchanged = (
            _file_sha256(dll_src) == _file_sha256(dst_x64)
            and _file_sha256(dll_x86_src) == _file_sha256(dst_x86)
        )
        stable_ok = _is_stable_tip_registered(target_dir)

        if stable_ok and tsf_unchanged:
            _write_generation_file(target_dir, 0)
            locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
            print("TSF DLLs unchanged (stable CLSID); skipped COM/TSF re-registration.")
            if locked:
                print(
                    f"note: {len(locked)} unreferenced old file(s) still mapped by existing processes "
                    "scheduled for automatic deletion on next reboot."
                )
            return

        if stable_ok and not tsf_unchanged:
            replaced_locked_64 = _copy_replace_locked(dll_src, dst_x64)
            replaced_locked_86 = _copy_replace_locked(dll_x86_src, dst_x86)
            _write_generation_file(target_dir, 0)
            _grant_appcontainer_acl(target_dir)
            locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
            print(
                f"updated stable TSF DLLs in-place ({x64_name}, {x86_name}) without changing CLSID; "
                "running apps remain uninterrupted and new apps load the update."
            )
            if replaced_locked_64 or replaced_locked_86 or locked:
                print(
                    f"note: {len(locked)} old file(s) still mapped by running processes "
                    "scheduled for automatic deletion on next reboot."
                )
            return

        # Path 3: Fresh install or one-time migration from legacy generationed CLSIDs (g0001..gFFFF).
        _copy_replace_locked(dll_src, dst_x64)
        _copy_replace_locked(dll_x86_src, dst_x86)
        _write_generation_file(target_dir, 0)
        _grant_appcontainer_acl(target_dir)

        legacy_gens = [g for g in old_gens if g != 0]
        if old_gens or not args.no_hotswap:
            activated, activate_rc = _register_tip_and_migrate(
                0, dst_x64, dst_x86, legacy_gens
            )
            locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
            if legacy_gens:
                old_str = ", ".join(f"g{g:04x}" for g in legacy_gens)
                print(
                    f"migrated legacy TSF {old_str} -> stable CLSID ({TIP_ID}) in {target_dir} "
                    f"({x64_name}, {x86_name}); future deploys will never churn CLSIDs again."
                )
            else:
                print(
                    f"deployed & registered stable TSF ({TIP_ID}) in {target_dir} "
                    f"({x64_name}, {x86_name}, with ALL APPLICATION PACKAGES RX ACL)"
                )
            if not activated:
                print(
                    f"note: session-wide ActivateProfile returned rc={activate_rc} "
                    f"(0x{activate_rc & 0xFFFFFFFF:08x}, e.g. current keyboard is EN-US or isolated desktop); "
                    "InstallLayoutOrTip has already updated active zh-CN TIP."
                )
            if locked:
                print(
                    f"note: {len(locked)} unreferenced old file(s) still mapped by existing processes "
                    "scheduled for automatic deletion on next reboot."
                )
        else:
            locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
            print(
                f"deployed stable TSF files ({x64_name}, {x86_name}) to {target_dir} "
                "(with ALL APPLICATION PACKAGES RX ACL)"
            )
            print("note: run `install_tsf.py register` to register COM + TSF profile.")
        return

    # Opt-in legacy CLSID-generation hot-swap mode (--hotswap-clsid).
    new_gen = choose_next_generation(target_dir, old_gens)
    x64_name, x86_name = dll_names_for_generation(new_gen)
    dst_x64 = target_dir / x64_name
    dst_x86 = target_dir / x86_name

    shutil.copy2(dll_src, dst_x64)
    shutil.copy2(dll_x86_src, dst_x86)
    _write_generation_file(target_dir, new_gen)
    _grant_appcontainer_acl(target_dir)

    if old_gens and not args.no_hotswap:
        activated, activate_rc = _register_tip_and_migrate(
            new_gen, dst_x64, dst_x86, old_gens
        )
        locked = _cleanup_obsolete_dlls(target_dir, {x64_name, x86_name})
        old_str = ", ".join(f"g{g:04x}" for g in old_gens)
        print(
            f"hot-swapped TSF {old_str} -> g{new_gen:04x} ({tip_id_for_generation(new_gen)}) "
            f"in {target_dir} (no app restart required)"
        )
        if not activated:
            print(
                f"note: session-wide ActivateProfile returned rc={activate_rc} "
                f"(0x{activate_rc & 0xFFFFFFFF:08x}, e.g. current keyboard is EN-US or isolated desktop); "
                "InstallLayoutOrTip has already updated active zh-CN TIP."
            )
        if locked:
            print(
                f"note: {len(locked)} unreferenced old file(s) still mapped by existing processes "
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

    activated, activate_rc = _register_tip_and_migrate(target_gen, dll, dll_x86, old_gens)

    x86_msg = f" + 32-bit ({dll_x86})" if dll_x86.exists() else ""
    print(f"registered COM + TSF TIP ({tip_id}): {dll}{x86_msg}")
    if not activated:
        print(
            f"note: session-wide ActivateProfile returned rc={activate_rc} "
            f"(0x{activate_rc & 0xFFFFFFFF:08x}); "
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
    _cleanup_stale_hkcu_tips(None)
    _remove_startup_run_entry()
    _cleanup_obsolete_dlls(base_dir, set())
    gen_str = ", ".join(f"g{g:04x}" for g in gens)
    print(f"unregistered all Pyn TSF generations: {gen_str}")


def cmd_start(args) -> None:
    srv = Path(args.dir) / "ime-server.exe"
    if not srv.exists():
        srv = ROOT / "target" / "release" / "ime-server.exe"
    if not srv.exists():
        raise SystemExit(f"ime-server.exe not found in {args.dir} or target/release")
    base_flags = CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    try:
        subprocess.Popen(
            [str(srv)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=base_flags | CREATE_BREAKAWAY_FROM_JOB,
            close_fds=True,
        )
    except OSError:
        subprocess.Popen(
            [str(srv)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=base_flags,
            close_fds=True,
        )
    print(f"started detached background daemon: {srv}")
    deadline = time.monotonic() + 3.0
    pong_out = None
    while time.monotonic() < deadline:
        res = subprocess.run([str(srv), "--ping"], capture_output=True, text=True)
        if res.returncode == 0:
            pong_out = res.stdout.strip()
            break
        time.sleep(0.15)
    if pong_out:
        print(f"ime-server ping: {pong_out}")
    else:
        print("note: ime-server is still initializing or unreachable via --ping.")


def _create_settings_shortcut(target_exe: Path) -> None:
    appdata = os.environ.get("APPDATA", "")
    if not appdata or not target_exe.exists():
        return
    programs_dir = Path(appdata) / "Microsoft" / "Windows" / "Start Menu" / "Programs"
    if not programs_dir.exists():
        return
    old_lnk = programs_dir / "Input Engine 设置.lnk"
    _try_remove_file(old_lnk)
    lnk = programs_dir / "Pyn 设置.lnk"
    ps = (
        f"$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{str(lnk)}');"
        f"$s.TargetPath='{str(target_exe)}';"
        f"$s.WorkingDirectory='{str(target_exe.parent)}';"
        f"$s.IconLocation='{str(target_exe)},0';"
        "$s.Description='Pyn 外观与输入设置';"
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


def _format_gen_label(gen: int) -> str:
    return "g0000 / stable (ime_tsf.dll)" if gen == 0 else f"g{gen:04x}"


def cmd_status(args) -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    target_dir = Path(args.dir)
    gen_file_val = _read_generation_file(target_dir)
    print(
        f"Deployed generation ({GEN_FILE}): "
        + (_format_gen_label(gen_file_val) if gen_file_val is not None else "none")
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
            tag = _format_gen_label(g)
            print(f"[{tag}] HKLM CLSID x64 {clsid}: {x64_path or 'not registered'}")
            print(f"[{tag}] HKLM CLSID x86 {clsid}: {x86_path or 'not registered'}")
            print(f"[{tag}] HKLM LanguageProfile {LANGID_STR} {prof}: {lp_desc or 'not registered'}")

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


def _pyn_local_data_dir() -> Path:
    local_app_data = os.environ.get("LOCALAPPDATA", "")
    if not local_app_data:
        raise SystemExit("LOCALAPPDATA environment variable is not set")
    return Path(local_app_data) / "Pyn"


def _tail_lines(path: Path, n: int) -> list[str]:
    if not path.is_file():
        return []
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    return lines[-n:] if n > 0 else []


def _read_hkcu_values(subkey: str) -> dict[str, object] | None:
    try:
        vals: dict[str, object] = {}
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, subkey) as k:
            i = 0
            while True:
                try:
                    n, v, _ = winreg.EnumValue(k, i)
                    vals[n] = v
                    i += 1
                except OSError:
                    break
        return vals
    except OSError:
        return None


def _snapshot_tsf_processes() -> list[tuple[int, str, bool, list[str]]]:
    k32 = ctypes.windll.kernel32
    k32.CreateToolhelp32Snapshot.argtypes = [wt.DWORD, wt.DWORD]
    k32.CreateToolhelp32Snapshot.restype = wt.HANDLE
    k32.CloseHandle.argtypes = [wt.HANDLE]
    k32.CloseHandle.restype = wt.BOOL

    invalid_handle = wt.HANDLE(-1).value
    psnap = k32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if psnap == invalid_handle:
        return []

    procs: list[tuple[int, str]] = []
    try:
        pe = _PROCESSENTRY32W(dwSize=ctypes.sizeof(_PROCESSENTRY32W))
        ok = bool(k32.Process32FirstW(psnap, ctypes.byref(pe)))
        while ok:
            procs.append((int(pe.th32ProcessID), str(pe.szExeFile)))
            ok = bool(k32.Process32NextW(psnap, ctypes.byref(pe)))
    finally:
        k32.CloseHandle(psnap)

    rows: list[tuple[int, str, bool, list[str]]] = []
    for pid, name in procs:
        if pid == 0:
            continue
        msnap = k32.CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)
        if msnap == invalid_handle:
            continue
        has_msctf = False
        tsf_mods: list[str] = []
        try:
            me = _MODULEENTRY32W(dwSize=ctypes.sizeof(_MODULEENTRY32W))
            mok = bool(k32.Module32FirstW(msnap, ctypes.byref(me)))
            while mok:
                mod_name = str(me.szModule)
                low = mod_name.lower()
                if low.startswith("ime_tsf"):
                    tsf_mods.append(mod_name)
                elif low == "msctf.dll":
                    has_msctf = True
                mok = bool(k32.Module32NextW(msnap, ctypes.byref(me)))
        finally:
            k32.CloseHandle(msnap)

        if tsf_mods or (name.lower() == "chrome.exe" and has_msctf):
            rows.append((pid, name, has_msctf, tsf_mods))
    return rows


def cmd_debug(args) -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    pyn_dir = _pyn_local_data_dir()
    flag = pyn_dir / "debug_tsf"
    log_path = pyn_dir / "tsf_debug.log"

    if args.on and args.off:
        raise SystemExit("cannot pass both --on and --off")
    if args.on:
        pyn_dir.mkdir(parents=True, exist_ok=True)
        flag.write_text("1\n", encoding="utf-8")
        print(f"TSF debug logging ON ({flag})")
    elif args.off:
        _try_remove_file(flag)
        print(f"TSF debug logging OFF ({flag})")

    if args.clear:
        removed = _try_remove_file(log_path)
        print(f"Cleared TSF debug log ({log_path}): {removed}")

    show_status = not (args.on or args.off or args.clear) or args.tail is not None
    if show_status:
        state = "ON" if flag.is_file() else "OFF"
        size_info = f"{log_path.stat().st_size} bytes" if log_path.is_file() else "missing"
        print(f"Flag: {state} ({flag})")
        print(f"Log:  {log_path} ({size_info})")
        n = args.tail if args.tail is not None else 20
        lines = _tail_lines(log_path, n)
        if lines:
            print(f"--- Last {len(lines)} lines ---")
            for line in lines:
                print(line)


def cmd_diag(args) -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    target_dir = Path(args.dir)
    cur_gen = _read_generation_file(target_dir)
    cur_x64, cur_x86 = dll_names_for_generation(cur_gen) if cur_gen is not None else ("", "")
    cur_lower = {cur_x64.lower(), cur_x86.lower()} - {""}

    print(
        f"Deployed generation ({target_dir / GEN_FILE}): "
        + (
            f"{_format_gen_label(cur_gen)} ({cur_x64}, {cur_x86})"
            if cur_gen is not None
            else "none"
        )
    )

    print("\n=== Processes with ime_tsf* or chrome.exe+msctf ===")
    for pid, name, has_msctf, tsf_mods in _snapshot_tsf_processes():
        mods_lower = {m.lower() for m in tsf_mods}
        if cur_gen == 0:
            if mods_lower & cur_lower:
                tag = "[CURRENT stable]"
            elif any(
                m.startswith(("ime_tsf.dll.old.", "ime_tsf_x86.dll.old."))
                for m in mods_lower
            ):
                tag = "[STABLE_PREV_DLL]"
            elif tsf_mods:
                tag = "[STALE OLD_CLSID]"
            else:
                tag = "[CHROME+MSCTF NO_CURRENT_PYN]"
        else:
            if cur_lower and (mods_lower & cur_lower):
                tag = f"[CURRENT g{cur_gen:04x}]"
            elif tsf_mods:
                tag = "[STALE OLD_CLSID]"
            else:
                tag = "[CHROME+MSCTF NO_CURRENT_PYN]"
        print(f"  PID={pid:6d} {name:26s} {tag:28s} msctf={has_msctf} tsf={tsf_mods}")

    print("\n=== EnumEnabledLayoutOrTip ===")
    for ptype, langid, subst, flags, sz_id in _enum_layout_or_tips():
        print(f"  type={ptype} lang=0x{langid:04x} subst=0x{subst:08x} flags=0x{flags:x} id={sz_id}")

    print(f"\n=== HKCU\\{ZH_CN_ASSEMBLY_KEY} ===")
    asm = _read_hkcu_values(ZH_CN_ASSEMBLY_KEY)
    if asm is None:
        print("  (missing)")
    else:
        for key in ("Default", "Profile", "KeyboardLayout"):
            if key in asm:
                print(f"  {key} = {asm[key]!r}")

    print(f"\n=== HKCU\\{ZH_CN_SORTORDER_ITEM0_KEY} ===")
    sort0 = _read_hkcu_values(ZH_CN_SORTORDER_ITEM0_KEY)
    if sort0 is None:
        print("  (missing)")
    else:
        for key in ("CLSID", "Profile", "KeyboardLayout"):
            if key in sort0:
                print(f"  {key} = {sort0[key]!r}")

    pyn_dir = _pyn_local_data_dir()
    flag = pyn_dir / "debug_tsf"
    log_path = pyn_dir / "tsf_debug.log"
    print(f"\n=== TSF Debug Log (flag={'ON' if flag.is_file() else 'OFF'}) ===")
    if log_path.is_file():
        lines = _tail_lines(log_path, 40)
        print(f"  {log_path} ({log_path.stat().st_size} bytes), showing last {len(lines)} lines:")
        for line in lines:
            print("  " + line)
    else:
        print(f"  {log_path} (not found)")


def main() -> None:
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)

    p_dep = sub.add_parser("deploy")
    p_dep.add_argument("--dir", default=str(DEFAULT_DIR))
    p_dep.add_argument("--hotswap-clsid", action="store_true")
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

    p_dbg = sub.add_parser("debug")
    p_dbg.add_argument("--on", action="store_true")
    p_dbg.add_argument("--off", action="store_true")
    p_dbg.add_argument("--clear", action="store_true")
    p_dbg.add_argument("--tail", nargs="?", const=40, type=int, default=None)

    p_diag = sub.add_parser("diag")
    p_diag.add_argument("--dir", default=str(DEFAULT_DIR))

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
        "debug": cmd_debug,
        "diag": cmd_diag,
        "settings": cmd_settings,
    }[args.cmd](args)


if __name__ == "__main__":
    main()
