#!/usr/bin/env python3
"""
Differential test: run a candidate pipeline and the reference on many generator
seeds and diff their outputs exactly.

Usage:
  scripts/diff_seeds.py [--seeds 2-30] [--events 200000] [--clicks 50000] -- CMD...

CMD gets DATA_DIR and OUTPUT_DIR as env vars and as its last two arguments,
the same convention as run_fixtures.py. Datasets and reference outputs are
cached under data/ (gitignored), so reruns only run the candidate.
Needs `generate` on PATH or at .tools/generate (scripts/build_generator.sh).
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
PY = sys.executable


def generator():
    local = ROOT / ".tools" / "generate"
    if local.exists():
        return str(local)
    found = shutil.which("generate")
    if not found:
        sys.exit("no generator: run scripts/build_generator.sh")
    return found


def seed_range(text):
    seeds = []
    for part in text.split(","):
        lo, _, hi = part.partition("-")
        seeds.extend(range(int(lo), int(hi or lo) + 1))
    return seeds


def run(cmd, data, out):
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    env = dict(os.environ, DATA_DIR=str(data), OUTPUT_DIR=str(out))
    t = time.perf_counter()
    p = subprocess.run(cmd + [str(data), str(out)], env=env, capture_output=True, text=True)
    return p, time.perf_counter() - t


def main():
    sep = sys.argv.index("--") if "--" in sys.argv else len(sys.argv)
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", default="2-30")
    ap.add_argument("--events", type=int, default=200_000)
    ap.add_argument("--clicks", type=int, default=50_000)
    args = ap.parse_args(sys.argv[1:sep])
    cmd = sys.argv[sep + 1:]
    if not cmd:
        sys.exit("give the candidate command after --")

    failed = []
    for seed in seed_range(args.seeds):
        base = ROOT / "data" / f"seed{seed}-e{args.events}-c{args.clicks}"
        data, ref_out, cand_out = base / "input", base / "reference", base / "candidate"
        if not (data / "manifest.json").exists():
            subprocess.run([generator(), "--seed", str(seed), "--events", str(args.events),
                            "--clicks", str(args.clicks), "--output", str(data)], check=True,
                           stdout=subprocess.DEVNULL)
        if not (ref_out / "user_daily").exists():
            p, _ = run([PY, str(ROOT / "reference" / "reference.py")], data, ref_out)
            if p.returncode:
                sys.exit(f"reference failed on seed {seed}:\n{p.stderr[-2000:]}")
        p, secs = run(cmd, data, cand_out)
        if p.returncode:
            failed.append(seed)
            print(f"FAIL seed {seed}: exit {p.returncode}\n{p.stderr[-2000:]}")
            continue
        cmp = subprocess.run([PY, str(ROOT / "scripts" / "compare.py"), str(cand_out), str(ref_out)],
                             capture_output=True, text=True)
        if cmp.returncode:
            failed.append(seed)
            print(f"FAIL seed {seed} ({secs:.2f}s)\n{cmp.stdout}")
        else:
            print(f"ok   seed {seed} ({secs:.2f}s)")
    total = len(seed_range(args.seeds))
    print(f"\n{total - len(failed)}/{total} seeds matched" + (f"; failed: {failed}" if failed else ""))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
