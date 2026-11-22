"""Run the evaluation suites against an engine and write a report.

    python tools/evalkit/run_eval.py --engine oracle
    python tools/evalkit/run_eval.py --engine rime-gram --suites function_words
    python tools/evalkit/run_eval.py --engine cmd --cmd "target/release/ime-cli --jsonl"

Reports go to eval/reports/<engine>.json.
"""
from __future__ import annotations

import argparse
import json
import shlex
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from engines import EchoEngine, Engine, OracleEngine, ProcessEngine, RimeEngine  # noqa: E402
from pinyin_util import is_hanzi, readings_of  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SETS = ROOT / "eval" / "sets"
REPORTS = ROOT / "eval" / "reports"
EXTERNAL = ROOT / ".external"
CASE_SUITES = ["function_words", "incremental", "abbrev", "precedence", "de_grammar", "general"]
NBEST = 10
RIME_DIRS = {"rime-plain": "rime_plain", "rime-gram": "rime_gram", "rime-gram-ctx": "rime_gram_ctx"}


def load_suite(name: str) -> list[dict]:
    path = SETS / f"{name}.jsonl"
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


# -- scoring ------------------------------------------------------------------

def edit_distance(a: str, b: str) -> int:
    row = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        prev, row[0] = row[0], i
        for j, cb in enumerate(b, 1):
            prev, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, prev + (ca != cb))
    return row[-1]


def spellable(text: str, keys: str) -> bool:
    """True if `text` can be read exactly as the typed keys, syllable by syllable."""
    if not is_hanzi(text):
        return False
    positions = {0}
    for char in text:
        positions = {
            pos + len(reading)
            for pos in positions
            for reading in readings_of(char)
            if keys.startswith(reading, pos)
        }
        if not positions:
            return False
    return len(keys) in positions


def score(case: dict, candidates: list[str]) -> dict:
    top = candidates[0] if candidates else ""
    expected = case["expected"]
    result = {"id": case["id"], "top1": top}
    if expected is not None:
        result["exact"] = top == expected
        result["nbest"] = expected in candidates
        result["errors"] = edit_distance(top, expected)
        result["length"] = len(expected)
    if case["check"] == "reading":
        result["passed"] = spellable(top, case["keys"])
    elif case["focus"]:
        result["passed"] = len(top) == len(expected) and all(
            top[i] == expected[i] for i in case["focus"]
        )
    else:
        result["passed"] = result["exact"]
    return result


def summarize(cases: list[dict], results: list[dict]) -> dict:
    by_id = {case["id"]: case for case in cases}
    groups: dict[str, list[dict]] = {"all": results}
    for result in results:
        groups.setdefault(by_id[result["id"]]["split"], []).append(result)
    summary = {}
    for key, group in groups.items():
        texted = [r for r in group if "exact" in r]
        gated = [r for r in group if by_id[r["id"]]["gate"]]
        focused = [r for r in group if by_id[r["id"]]["focus"] or by_id[r["id"]]["check"] == "reading"]
        length = sum(r["length"] for r in texted)
        summary[key] = {
            "cases": len(group),
            "exact": ratio(sum(r["exact"] for r in texted), len(texted)),
            "nbest": ratio(sum(r["nbest"] for r in texted), len(texted)),
            "cer": ratio(sum(r["errors"] for r in texted), length),
            "focus_pass": ratio(sum(r["passed"] for r in focused), len(focused)),
            "gate_cases": len(gated),
            "gate_failures": sum(not r["passed"] for r in gated),
        }
    return summary


def ratio(numerator: int, denominator: int) -> float | None:
    return round(numerator / denominator, 4) if denominator else None


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * fraction))]


# -- running ------------------------------------------------------------------

def run_cases(engine: Engine, cases: list[dict], timings: list[float]) -> list[dict]:
    results = []
    for case in cases:
        engine.begin_case(case)
        context_ok = engine.set_context(case["context"])
        started = time.perf_counter()
        candidates = engine.convert(case["keys"], NBEST)
        timings.append((time.perf_counter() - started) * 1000)
        result = score(case, candidates)
        if case["context"]:
            result["context_ok"] = context_ok
        results.append(result)
    return results


def _run_pollution_checks(engine: Engine, checks: list[dict], sink: list[float]) -> list[dict]:
    """Run checks against a checkpointed engine state without context bleed."""
    engine.checkpoint()
    zero_ctx = [c for c in checks if not c["context"]]
    abbrev_ctx = [c for c in checks if c["context"] and c["suite"] == "abbrev"]
    other_ctx = [c for c in checks if c["context"] and c["suite"] != "abbrev"]

    results: list[dict] = []
    results.extend(run_cases(engine, zero_ctx, sink))
    for case in abbrev_ctx:
        engine.restore_checkpoint()
        results.extend(run_cases(engine, [case], sink))
    engine.restore_checkpoint()
    results.extend(run_cases(engine, other_ctx, sink))

    by_id = {r["id"]: r for r in results}
    return [by_id[c["id"]] for c in checks]


def run_pollution(engine: Engine, checks: list[dict], scenarios: list[dict]) -> dict:
    """A scenario fails if a check that passed on a clean engine fails afterwards."""
    sink: list[float] = []
    engine.isolate_contexts = False
    engine.reset()
    clean = {r["id"]: r for r in _run_pollution_checks(engine, checks, sink)}
    clean_passed = sum(1 for r in clean.values() if r["passed"])
    total_checks = len(checks)
    report = {}
    for scenario in scenarios:
        engine.reset()
        applied = True
        for step in scenario["preamble"]:
            for _ in range(step["times"]):
                engine.set_context([])
                engine.convert(step["keys"], 50)
                applied &= engine.commit(step["pick"])
        after = _run_pollution_checks(engine, checks, sink)
        regressions = [
            {"id": r["id"], "clean": clean[r["id"]]["top1"], "after": r["top1"]}
            for r in after
            if clean[r["id"]]["passed"] and not r["passed"]
        ]
        report[scenario["id"]] = {
            "desc": scenario["desc"],
            "preamble_applied": applied,
            "clean_passed": clean_passed,
            "total_checks": total_checks,
            "regressions": len(regressions),
            "examples": regressions[:8],
        }
    engine.reset()
    engine.isolate_contexts = True
    return report


def build_engine(args) -> Engine:
    if args.engine == "oracle":
        return OracleEngine()
    if args.engine == "echo":
        return EchoEngine()
    if args.engine in RIME_DIRS:
        return RimeEngine(EXTERNAL / "librime" / "dist", EXTERNAL / RIME_DIRS[args.engine],
                          schema="wanxiang", name=args.engine)
    if args.engine == "cmd":
        if not args.cmd:
            raise SystemExit("--engine cmd needs --cmd")
        return ProcessEngine(shlex.split(args.cmd), name=args.name)
    raise SystemExit(f"unknown engine {args.engine!r}")


def print_table(name: str, report: dict) -> None:
    print(f"\n== {name} ==")
    header = f"{'suite':16s}{'split':9s}{'cases':>6s}{'exact':>8s}{'nbest':>8s}{'CER':>8s}{'focus':>8s}{'gate fail':>12s}"
    print(header)
    for suite, summary in report["suites"].items():
        for split, row in summary.items():
            def show(value):
                return "-" if value is None else f"{value * 100:.2f}"
            gate = f"{row['gate_failures']}/{row['gate_cases']}" if row["gate_cases"] else "-"
            print(f"{suite:16s}{split:9s}{row['cases']:6d}{show(row['exact']):>8s}"
                  f"{show(row['nbest']):>8s}{show(row['cer']):>8s}{show(row['focus_pass']):>8s}{gate:>12s}")
    pollution = report.get("pollution", {})
    if pollution:
        first = next(iter(pollution.values()))
        if "clean_passed" in first and "total_checks" in first:
            print(f"pollution baseline clean_passed={first['clean_passed']}/{first['total_checks']}")
    for scenario, row in pollution.items():
        note = "" if row["preamble_applied"] else "  (preamble could not be fully applied)"
        print(f"pollution {scenario:24s} regressions={row['regressions']}{note}")
    timing = report.get("latency_ms")
    if timing:
        print(f"convert latency ms: p50={timing['p50']:.2f} p95={timing['p95']:.2f} max={timing['max']:.2f}")
    if report.get("context_not_reproduced"):
        print(f"contexts the engine could not take as given: {report['context_not_reproduced']}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", required=True)
    parser.add_argument("--cmd")
    parser.add_argument("--name")
    parser.add_argument("--suites", nargs="*", default=CASE_SUITES + ["pollution"])
    parser.add_argument("--limit", type=int, help="first N cases per suite (smoke runs)")
    parser.add_argument("--no-report", action="store_true")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    suites = {name: load_suite(name) for name in CASE_SUITES if name in args.suites}
    if args.limit:
        suites = {name: cases[: args.limit] for name, cases in suites.items()}
    checks = [c for name in ("function_words", "incremental", "abbrev")
              for c in load_suite(name) if c["gate"]]
    if args.limit:
        checks = checks[:: max(1, len(checks) // args.limit)]
    engine = build_engine(args)

    report = {"engine": engine.name, "suites": {}, "cases": {}}
    timings: list[float] = []
    bad_contexts = 0
    started = time.time()
    try:
        engine.reset()
        for name, cases in suites.items():
            results = run_cases(engine, cases, timings)
            bad_contexts += sum(1 for r in results if r.get("context_ok") is False)
            report["suites"][name] = summarize(cases, results)
            report["cases"][name] = results
        if "pollution" in args.suites:
            scenarios = json.loads((ROOT / "eval" / "src" / "pollution.json").read_text(encoding="utf-8"))
            report["pollution"] = run_pollution(engine, checks, scenarios)
    finally:
        engine.close()
    if timings:
        report["latency_ms"] = {"p50": percentile(timings, 0.5), "p95": percentile(timings, 0.95),
                                "max": max(timings)}
    report["context_not_reproduced"] = bad_contexts
    report["seconds"] = round(time.time() - started, 1)

    print_table(engine.name, report)
    if not args.no_report:
        REPORTS.mkdir(parents=True, exist_ok=True)
        out = REPORTS / f"{engine.name}.json"
        out.write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"\nreport: {out}")


if __name__ == "__main__":
    main()
