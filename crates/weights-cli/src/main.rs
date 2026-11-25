//! `weights`: parallel, resumable, ranged transfers of model weight trees.
//!
//! ```text
//! weights get URL_OR_PATH DEST [--size N]
//! weights plan PLAN [--root DIR]        # lines: SOURCE [SIZE] DEST ("-" reads stdin)
//! weights copy PATH... --from ROOT --to DIR
//! ```
//!
//! Every file is cut into ranges shared by one pool of workers (`-j`), written
//! in place into `DEST.part` and renamed when complete; a rerun resumes the
//! missing ranges and skips finished files.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use weights::{Item, Options, Source, Stats};

#[derive(Parser)]
#[command(
    name = "weights",
    version,
    about = "Parallel ranged downloads and copies of model weight trees"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    #[command(flatten)]
    opts: Opts,
}

#[derive(Args)]
struct Opts {
    /// Concurrent ranges across all files.
    #[arg(short = 'j', long, default_value_t = 16, global = true)]
    jobs: usize,
    /// Range size in MiB.
    #[arg(long, default_value_t = 64, global = true)]
    chunk_mb: u64,
    /// Attempts per range.
    #[arg(long, default_value_t = 8, global = true)]
    retries: u32,
    /// Seconds between progress lines (0: none).
    #[arg(long, default_value_t = 10, global = true)]
    progress: u64,
    /// Seconds a connection may stall before its range is retried.
    #[arg(long, default_value_t = 20, global = true)]
    read_timeout: u64,
    /// Straggler watchdog: a GET running this many seconds far behind the
    /// recent ranges' pace is cut and its rest fetched again (0: off).
    #[arg(long, default_value_t = 4.0, global = true)]
    slow_after: f64,
}

#[derive(Subcommand)]
enum Cmd {
    /// One URL (or local file) to DEST.
    Get {
        src: String,
        dest: PathBuf,
        /// The size, when known (otherwise probed with a one-byte range GET).
        #[arg(long)]
        size: Option<u64>,
    },
    /// Every line of a plan: `SOURCE [SIZE] DEST`.
    Plan {
        plan: PathBuf,
        /// Directory that relative destinations are under.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Files or directories under ROOT to the same relative paths under DIR.
    Copy {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let opts = Options {
        jobs: cli.opts.jobs,
        chunk: cli.opts.chunk_mb.max(1) << 20,
        retries: cli.opts.retries,
        progress_s: cli.opts.progress,
        read_timeout_s: cli.opts.read_timeout,
        slow_after_s: cli.opts.slow_after,
    };
    let items: Vec<Item> = match cli.cmd {
        Cmd::Get { src, dest, size } => vec![Item {
            src: Source::parse(&src),
            dest,
            size,
        }],
        Cmd::Plan { plan, root } => weights::read_plan(&plan, root.as_deref())?,
        Cmd::Copy { paths, from, to } => weights::tree_items(&paths, &from, &to)?,
    };
    let s: Stats = weights::fetch(&items, &opts)?;
    eprintln!(
        "weights: {} files ({} already complete), {:.2} GB moved of {:.2} GB in {:.1} s ({:.2} GB/s){}",
        s.files,
        s.skipped,
        s.moved_bytes as f64 / 1e9,
        s.total_bytes as f64 / 1e9,
        s.seconds,
        s.rate_gbps(),
        if s.slow_restarts > 0 {
            format!(", {} slow GETs cut", s.slow_restarts)
        } else {
            String::new()
        }
    );
    Ok(())
}
