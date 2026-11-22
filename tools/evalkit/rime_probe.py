"""Smoke test: deploy the wanxiang schema and convert a few inputs."""
from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from rime_ctypes import Rime  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / ".external" / "librime" / "dist"


def main() -> None:
    user_dir = Path(sys.argv[1]).resolve()
    inputs = sys.argv[2:] or ["woshuole", "nichifanlema", "le", "de"]
    sys.stdout.reconfigure(encoding="utf-8")
    rime = Rime(DIST, user_dir, log_dir=user_dir.parent / "rime_logs")
    t0 = time.time()
    rime.start(full_check=True)
    print(f"librime {rime.version()} started in {time.time() - t0:.1f}s")
    print("schema before select:", rime.schema())
    print("select wanxiang:", rime.select_schema("wanxiang"), "->", rime.schema())
    for text in inputs:
        rime.clear()
        ok = rime.set_input(text)
        cands = rime.candidates(8)
        print(f"{text!r} set_input={ok} input={rime.get_input()!r}")
        for i, (cand, comment) in enumerate(cands):
            print(f"   {i}: {cand}   [{comment}]")
    rime.stop()


if __name__ == "__main__":
    main()
