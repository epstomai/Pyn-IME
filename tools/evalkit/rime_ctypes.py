"""Thin ctypes binding over librime (rime.dll) through its versioned API table.

Only what the evaluation baseline needs: deploy, sessions, set input,
enumerate candidates, select, commit, options and properties.

The member order of the API table is read from rime_api.h next to the DLL,
so the binding follows whatever librime build is installed.
"""
from __future__ import annotations

import ctypes as C
import os
import re
from pathlib import Path

SessionId = C.c_size_t
Bool = C.c_int


class RimeTraits(C.Structure):
    _fields_ = [
        ("data_size", C.c_int),
        ("shared_data_dir", C.c_char_p),
        ("user_data_dir", C.c_char_p),
        ("distribution_name", C.c_char_p),
        ("distribution_code_name", C.c_char_p),
        ("distribution_version", C.c_char_p),
        ("app_name", C.c_char_p),
        ("modules", C.POINTER(C.c_char_p)),
        ("min_log_level", C.c_int),
        ("log_dir", C.c_char_p),
        ("prebuilt_data_dir", C.c_char_p),
        ("staging_dir", C.c_char_p),
    ]


class RimeCandidate(C.Structure):
    _fields_ = [
        ("text", C.c_char_p),
        ("comment", C.c_char_p),
        ("reserved", C.c_void_p),
    ]


class RimeCandidateListIterator(C.Structure):
    _fields_ = [
        ("ptr", C.c_void_p),
        ("index", C.c_int),
        ("candidate", RimeCandidate),
    ]


class RimeCommit(C.Structure):
    _fields_ = [("data_size", C.c_int), ("text", C.c_char_p)]


class RimeStatus(C.Structure):
    _fields_ = [
        ("data_size", C.c_int),
        ("schema_id", C.c_char_p),
        ("schema_name", C.c_char_p),
        ("is_disabled", C.c_int),
        ("is_composing", C.c_int),
        ("is_ascii_mode", C.c_int),
        ("is_full_shape", C.c_int),
        ("is_simplified", C.c_int),
        ("is_traditional", C.c_int),
        ("is_ascii_punct", C.c_int),
    ]


# name -> (restype, argtypes); only the entries this binding calls.
_SIGNATURES = {
    "setup": (None, [C.POINTER(RimeTraits)]),
    "initialize": (None, [C.POINTER(RimeTraits)]),
    "finalize": (None, []),
    "start_maintenance": (Bool, [Bool]),
    "join_maintenance_thread": (None, []),
    "create_session": (SessionId, []),
    "destroy_session": (Bool, [SessionId]),
    "process_key": (Bool, [SessionId, C.c_int, C.c_int]),
    "commit_composition": (Bool, [SessionId]),
    "clear_composition": (None, [SessionId]),
    "get_commit": (Bool, [SessionId, C.POINTER(RimeCommit)]),
    "free_commit": (Bool, [C.POINTER(RimeCommit)]),
    "get_status": (Bool, [SessionId, C.POINTER(RimeStatus)]),
    "free_status": (Bool, [C.POINTER(RimeStatus)]),
    "set_option": (None, [SessionId, C.c_char_p, Bool]),
    "get_option": (Bool, [SessionId, C.c_char_p]),
    "set_property": (None, [SessionId, C.c_char_p, C.c_char_p]),
    "select_schema": (Bool, [SessionId, C.c_char_p]),
    "get_input": (C.c_char_p, [SessionId]),
    "set_input": (Bool, [SessionId, C.c_char_p]),
    "select_candidate": (Bool, [SessionId, C.c_size_t]),
    "delete_candidate": (Bool, [SessionId, C.c_size_t]),
    "get_version": (C.c_char_p, []),
    "candidate_list_begin": (Bool, [SessionId, C.POINTER(RimeCandidateListIterator)]),
    "candidate_list_next": (Bool, [C.POINTER(RimeCandidateListIterator)]),
    "candidate_list_end": (None, [C.POINTER(RimeCandidateListIterator)]),
}


def _api_member_names(header: Path) -> list[str]:
    text = header.read_text(encoding="utf-8")
    start = text.index("typedef struct RIME_FLAVORED(rime_api_t) {")
    end = text.index("} RIME_FLAVORED(RimeApi);")
    body = re.sub(r"//.*", "", text[start:end])
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.S)
    names = []
    for decl in body.split(";")[1:]:
        found = re.findall(r"\(\*(\w+)\)", decl)
        if len(found) == 1:
            names.append(found[0])
        elif decl.strip():
            raise ValueError(f"unparsed API member: {decl.strip()!r}")
    return names


def _init(struct):
    struct.data_size = C.sizeof(type(struct)) - C.sizeof(C.c_int)
    return struct


class Rime:
    """One librime instance bound to a user data directory."""

    def __init__(self, dist_dir: Path, user_dir: Path, log_dir: Path | None = None):
        dist_dir = Path(dist_dir)
        self.user_dir = Path(user_dir)
        os.add_dll_directory(str(dist_dir / "lib"))
        self._dll = C.CDLL(str(dist_dir / "lib" / "rime.dll"))
        names = _api_member_names(dist_dir / "include" / "rime_api.h")

        fields = [("data_size", C.c_int)] + [(n, C.c_void_p) for n in names]
        table_type = type("RimeApi", (C.Structure,), {"_fields_": fields})
        self._dll.rime_get_api.restype = C.POINTER(table_type)
        table = self._dll.rime_get_api().contents
        available = table.data_size + C.sizeof(C.c_int)

        self._fn = {}
        for name, (restype, argtypes) in _SIGNATURES.items():
            offset = getattr(table_type, name).offset
            pointer = getattr(table, name)
            if offset >= available or not pointer:
                raise RuntimeError(f"librime build lacks API function {name!r}")
            self._fn[name] = C.CFUNCTYPE(restype, *argtypes)(pointer)

        self._log_dir = str(log_dir).encode("utf-8") if log_dir else b""
        self._keep: list[RimeTraits] = []
        self._setup_done = False
        self.session = None

    # -- lifecycle -------------------------------------------------------
    def _traits(self) -> RimeTraits:
        traits = _init(RimeTraits())
        user_dir = str(self.user_dir).encode("utf-8")
        traits.shared_data_dir = user_dir
        traits.user_data_dir = user_dir
        traits.distribution_name = b"ime-eval"
        traits.distribution_code_name = b"ime-eval"
        traits.distribution_version = b"0.1"
        traits.app_name = b"rime.ime-eval"
        traits.min_log_level = 2
        traits.log_dir = self._log_dir
        self._keep.append(traits)
        return traits

    def start(self, full_check: bool = True) -> None:
        traits = self._traits()
        if not self._setup_done:
            self._fn["setup"](C.byref(traits))
            self._setup_done = True
        self._fn["initialize"](C.byref(traits))
        if self._fn["start_maintenance"](1 if full_check else 0):
            self._fn["join_maintenance_thread"]()
        self.session = self._fn["create_session"]()

    def stop(self) -> None:
        if self.session is not None:
            self._fn["destroy_session"](self.session)
            self.session = None
        self._fn["finalize"]()

    def version(self) -> str:
        return self._fn["get_version"]().decode("utf-8")

    # -- session ops -----------------------------------------------------
    def select_schema(self, schema_id: str) -> bool:
        return bool(self._fn["select_schema"](self.session, schema_id.encode()))

    def schema(self) -> str:
        status = _init(RimeStatus())
        if not self._fn["get_status"](self.session, C.byref(status)):
            return ""
        schema_id = (status.schema_id or b"").decode("utf-8")
        self._fn["free_status"](C.byref(status))
        return schema_id

    def set_option(self, name: str, value: bool) -> None:
        self._fn["set_option"](self.session, name.encode(), 1 if value else 0)

    def get_option(self, name: str) -> bool:
        return bool(self._fn["get_option"](self.session, name.encode()))

    def set_property(self, name: str, value: str) -> None:
        self._fn["set_property"](self.session, name.encode(), value.encode("utf-8"))

    def clear(self) -> None:
        self._fn["clear_composition"](self.session)

    def set_input(self, text: str) -> bool:
        return bool(self._fn["set_input"](self.session, text.encode("ascii")))

    def get_input(self) -> str:
        raw = self._fn["get_input"](self.session)
        return raw.decode("utf-8") if raw else ""

    def process_key(self, keycode: int, mask: int = 0) -> bool:
        return bool(self._fn["process_key"](self.session, keycode, mask))

    def candidates(self, limit: int) -> list[tuple[str, str]]:
        iterator = RimeCandidateListIterator()
        out: list[tuple[str, str]] = []
        if not self._fn["candidate_list_begin"](self.session, C.byref(iterator)):
            return out
        while len(out) < limit and self._fn["candidate_list_next"](C.byref(iterator)):
            text = (iterator.candidate.text or b"").decode("utf-8", "replace")
            comment = (iterator.candidate.comment or b"").decode("utf-8", "replace")
            out.append((text, comment))
        self._fn["candidate_list_end"](C.byref(iterator))
        return out

    def select(self, index: int) -> bool:
        return bool(self._fn["select_candidate"](self.session, index))

    def commit_composition(self) -> bool:
        return bool(self._fn["commit_composition"](self.session))

    def take_commit(self) -> str:
        commit = _init(RimeCommit())
        if not self._fn["get_commit"](self.session, C.byref(commit)):
            return ""
        text = (commit.text or b"").decode("utf-8", "replace")
        self._fn["free_commit"](C.byref(commit))
        return text
