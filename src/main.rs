//! Binary entry point: CLI/env paths, engine choice (`SHOPT_ENGINE=v1|v2`,
//! default v2), and phase timing.
//!
//! Logs go to stderr only; stdout stays empty.

mod agg;
mod csv;
mod id;
mod output;
mod people;
mod ts;
mod v1;
mod v2;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

/// `SHOPT_IO_STATS=1` phase instrumentation (PLAN.md §6 S3-7). Off by default:
/// the disabled path does no `/proc` I/O and reads the env var only once.
///
/// `RssAnon`/`RssFile` are point samples taken at each phase boundary, from one
/// read of `/proc/self/status`, so they are coherent with each other but miss
/// transient peaks inside a phase. `VmHWM` is the kernel-tracked peak of the
/// whole resident set (anon plus file-backed pages), printed once at the end.
/// It is *not* the anon cap metric: a sampled `RssAnon` is only a lower bound on
/// the anon peak, and `VmHWM` can be larger than peak anon because it also
/// counts mapped file pages. Quote `VmHWM` for total peak memory, not for anon.
pub(crate) mod iostats {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::Instant;

    fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("SHOPT_IO_STATS").as_deref() == Ok("1"))
    }

    /// `(minflt, majflt)` from `/proc/self/stat` fields 10 and 12 (1-based).
    /// Counting starts after the last `)` of `comm`, so spaces or parentheses in
    /// the command name cannot shift the fields: `state` is then index 0,
    /// `minflt` index 7 and `majflt` index 9.
    fn parse_faults(stat: &[u8]) -> Option<(u64, u64)> {
        let close = stat.iter().rposition(|&b| b == b')')?;
        let mut minflt = None;
        let mut majflt = None;
        for (index, field) in stat[close + 1..]
            .split(|b| b.is_ascii_whitespace())
            .filter(|f| !f.is_empty())
            .enumerate()
        {
            match index {
                7 => minflt = parse_u64(field),
                9 => {
                    majflt = parse_u64(field);
                    break;
                }
                _ => {}
            }
        }
        Some((minflt?, majflt?))
    }

    fn parse_u64(bytes: &[u8]) -> Option<u64> {
        std::str::from_utf8(bytes).ok()?.parse().ok()
    }

    /// Value in kB of a `/proc/self/status` line such as `RssAnon:\t  1234 kB`.
    fn parse_status_kb(status: &[u8], key: &[u8]) -> Option<u64> {
        for line in status.split(|&b| b == b'\n') {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let start = rest.iter().position(|b| b.is_ascii_digit())?;
            let end = rest[start..]
                .iter()
                .position(|b| !b.is_ascii_digit())
                .map_or(rest.len(), |p| start + p);
            return parse_u64(&rest[start..end]);
        }
        None
    }

    fn faults() -> Option<(u64, u64)> {
        parse_faults(&std::fs::read("/proc/self/stat").ok()?)
    }

    fn status() -> Option<Vec<u8>> {
        std::fs::read("/proc/self/status").ok()
    }

    /// One read of `/proc/self/status`, so the two samples describe one moment.
    fn rss() -> (Option<u64>, Option<u64>) {
        match status() {
            Some(status) => (
                parse_status_kb(&status, b"RssAnon:"),
                parse_status_kb(&status, b"RssFile:"),
            ),
            None => (None, None),
        }
    }

    // minflt, majflt of the previous phase line, for per-phase deltas.
    static LAST: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    static HAVE_LAST: AtomicBool = AtomicBool::new(false);

    /// `/proc/vmstat` counters sampled per phase, in report order.
    const VM_KEYS: [&[u8]; 7] = [
        b"pgfault",
        b"thp_fault_alloc",
        b"thp_fault_fallback",
        b"compact_stall",
        b"pgscan_direct",
        b"pswpin",
        b"pswpout",
    ];

    // Previous values of the VM_KEYS counters, for per-phase deltas. The baseline
    // is taken by `init` before the pipeline starts, because these counters are
    // system-wide rather than per-process.
    static LAST_VM: [AtomicU64; 7] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static HAVE_VM: [AtomicBool; 7] = [
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
    ];
    static INITED: AtomicBool = AtomicBool::new(false);

    /// Previous getrusage CPU times, for per-phase deltas; the first line is
    /// measured from process start, as for the fault counters.
    static LAST_CPU: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    static HAVE_CPU: AtomicBool = AtomicBool::new(false);

    /// A `key value` counter line from `/proc/vmstat`.
    fn parse_vmstat(bytes: &[u8], key: &[u8]) -> Option<u64> {
        for line in bytes.split(|&b| b == b'\n') {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            // Require whitespace after the key so `pgfault` can't match a longer
            // name that starts with it.
            if !rest.first().is_some_and(u8::is_ascii_whitespace) {
                continue;
            }
            let start = rest.iter().position(u8::is_ascii_digit)?;
            let end = rest[start..]
                .iter()
                .position(|b| !b.is_ascii_digit())
                .map_or(rest.len(), |p| start + p);
            return parse_u64(&rest[start..end]);
        }
        None
    }

    /// All [`VM_KEYS`] counters from one read of `/proc/vmstat`.
    fn vmstat() -> [Option<u64>; VM_KEYS.len()] {
        let Ok(bytes) = std::fs::read("/proc/vmstat") else {
            return [None; VM_KEYS.len()];
        };
        let mut values = [None; VM_KEYS.len()];
        for (slot, key) in values.iter_mut().zip(VM_KEYS) {
            *slot = parse_vmstat(&bytes, key);
        }
        values
    }

    /// `(user, system)` CPU microseconds consumed by the whole process.
    fn cpu_times() -> Option<(u64, u64)> {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        let micros = |tv: &libc::timeval| {
            tv.tv_sec.max(0) as u64 * 1_000_000 + tv.tv_usec.max(0) as u64
        };
        Some((micros(&usage.ru_utime), micros(&usage.ru_stime)))
    }

    /// `AnonHugePages` in kB from one read of `/proc/self/smaps_rollup`.
    fn anon_hugepages() -> Option<u64> {
        std::fs::read("/proc/self/smaps_rollup")
            .ok()
            .and_then(|bytes| parse_status_kb(&bytes, b"AnonHugePages:"))
    }

    /// Takes the system-wide vmstat baseline before the pipeline starts. No-op
    /// when the instrumentation is disabled, so default output stays unchanged.
    pub fn init() {
        if !enabled() || INITED.swap(true, Ordering::Relaxed) {
            return;
        }
        for (index, value) in vmstat().into_iter().enumerate() {
            if let Some(value) = value {
                LAST_VM[index].store(value, Ordering::Relaxed);
                HAVE_VM[index].store(true, Ordering::Relaxed);
            }
        }
    }

    /// Per-counter delta, or `None` when the counter could not be read.
    fn delta(current: Option<u64>, last: &AtomicU64, have: &AtomicBool) -> Option<u64> {
        let current = current?;
        let previous = last.swap(current, Ordering::Relaxed);
        Some(if have.swap(true, Ordering::Relaxed) {
            current.saturating_sub(previous)
        } else {
            current
        })
    }

    fn plus(value: Option<u64>) -> String {
        value.map_or_else(|| "?".to_string(), |v| format!("+{v}"))
    }

    fn kb(value: Option<u64>) -> String {
        value.map_or_else(|| "?".to_string(), |v| format!("{v}kB"))
    }

    pub fn log_phase(name: &str, started: Instant) {
        if !enabled() {
            eprintln!("[phase] {name} {}ms", started.elapsed().as_millis());
            return;
        }
        // Normally already done by `run`; this only matters for a phase line
        // logged before that baseline, so its own I/O is not in the phase time.
        init();
        let ms = started.elapsed().as_millis();
        let (minflt, majflt) = faults().unwrap_or((0, 0));
        let prev_min = LAST[0].swap(minflt, Ordering::Relaxed);
        let prev_maj = LAST[1].swap(majflt, Ordering::Relaxed);
        // The first line's delta is measured from process start.
        let have_prev = HAVE_LAST.swap(true, Ordering::Relaxed);
        let (d_min, d_maj) = if have_prev {
            (minflt.saturating_sub(prev_min), majflt.saturating_sub(prev_maj))
        } else {
            (minflt, majflt)
        };
        let (d_user, d_sys) = match cpu_times() {
            Some((user, sys)) => {
                let have_cpu = HAVE_CPU.swap(true, Ordering::Relaxed);
                let prev_user = LAST_CPU[0].swap(user, Ordering::Relaxed);
                let prev_sys = LAST_CPU[1].swap(sys, Ordering::Relaxed);
                if have_cpu {
                    (
                        Some(user.saturating_sub(prev_user)),
                        Some(sys.saturating_sub(prev_sys)),
                    )
                } else {
                    (Some(user), Some(sys))
                }
            }
            None => (None, None),
        };
        let current_vm = vmstat();
        let mut d_vm = [None; VM_KEYS.len()];
        for index in 0..VM_KEYS.len() {
            d_vm[index] = delta(current_vm[index], &LAST_VM[index], &HAVE_VM[index]);
        }
        let (anon, file) = rss();
        eprintln!(
            "[phase] {name} {ms}ms minflt=+{d_min} majflt=+{d_maj} \
             RssAnon={}kB RssFile={}kB user={}us sys={}us \
             pgfault={} thp_fault_alloc={} thp_fault_fallback={} \
             compact_stall={} pgscan_direct={} pswpin={} pswpout={} \
             AnonHugePages={}",
            anon.map_or_else(|| "?".to_string(), |v| v.to_string()),
            file.map_or_else(|| "?".to_string(), |v| v.to_string()),
            plus(d_user),
            plus(d_sys),
            plus(d_vm[0]),
            plus(d_vm[1]),
            plus(d_vm[2]),
            plus(d_vm[3]),
            plus(d_vm[4]),
            plus(d_vm[5]),
            plus(d_vm[6]),
            kb(anon_hugepages()),
        );
    }

    /// Wall-clock milliseconds since the process started on the timed path, for
    /// subtracting the exit teardown from the bench wall. No-op unless enabled.
    pub fn log_exit(started: Instant) {
        if enabled() {
            eprintln!("[exit] t={}ms", started.elapsed().as_millis());
        }
    }

    /// Kernel-tracked peak resident set (whole RSS, not the anon cap) at exit.
    pub fn log_peak() {
        if !enabled() {
            return;
        }
        let hwm = status().and_then(|status| parse_status_kb(&status, b"VmHWM:"));
        match hwm {
            Some(kb) => eprintln!("[stats] VmHWM={kb}kB"),
            None => eprintln!("[stats] VmHWM=?"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const STAT: &[u8] = b"1234 (shopt (x) y) S 1 2345 2345 0 -1 4194304 123 45 67 89 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

        #[test]
        fn faults_skip_comm_with_spaces_and_parens() {
            assert_eq!(parse_faults(STAT), Some((123, 67)));
        }

        #[test]
        fn faults_reject_truncated_stat() {
            assert_eq!(parse_faults(b"1234 (x) S 1 2 3 4 5 6 7 8"), None);
            assert_eq!(parse_faults(b""), None);
        }

        #[test]
        fn faults_real_proc_stat() {
            let stat = std::fs::read("/proc/self/stat").expect("/proc/self/stat");
            let (minflt, majflt) = parse_faults(&stat).expect("parse /proc/self/stat");
            assert!(minflt > 0);
            let _ = majflt;
        }

        #[test]
        fn status_fields() {
            let status = b"Name:\tshoptimizer\nRssAnon:\t  1234 kB\nRssFile:\t  567 kB\nVmHWM:\t 2345 kB\n";
            assert_eq!(parse_status_kb(status, b"RssAnon:"), Some(1234));
            assert_eq!(parse_status_kb(status, b"RssFile:"), Some(567));
            assert_eq!(parse_status_kb(status, b"VmHWM:"), Some(2345));
            assert_eq!(parse_status_kb(status, b"RssShmem:"), None);
        }

        #[test]
        fn vmstat_fields() {
            const VMSTAT: &[u8] = b"nr_free_pages 123\npgfault 456 789\nthp_fault_alloc 7\nthp_fault_fallback 8\ncompact_stall 9\npgscan_direct 10\npswpin 11\npswpout 12\npgfault_extra 99\n";
            assert_eq!(parse_vmstat(VMSTAT, b"pgfault"), Some(456));
            assert_eq!(parse_vmstat(VMSTAT, b"thp_fault_alloc"), Some(7));
            assert_eq!(parse_vmstat(VMSTAT, b"thp_fault_fallback"), Some(8));
            assert_eq!(parse_vmstat(VMSTAT, b"compact_stall"), Some(9));
            assert_eq!(parse_vmstat(VMSTAT, b"pgscan_direct"), Some(10));
            assert_eq!(parse_vmstat(VMSTAT, b"pswpin"), Some(11));
            assert_eq!(parse_vmstat(VMSTAT, b"pswpout"), Some(12));
            assert_eq!(parse_vmstat(VMSTAT, b"pgfault_extra"), Some(99));
            assert_eq!(parse_vmstat(VMSTAT, b"missing"), None);
            assert_eq!(parse_vmstat(b"pgfault\n", b"pgfault"), None);
        }

        #[test]
        fn vmstat_real_proc_vmstat() {
            let vmstat = std::fs::read("/proc/vmstat").expect("/proc/vmstat");
            assert!(parse_vmstat(&vmstat, b"pgfault").is_some());
            let _ = parse_vmstat(&vmstat, b"thp_fault_alloc");
            let _ = parse_vmstat(&vmstat, b"compact_stall");
            let _ = parse_vmstat(&vmstat, b"pswpin");
        }

        #[test]
        fn cpu_times_real_getrusage() {
            let (user, sys) = cpu_times().expect("getrusage(RUSAGE_SELF)");
            let _ = (user, sys);
        }

        #[test]
        fn status_real_proc_status() {
            let status = std::fs::read("/proc/self/status").expect("/proc/self/status");
            assert!(parse_status_kb(&status, b"VmHWM:").is_some());
            assert!(parse_status_kb(&status, b"RssFile:").is_some());
            // RssAnon only exists on 4.5+ kernels; skip the assert if absent.
            let _ = parse_status_kb(&status, b"RssAnon:");
        }
    }
}

fn usage() -> &'static str {
    "usage: shoptimizer [DATA_DIR OUTPUT_DIR]\n\
     (or set DATA_DIR and OUTPUT_DIR)"
}

/// Resolves `[DATA_DIR OUTPUT_DIR]` from arguments, falling back to env vars.
fn paths() -> Result<(PathBuf, PathBuf), String> {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    let second = args.next();
    if args.next().is_some() {
        return Err(usage().to_string());
    }
    match (first, second) {
        (Some(data), Some(out)) => Ok((PathBuf::from(data), PathBuf::from(out))),
        (None, None) => {
            let data = env::var_os("DATA_DIR")
                .ok_or_else(|| format!("DATA_DIR is not set\n{}", usage()))?;
            let out = env::var_os("OUTPUT_DIR")
                .ok_or_else(|| format!("OUTPUT_DIR is not set\n{}", usage()))?;
            Ok((PathBuf::from(data), PathBuf::from(out)))
        }
        _ => Err(usage().to_string()),
    }
}

fn phase(name: &str, started: Instant) {
    iostats::log_phase(name, started);
}

fn run() -> io::Result<()> {
    // System-wide vmstat baseline, before any pipeline work; no-op by default.
    iostats::init();
    let (data, out) = match paths() {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("{message}");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
        }
    };
    let started = Instant::now();
    if env::var("SHOPT_ENGINE").as_deref() == Ok("v1") {
        v1::run(&data, &out)?;
        phase("v1", started);
    } else {
        let threads = env::var("NCPU")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(8, usize::from));
        let _ = rayon::ThreadPoolBuilder::new().num_threads(threads).build_global();
        // S3-3: exit from this completion hook, while V2 still owns `acc`, the
        // mmaps and the lookup maps. Returning to `main` would drop all of them
        // first, paying that teardown on the critical path. The hook runs after
        // both Parquet writers returned (files closed), and stderr is flushed
        // before the process exits so no log line is lost.
        v2::run_with(&data, &out, || {
            phase("total", started);
            iostats::log_peak();
            iostats::log_exit(started);
            let _ = io::stderr().flush();
            std::process::exit(0);
        })?;
        // Only the V1 fallback (and any caller whose hook returns) gets here; the
        // success hook above never returns, so this line is not a duplicate.
        phase("total", started);
    }
    // V1 and the V1 fallback reach this; successful V2 exits from the hook above.
    iostats::log_peak();
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
