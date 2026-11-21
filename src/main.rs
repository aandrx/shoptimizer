//! Binary entry point: CLI/env paths, phase timing, and output writing.
//!
//! T01 writes the two tables with zero rows and the exact schema. T05/T11 fill
//! in the pipeline behind the same interface.

// Stubs filled in by later tasks (T02-T11); unused until then.
#[allow(dead_code)]
mod csv;
mod output;
#[allow(dead_code)]
mod people;
#[allow(dead_code)]
mod ts;
#[allow(dead_code)]
mod v1;
#[allow(dead_code)]
mod v2;

use std::env;
use std::io;
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
    let (_data, out) = match paths() {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("{message}");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
        }
    };
    // T01: no pipeline yet; emit both tables with the exact schema and zero rows.
    let started = Instant::now();
    output::write_user_daily(&out.join("user_daily"), &[])?;
    phase("write_user_daily", started);

    let started = Instant::now();
    output::write_campaign_daily(&out.join("campaign_daily"), &[])?;
    phase("write_campaign_daily", started);

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
