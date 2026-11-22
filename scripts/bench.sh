#!/usr/bin/env bash
# S3-0 bench and oracle targets (PLAN.md §6 S3-0, gates G3/G4 in §7).
#
#   scripts/bench.sh oracle   # SHOPT_ENGINE=v1 target/release/shoptimizer DATA -> DATA.v1out (once)
#   scripts/bench.sh bench    # CAND RUNS times in out-bench/, median + peak RSS, then diff
#
# Bench output is always <repo>/out-bench/. It is deleted only when it holds
# candidate output: user_daily/ and campaign_daily/ with regular files and
# nothing else. A symlink or a foreign entry makes the bench refuse to run.
# CAND follows the candidate CLI convention: it receives DATA_DIR and OUTPUT_DIR
# as env vars and as its last two arguments, and may contain arguments
# (e.g. "python reference/reference.py"); it is split on whitespace.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

DATA=${DATA:-data/dev}
DATA=${DATA%/}
RUNS=${RUNS:-3}
PY=${PY:-.venv/bin/python}
CAND=${CAND:-target/release/shoptimizer}

# Fixed by the card: the candidate binary, the oracle output and the pin shield.
V1_BIN=target/release/shoptimizer
BENCH_OUT=$ROOT/out-bench
CPUSET=0-3,8-11
VCPUS=8
TIMING_PY=python3

log() { printf 'bench: %s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

usage() {
    printf 'usage: %s {oracle|bench}\n' "${BASH_SOURCE[0]}" >&2
    exit 2
}

# True when DIR/TABLE holds at least one Parquet file the rules do not ignore.
any_parquet() {
    local dir=$1 file base
    for file in "$dir"/*.parquet; do
        [ -L "$file" ] && continue
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

# The bench box is 4 cores x 2 threads; an unpinned run is not a comparable number.
require_pinned_cpus() {
    command -v "$TIMING_PY" >/dev/null 2>&1 || die "$TIMING_PY is required for timing"
    command -v taskset >/dev/null 2>&1 || die "taskset is required (bench runs on $VCPUS vCPUs)"
    taskset -c "$CPUSET" true >/dev/null 2>&1 || die "taskset -c $CPUSET is not runnable here"
    local cpus
    cpus=$(taskset -c "$CPUSET" "$TIMING_PY" -c 'import os; print(len(os.sched_getaffinity(0)))') ||
        die "could not count the CPUs under taskset -c $CPUSET"
    [ "$cpus" = "$VCPUS" ] || die "expected $VCPUS vCPUs under taskset -c $CPUSET, got $cpus"
}

# Never let out-bench or the oracle's temporary folder touch the input or the oracle.
guard_paths() {
    local out_abs data_abs oracle_abs
    out_abs=$(realpath -m -- "$BENCH_OUT")
    data_abs=$(realpath -m -- "$DATA")
    oracle_abs=$(realpath -m -- "$DATA.v1out")
    case "$data_abs/" in
        "$out_abs"/*) die "refusing: out-bench contains DATA ($DATA)" ;;
    esac
    case "$oracle_abs/" in
        "$out_abs"/*) die "refusing: out-bench contains the oracle ($DATA.v1out)" ;;
    esac
    case "$out_abs/" in
        "$data_abs"/*) die "refusing: DATA ($DATA) contains out-bench" ;;
    esac
}

oracle() {
    [ -d "$DATA" ] || die "DATA directory not found: $DATA"
    local final=$DATA.v1out tmp
    if [ -L "$final" ]; then
        die "refusing: $final is a symlink"
    fi
    if [ -e "$final" ]; then
        output_complete "$final" ||
            die "$final exists but is not complete candidate output; remove it and rerun make oracle"
        log "oracle: $final exists, skipping"
        return 0
    fi
    [ -x "$V1_BIN" ] || die "missing $V1_BIN (cargo build --release)"
    tmp=$(mktemp -d "${final}.tmp.XXXXXX") || die "mktemp failed next to $final"
    # shellcheck disable=SC2064 # expand tmp now: the local is gone when the trap runs
    trap "rm -rf -- $(printf %q "$tmp")" EXIT INT TERM
    log "oracle: SHOPT_ENGINE=v1 $V1_BIN $DATA -> $tmp"
    SHOPT_ENGINE=v1 DATA_DIR="$DATA" OUTPUT_DIR="$tmp" "$V1_BIN" "$DATA" "$tmp"
    output_complete "$tmp" || die "oracle run wrote no complete output in $tmp"
    if [ -e "$final" ] || [ -L "$final" ]; then
        die "$final appeared during the oracle run; not overwriting it"
    fi
    mv -- "$tmp" "$final" || die "could not rename $tmp to $final"
    trap - EXIT INT TERM
    log "oracle: wrote $final"
}

bench() {
    [ -d "$DATA" ] || die "DATA directory not found: $DATA"
    [[ $RUNS =~ ^[1-9][0-9]*$ ]] || die "RUNS must be a positive integer, got: $RUNS"
    [ -x "$PY" ] || die "PY=$PY is not executable (run make setup, or pass PY=...)"
    if [ -L "$BENCH_OUT" ]; then
        die "refusing: $BENCH_OUT is a symlink"
    fi
    resolve_cand
    require_pinned_cpus
    guard_paths

    log "bench: ${RUNS} run(s) of ${CAND_CMD[*]} on $DATA -> out-bench (NCPU=$VCPUS, taskset -c $CPUSET)"
    local rc=0
    BENCH_CPUSET=$CPUSET BENCH_VCPUS=$VCPUS "$TIMING_PY" - "$RUNS" "$BENCH_OUT" "$DATA" -- "${CAND_CMD[@]}" <<'PY' || rc=$?
import os
import resource
import shutil
import statistics
import subprocess
import sys
import time

TABLES = ("user_daily", "campaign_daily")


def say(message):
    print(f"bench: {message}", file=sys.stderr, flush=True)


def layout_problem(path):
    """Why PATH is not candidate output this script may delete, or None."""
    if os.path.islink(path):
        return "it is a symlink"
    try:
        entries = list(os.scandir(path))
    except OSError as exc:
        return f"cannot list it: {exc}"
    for entry in entries:
        try:
            if entry.is_symlink():
                return f"{entry.name!r} is a symlink"
            if entry.name in TABLES:
                if not entry.is_dir(follow_symlinks=False):
                    return f"{entry.name!r} is not a directory"
                for child in os.scandir(entry.path):
                    if child.is_symlink():
                        return f"{entry.name}/{child.name!r} is a symlink"
                    if not child.is_file(follow_symlinks=False):
                        return f"{entry.name}/{child.name!r} is not a regular file"
            elif entry.name.startswith((".", "_")) and entry.is_file(follow_symlinks=False):
                continue
            else:
                return f"unexpected entry {entry.name!r}"
        except OSError as exc:
            return f"cannot inspect {entry.name!r}: {exc}"
    return None


def has_table(out, table):
    try:
        entries = os.scandir(os.path.join(out, table))
    except OSError:
        return False
    with entries:
        for entry in entries:
            name = entry.name
            if name.startswith((".", "_")) or not name.endswith(".parquet"):
                continue
            if entry.is_file(follow_symlinks=False):
                return True
    return False


def reset(out):
    if not os.path.lexists(out):
        return True
    problem = layout_problem(out)
    if problem:
        say(f"refusing to delete {out}: {problem}")
        return False
    try:
        shutil.rmtree(out)
    except OSError as exc:
        say(f"could not delete {out}: {exc}")
        return False
    return True


def main():
    runs = int(sys.argv[1])
    out, data = sys.argv[2], sys.argv[3]
    cand = sys.argv[sys.argv.index("--") + 1:]
    if not cand:
        say("no candidate command")
        return 2

    prefix = ["taskset", "-c", os.environ["BENCH_CPUSET"]]
    env = dict(os.environ, DATA_DIR=data, OUTPUT_DIR=out,
               NCPU=os.environ.get("BENCH_VCPUS", "8"))

    walls = []
    for run in range(1, runs + 1):
        if not reset(out):
            return 2
        print(f"=== run {run}/{runs} ===", file=sys.stderr, flush=True)
        started = time.perf_counter()
        try:
            done = subprocess.run(prefix + cand + [data, out], env=env)
        except OSError as exc:
            say(f"cannot run {cand[0]}: {exc}")
            return 2
        wall = time.perf_counter() - started
        if done.returncode != 0:
            say(f"run {run} failed with exit {done.returncode}; not comparing a partial output")
            return 1
        missing = [table for table in TABLES if not has_table(out, table)]
        if missing:
            say(f"run {run} wrote no {' or '.join(missing)} output in {out}")
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
    if [ "$rc" -ne 0 ]; then
        log "bench: timing run failed (exit $rc); skipping comparison"
        return 1
    fi

    local oracle=$DATA.v1out cmp=0 compare_out
    if [ -L "$oracle" ]; then
        log "refusing: $oracle is a symlink"
        printf 'DIFFER\n'
        return 1
    fi
    if [ ! -e "$oracle" ]; then
        log "oracle missing: $oracle"
        log "run 'make oracle DATA=$DATA' first"
        printf 'DIFFER\n'
        return 1
    fi
    if ! output_complete "$oracle"; then
        log "oracle $oracle is incomplete; remove it and rerun 'make oracle DATA=$DATA'"
        printf 'DIFFER\n'
        return 1
    fi

    compare_out=$("$PY" scripts/compare.py "$BENCH_OUT" "$oracle" 2>&1) || cmp=$?
    printf '%s\n' "$compare_out"
    [ "$cmp" -eq 0 ] && return 0                      # compare.py printed MATCH
    grep -qx 'DIFFER' <<<"$compare_out" && return 1   # compare.py printed DIFFER
    log "comparison failed with exit $cmp (needs polars: make setup; check out-bench and $oracle)"
    printf 'DIFFER\n'
    return 1
}

case ${1:-} in
    oracle) oracle ;;
    bench) bench ;;
    *) usage ;;
esac
