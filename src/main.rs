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
    eprintln!("[phase] {name} {}ms", started.elapsed().as_millis());
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
            let _ = io::stderr().flush();
            std::process::exit(0);
        })?;
        // Only the V1 fallback (and any caller whose hook returns) gets here; the
        // success hook above never returns, so this line is not a duplicate.
        phase("total", started);
    }
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
