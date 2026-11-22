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

    pub fn log_phase(name: &str, started: Instant) {
        if !enabled() {
            eprintln!("[phase] {name} {}ms", started.elapsed().as_millis());
            return;
        }
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
        let (anon, file) = rss();
        eprintln!(
            "[phase] {name} {ms}ms minflt=+{d_min} majflt=+{d_maj} \
             RssAnon={}kB RssFile={}kB",
            anon.map_or_else(|| "?".to_string(), |v| v.to_string()),
            file.map_or_else(|| "?".to_string(), |v| v.to_string()),
        );
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
