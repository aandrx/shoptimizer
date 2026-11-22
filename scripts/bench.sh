#!/usr/bin/env bash
# S3-0 bench and oracle targets (PLAN.md §6 S3-0, gates G3/G4 in §7).
#
#   scripts/bench.sh oracle          # SHOPT_ENGINE=v1 CAND DATA -> DATA.v1out (once)
#   scripts/bench.sh bench           # CAND RUNS times on DATA -> OUT, median + peak RSS, then diff
#
# Everything is configured through the environment so `make oracle` / `make bench`
# stay thin. CAND follows the candidate CLI convention: it receives DATA_DIR and
# OUTPUT_DIR as env vars and as its last two arguments. It may contain arguments
# (e.g. "python reference/reference.py"); it is split on whitespace.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

DATA=${DATA:-data/dev}
RUNS=${RUNS:-3}
OUT=${OUT:-out-bench}
NCPU=${NCPU:-8}
CPUSET=${CPUSET:-0-3,8-11}
CAND=${CAND:-target/release/shoptimizer}
PY=${PY:-.venv/bin/python}
TIMING_PY=${TIMING_PY:-python3}
# DATA=data/p20/ would otherwise put the oracle at data/p20/.v1out.
DATA=${DATA%/}
OUT=${OUT%/}
ORACLE=${ORACLE:-$DATA.v1out}
ORACLE=${ORACLE%/}

log() { printf 'bench: %s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

usage() {
    printf 'usage: %s {oracle|bench}\n' "${BASH_SOURCE[0]}" >&2
    exit 2
}

# True when DIR/TABLE holds at least one Parquet file whose name the rules do not ignore.
any_parquet() {
    local dir=$1 file base
    for file in "$dir"/*.parquet; do
        [ -f "$file" ] || continue
        base=${file##*/}
        case $base in .* | _*) continue ;; esac
        return 0
    done
    return 1
}

# An output folder counts as complete only when both tables are present.
output_complete() {
    local dir=$1
    any_parquet "$dir/user_daily" && any_parquet "$dir/campaign_daily"
}

resolve_cand() {
    local exe
    CAND_CMD=()
    read -r -a CAND_CMD <<<"$CAND" || true
    [ "${#CAND_CMD[@]}" -gt 0 ] || die "CAND is empty"
    exe=${CAND_CMD[0]}
    if [[ $exe == */* ]]; then
        [ -x "$exe" ] || die "candidate not executable: $exe (build it first)"
    else
        command -v -- "$exe" >/dev/null 2>&1 || die "candidate not on PATH: $exe"
    fi
}

# The venv python carries polars for scripts/compare.py; fall back with a warning.
resolve_compare_py() {
    if [ -x "$PY" ]; then
        return 0
    fi
    if command -v python3 >/dev/null 2>&1; then
        log "PY=$PY is not executable; falling back to python3 (needs polars: make setup)"
        PY=python3
        return 0
    fi
    die "no python for scripts/compare.py; set PY=... or run make setup"
}

# taskset is optional: a missing binary or an invalid CPU list must not stop a bench.
PIN_FLAG=0
PIN_NOTE=", unpinned"
pin_cpus() {
    if command -v taskset >/dev/null 2>&1 && taskset -c "$CPUSET" true >/dev/null 2>&1; then
        PIN_FLAG=1
        PIN_NOTE=", taskset -c $CPUSET"
        return 0
    fi
    log "taskset -c $CPUSET unavailable here; running unpinned"
    PIN_FLAG=0
    PIN_NOTE=", unpinned"
}

# Never let `rm -rf $OUT` touch the input or the oracle.
guard_paths() {
    local out_abs data_abs oracle_abs
    out_abs=$(realpath -m -- "$OUT")
    data_abs=$(realpath -m -- "$DATA")
    oracle_abs=$(realpath -m -- "$ORACLE")
    [ -n "$out_abs" ] && [ "$out_abs" != / ] || die "refusing to use OUT=$OUT"
    case "$data_abs/" in
        "$out_abs"/*) die "refusing: OUT ($OUT) contains DATA ($DATA)" ;;
    esac
    case "$oracle_abs/" in
        "$out_abs"/*) die "refusing: OUT ($OUT) contains the oracle ($ORACLE)" ;;
    esac
}

oracle() {
    [ -d "$DATA" ] || die "DATA directory not found: $DATA"
    local final=$ORACLE tmp
    if output_complete "$final"; then
        log "oracle: $final exists, skipping"
        return 0
    fi
    if [ -e "$final" ]; then
        log "oracle: $final is incomplete, rebuilding it"
    fi
    resolve_cand
    tmp="${final}.tmp.$$"
    rm -rf "$tmp"
    # shellcheck disable=SC2064 # expand tmp now: the local is gone when the trap runs
    trap "rm -rf -- $(printf %q "$tmp")" EXIT INT TERM
    log "oracle: running SHOPT_ENGINE=v1 ${CAND_CMD[*]} $DATA -> $tmp"
    SHOPT_ENGINE=v1 DATA_DIR="$DATA" OUTPUT_DIR="$tmp" "${CAND_CMD[@]}" "$DATA" "$tmp"
    output_complete "$tmp" || die "oracle run wrote no complete output in $tmp"
    rm -rf "$final"
    mv -- "$tmp" "$final"
    trap - EXIT INT TERM
    log "oracle: wrote $final"
}

bench() {
    [ -d "$DATA" ] || die "DATA directory not found: $DATA"
    [[ $RUNS =~ ^[1-9][0-9]*$ ]] || die "RUNS must be a positive integer, got: $RUNS"
    resolve_cand
    pin_cpus
    guard_paths

    local taskset_flag=$PIN_FLAG

    log "bench: ${RUNS} run(s) of ${CAND_CMD[*]} on $DATA -> $OUT (NCPU=$NCPU$PIN_NOTE)"
    local rc=0
    timing=$(BENCH_TASKSET=$taskset_flag BENCH_CPUSET=$CPUSET NCPU=$NCPU \
        "$TIMING_PY" - "$RUNS" "$OUT" "$DATA" -- "${CAND_CMD[@]}" <<'PY'
import os
import resource
import shutil
import statistics
import subprocess
import sys
import time


def main():
    runs = int(sys.argv[1])
    out, data = sys.argv[2], sys.argv[3]
    cand = sys.argv[sys.argv.index("--") + 1:]
    if not cand:
        print("bench: no candidate command", file=sys.stderr)
        return 2

    prefix = []
    if os.environ.get("BENCH_TASKSET") == "1":
        prefix = ["taskset", "-c", os.environ.get("BENCH_CPUSET", "0-3,8-11")]
    env = dict(os.environ, DATA_DIR=data, OUTPUT_DIR=out,
               NCPU=os.environ.get("NCPU", "8"))

    walls = []
    for run in range(1, runs + 1):
        shutil.rmtree(out, ignore_errors=True)
        print(f"=== run {run}/{runs} ===", file=sys.stderr, flush=True)
        started = time.perf_counter()
        try:
            done = subprocess.run(prefix + cand + [data, out], env=env)
        except OSError as exc:
            print(f"bench: cannot run {cand[0]}: {exc}", file=sys.stderr)
            return 2
        wall = time.perf_counter() - started
        if done.returncode != 0:
            print(f"bench: run {run} failed with exit {done.returncode}; "
                  f"not comparing a partial output", file=sys.stderr, flush=True)
            return 1
        walls.append(wall)
        peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024
        print(f"[bench] run {run}: {wall:.3f} s, peak RSS so far {peak:.0f} MiB",
              file=sys.stderr, flush=True)

    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024
    print("[bench] walls: " + ", ".join(f"{wall:.3f} s" for wall in walls),
          file=sys.stderr)
    print(f"[bench] median wall: {statistics.median(walls):.3f} s over {runs} run(s)",
          file=sys.stderr)
    # ru_maxrss for RUSAGE_CHILDREN is the maximum over all waited-for children,
    # so this is the peak across the runs, never a per-run undercount.
    print(f"[bench] peak RSS: {peak:.0f} MiB (max over runs)", file=sys.stderr,
          flush=True)
    return 0


sys.exit(main())
PY
    ) || rc=$?
    if [ "$rc" -ne 0 ]; then
        log "bench: timing run failed (exit $rc); skipping comparison"
        return 1
    fi

    if [ ! -e "$ORACLE" ]; then
        log "oracle missing: $ORACLE"
        log "run 'make oracle DATA=$DATA' first"
        printf 'DIFFER\n'
        return 1
    fi
    resolve_compare_py
    if ! any_parquet "$OUT/user_daily" || ! any_parquet "$OUT/campaign_daily"; then
        log "candidate output is missing a table in $OUT"
        printf 'DIFFER\n'
        return 1
    fi

    local cmp=0 compare_out
    compare_out=$("$PY" scripts/compare.py "$OUT" "$ORACLE" 2>&1) || cmp=$?
    printf '%s\n' "$compare_out"
    [ "$cmp" -eq 0 ] && return 0          # compare.py printed MATCH
    grep -qx 'DIFFER' <<<"$compare_out" && return 1   # compare.py printed DIFFER
    log "comparison failed with exit $cmp (needs polars: make setup; check $OUT and $ORACLE)"
    printf 'DIFFER\n'
    return 1
}

case ${1:-} in
    oracle) oracle ;;
    bench) bench ;;
    *) usage ;;
esac
