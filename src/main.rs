#![allow(dead_code)]

mod agg;
mod csv;
mod output;
mod people;
mod ts;
mod v1;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_or = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (data, out) = if args.len() >= 2 {
        (PathBuf::from(&args[args.len() - 2]), PathBuf::from(&args[args.len() - 1]))
    } else {
        (PathBuf::from(env_or("DATA_DIR", "/data")), PathBuf::from(env_or("OUTPUT_DIR", "/output")))
    };
    let t = Instant::now();
    eprintln!("[start] data={} out={}", data.display(), out.display());
    // T05: call v1::run(&data, &out) instead of writing empty tables.
    let r = output::write_user_daily(&out.join("user_daily"), &[])
        .and_then(|_| output::write_campaign_daily(&out.join("campaign_daily"), &[]));
    match r {
        Ok(()) => {
            eprintln!("[phase] write {} ms", t.elapsed().as_millis());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
