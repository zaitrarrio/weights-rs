//! Parallel, resumable transfers of model weight trees.
//!
//! Every transfer is cut into byte ranges ([`Options::chunk`], 64 MiB by
//! default) and the ranges of *all* files share one pool of worker threads
//! ([`Options::jobs`]), so one 10 GB shard is fetched over many connections at
//! once instead of one, and a tree of many small files keeps every worker
//! busy. Each range is written in place (`pwrite`) into `DEST.part`, which is
//! renamed to `DEST` once every range is in; finished ranges are recorded in
//! `DEST.part.done`, so an interrupted transfer resumes with only the missing
//! ranges. A destination that already exists at the expected size is skipped.
//!
//! Sources:
//! - HTTP(S) URLs, e.g. presigned R2 / S3 GETs or Hugging Face `resolve`
//!   URLs, fetched with `Range` requests. A presigned GET cannot be HEADed
//!   (the signature covers the method), so an unknown size is probed with a
//!   `Range: bytes=0-0` GET; a server that ignores ranges is read in one
//!   stream.
//! - Local paths (a network volume onto local disk), read with `pread`.
//!
//! [`fetch`] runs a list of [`Item`]s; [`plan_items`] parses the plain plan
//! format `SOURCE [SIZE] DEST` (one per line) and [`tree_items`] lists a
//! local directory tree.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::{HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where an item's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Url(String),
    Path(PathBuf),
}

impl Source {
    pub fn parse(s: &str) -> Source {
        if s.starts_with("http://") || s.starts_with("https://") {
            Source::Url(s.to_string())
        } else {
            Source::Path(PathBuf::from(s))
        }
    }
}

/// One file to transfer. `size` is optional: it is probed when absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub src: Source,
    pub dest: PathBuf,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Worker threads (concurrent ranges across all files).
    pub jobs: usize,
    /// Range size in bytes.
    pub chunk: u64,
    /// Attempts per range before the transfer fails.
    pub retries: u32,
    /// Seconds between progress lines on stderr (0: none).
    pub progress_s: u64,
    /// Seconds a connection may stall before the range is retried.
    pub read_timeout_s: u64,
    /// Straggler watchdog (0: off). A ranged GET that has run this long and
    /// at under 1 MB/s after 20 s, or over 3× the time the recent ranges took
    /// at under a third of their median rate, is cut and its unwritten rest
    /// requested again on a new connection (at most 4 cuts per range; cuts are
    /// not retries). R2 now and then serves one GET at 1-3 MB/s for minutes;
    /// fastvideo-rs's `r2-fetch.py` measured 0.16-0.42 GB/s without such a
    /// watchdog and 1.2-1.6 GB/s with it.
    pub slow_after_s: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jobs: 16,
            chunk: 64 << 20,
            retries: 8,
            progress_s: 10,
            read_timeout_s: 20,
            slow_after_s: 4.0,
        }
    }
}

/// What a [`fetch`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Files listed.
    pub files: usize,
    /// Files already complete at the destination (skipped).
    pub skipped: usize,
    /// Bytes of every listed file.
    pub total_bytes: u64,
    /// Bytes moved by this call (resumed ranges excluded).
    pub moved_bytes: u64,
    pub seconds: f64,
    /// GETs the straggler watchdog cut.
    pub slow_restarts: u64,
}

impl Stats {
    pub fn rate_gbps(&self) -> f64 {
        self.moved_bytes as f64 / 1e9 / self.seconds.max(1e-9)
    }
}

/// Parse a plan: one item per line, `SOURCE DEST` or `SOURCE SIZE DEST`;
/// blank lines and `#` comments are ignored. A relative DEST is joined to
/// `dest_root` when given. (`fetch_plan.py` in h3-rs prints this format.)
pub fn plan_items(text: &str, dest_root: Option<&Path>) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let (src, size, dest) = match f.as_slice() {
            [s, d] => (s, None, d),
            [s, z, d] => (
                s,
                Some(
                    z.parse::<u64>()
                        .with_context(|| format!("plan line {}: size `{z}`", n + 1))?,
                ),
                d,
            ),
            _ => bail!("plan line {}: expected `SOURCE [SIZE] DEST`", n + 1),
        };
        let dest = PathBuf::from(dest);
        let dest = match dest_root {
            Some(r) if dest.is_relative() => r.join(dest),
            _ => dest,
        };
        items.push(Item {
            src: Source::parse(src),
            dest,
            size,
        });
    }
    Ok(items)
}

/// Every file under each of `paths` (files or directories, which must lie
/// under `root`, symlinks followed), mapped to the same relative path under
/// `dest`. Paths outside `root`, `..` included, are refused.
pub fn tree_items(paths: &[PathBuf], root: &Path, dest: &Path) -> Result<Vec<Item>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("root {}", root.display()))?;
    let mut out = Vec::new();
    fn walk(p: &Path, root: &Path, dest: &Path, out: &mut Vec<Item>) -> Result<()> {
        let m = std::fs::metadata(p).with_context(|| format!("{}", p.display()))?;
        if m.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(p)?.collect::<std::io::Result<_>>()?;
            entries.sort_by_key(|e| e.path());
            for e in entries {
                walk(&e.path(), root, dest, out)?;
            }
        } else if m.is_file() {
            let rel = p.strip_prefix(root).unwrap_or(p);
            out.push(Item {
                src: Source::Path(p.to_path_buf()),
                dest: dest.join(rel),
                size: Some(m.len()),
            });
        }
        Ok(())
    }
    for p in paths {
        let abs = p
            .canonicalize()
            .with_context(|| format!("{}", p.display()))?;
        if !abs.starts_with(&root) {
            bail!("{}: not under {}", p.display(), root.display());
        }
        walk(&abs, &root, dest, &mut out)?;
    }
    Ok(out)
}

/// HTTP agents: through the environment's proxy (`HTTPS_PROXY` / `HTTP_PROXY`
/// / `ALL_PROXY`) and direct, picked per URL by `NO_PROXY` (ureq 2 reads the
/// proxy variables but not `NO_PROXY`).
#[derive(Clone)]
pub struct Http {
    proxied: ureq::Agent,
    direct: ureq::Agent,
    no_proxy: Vec<String>,
}

impl Http {
    pub fn from_env(read_timeout: Duration) -> Self {
        let base = || {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(30))
                .timeout_read(read_timeout)
        };
        let no_proxy = std::env::var("NO_PROXY")
            .or_else(|_| std::env::var("no_proxy"))
            .unwrap_or_default()
            .split(',')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        Self {
            proxied: base().try_proxy_from_env(true).build(),
            direct: base().build(),
            no_proxy,
        }
    }

    fn agent(&self, url: &str) -> &ureq::Agent {
        if bypass(&self.no_proxy, host_of(url)) {
            &self.direct
        } else {
            &self.proxied
        }
    }

    /// A GET with an optional `Range: bytes=a-b`; errors name the URL
    /// without its query (no presigned signatures in logs).
    pub fn get(&self, url: &str, range: Option<(u64, u64)>) -> Result<ureq::Response> {
        let mut req = self.agent(url).get(url);
        if let Some((a, b)) = range {
            req = req.set("Range", &format!("bytes={a}-{b}"));
        }
        req.call().map_err(|e| match e {
            ureq::Error::Status(code, _) => anyhow!("GET {}: status {code}", redact(url)),
            ureq::Error::Transport(t) => anyhow!("GET {}: {:?}", redact(url), t.kind()),
        })
    }
}

/// The host of an http(s) URL, lowercase, without port or brackets.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(v6) = auth.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

/// Whether `NO_PROXY` entries cover `host`: `*`, an exact name or address, a
/// domain suffix (`example.com` or `.example.com`), or an IP range
/// (`127.0.0.0/8`).
pub fn bypass(entries: &[String], host: String) -> bool {
    let ip: Option<std::net::IpAddr> = host.parse().ok();
    entries.iter().any(|e| {
        if e == "*" || *e == host {
            return true;
        }
        if let (Some(ip), Some((net, bits))) = (ip, e.split_once('/'))
            && let (Ok(net), Ok(bits)) = (net.parse::<std::net::IpAddr>(), bits.parse::<u32>())
        {
            return match (ip, net) {
                (std::net::IpAddr::V4(a), std::net::IpAddr::V4(n)) if bits <= 32 => {
                    let m = if bits == 0 {
                        0
                    } else {
                        u32::MAX << (32 - bits)
                    };
                    u32::from(a) & m == u32::from(n) & m
                }
                (std::net::IpAddr::V6(a), std::net::IpAddr::V6(n)) if bits <= 128 => {
                    let m = if bits == 0 {
                        0
                    } else {
                        u128::MAX << (128 - bits)
                    };
                    u128::from(a) & m == u128::from(n) & m
                }
                _ => false,
            };
        }
        let d = e.trim_start_matches('.');
        ip.is_none() && !d.is_empty() && host.ends_with(&format!(".{d}"))
    })
}

/// The size of `src`: the file's length, or for a URL the total of a
/// `Range: bytes=0-0` probe (`Content-Range: bytes 0-0/TOTAL`), else the
/// response's `Content-Length`.
/// `(size, ranges)`: `size` is `None` for a server that ignores ranges and
/// sends no `Content-Length` (the file is then read in one stream to its end).
pub fn probe_size(http: &Http, src: &Source) -> Result<(Option<u64>, bool)> {
    match src {
        Source::Path(p) => Ok((
            Some(
                std::fs::metadata(p)
                    .with_context(|| format!("{}", p.display()))?
                    .len(),
            ),
            true,
        )),
        Source::Url(u) => {
            let r = http.get(u, Some((0, 0)))?;
            if r.status() == 206 {
                let cr = r.header("Content-Range").unwrap_or("");
                let total = cr.rsplit('/').next().and_then(|t| t.parse().ok());
                return total
                    .map(|t| (Some(t), true))
                    .ok_or_else(|| anyhow!("probe {}: no total in `{cr}`", redact(u)));
            }
            Ok((
                r.header("Content-Length").and_then(|v| v.parse().ok()),
                false,
            ))
        }
    }
}

/// A probe's answer: the size, if the source told it, and whether it serves ranges.
type Probe = (Option<u64>, bool);

/// The size probes a plan needs, run `jobs` at a time: every item without a
/// size, and every URL of more than one range that is not already at its
/// destination (a server that ignores `Range` answers each range with the
/// whole body, so such a file must be read as one stream). `None`: no probe.
fn probe_all(http: &Http, items: &[Item], chunk: u64, jobs: usize) -> Result<Vec<Option<Probe>>> {
    let need: Vec<usize> = (0..items.len())
        .filter(|&i| {
            let it = &items[i];
            match it.size {
                None => true,
                Some(s) => {
                    matches!(it.src, Source::Url(_))
                        && s > chunk
                        && !std::fs::metadata(&it.dest).is_ok_and(|m| m.is_file() && m.len() == s)
                }
            }
        })
        .collect();
    let out: Vec<Mutex<Option<Probe>>> = (0..items.len()).map(|_| Mutex::new(None)).collect();
    let next = AtomicU64::new(0);
    let err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|sc| {
        for _ in 0..jobs.clamp(1, 32).min(need.len()) {
            sc.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::SeqCst) as usize;
                    let Some(&i) = need.get(k) else { return };
                    if err.lock().unwrap().is_some() {
                        return;
                    }
                    match probe_size(http, &items[i].src) {
                        Ok(p) => *out[i].lock().unwrap() = Some(p),
                        Err(e) => {
                            err.lock().unwrap().get_or_insert(e);
                            return;
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }
    Ok(out.into_iter().map(|m| m.into_inner().unwrap()).collect())
}

/// A URL without its query string (presigned signatures stay out of logs).
pub fn redact(u: &str) -> &str {
    u.split('?').next().unwrap_or(u)
}

struct FileState {
    item: Item,
    /// `u64::MAX`: unknown, read to the end of one stream.
    size: u64,
    ranges: bool,
    part: PathBuf,
    done_path: PathBuf,
    file: File,
    done_log: Mutex<File>,
    remaining: AtomicU64,
}

struct Job {
    file: usize,
    index: u64,
    offset: u64,
    len: u64,
}

fn part_paths(dest: &Path) -> (PathBuf, PathBuf) {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        dest.with_file_name(format!("{name}.part")),
        dest.with_file_name(format!("{name}.part.done")),
    )
}

/// Transfer every item; see the crate docs. Fails if any range fails after
/// its retries (finished ranges stay recorded for the next attempt).
pub fn fetch(items: &[Item], opts: &Options) -> Result<Stats> {
    let t0 = Instant::now();
    let agent = Http::from_env(Duration::from_secs(opts.read_timeout_s.max(1)));
    let chunk = opts.chunk.max(1);
    let mut stats = Stats {
        files: items.len(),
        ..Default::default()
    };
    let mut files: Vec<FileState> = Vec::new();
    let mut jobs: VecDeque<Job> = VecDeque::new();
    let mut todo_bytes = 0u64;
    let probes = probe_all(&agent, items, chunk, opts.jobs)?;
    for (item, probe) in items.iter().zip(probes) {
        let (size, ranges) = match (item.size, probe) {
            (Some(s), None) => (Some(s), true),
            (Some(s), Some((Some(p), _))) if p != s => bail!(
                "{}: the plan says {s} bytes, the source has {p}",
                item.dest.display()
            ),
            (Some(s), Some((_, r))) => (Some(s), r),
            (None, Some(p)) => p,
            (None, None) => unreachable!("probe_all probes every item without a size"),
        };
        stats.total_bytes += size.unwrap_or(0);
        let size = size.unwrap_or(u64::MAX);
        if size != u64::MAX
            && std::fs::metadata(&item.dest).is_ok_and(|m| m.is_file() && m.len() == size)
        {
            stats.skipped += 1;
            continue;
        }
        if let Some(d) = item.dest.parent() {
            std::fs::create_dir_all(d).with_context(|| format!("{}", d.display()))?;
        }
        let (part, done_path) = part_paths(&item.dest);
        let n_chunks = if ranges {
            size.div_ceil(chunk).max(1)
        } else {
            1
        };
        // Ranges recorded by an earlier attempt, if its .part is still the right size.
        let done: HashSet<u64> = match (
            std::fs::metadata(&part),
            std::fs::read_to_string(&done_path),
        ) {
            (Ok(m), Ok(t)) if m.len() == size && ranges => t
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .filter(|i| *i < n_chunks)
                .collect(),
            _ => HashSet::new(),
        };
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .with_context(|| format!("{}", part.display()))?;
        file.set_len(if size == u64::MAX { 0 } else { size })?;
        if done.is_empty() {
            let _ = std::fs::remove_file(&done_path);
        }
        let done_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&done_path)?;
        let idx = files.len();
        let mut missing = 0u64;
        for i in 0..n_chunks {
            if done.contains(&i) {
                continue;
            }
            let offset = i * chunk;
            let len = if ranges {
                chunk.min(size - offset)
            } else {
                size
            };
            jobs.push_back(Job {
                file: idx,
                index: i,
                offset,
                len,
            });
            todo_bytes += if len == u64::MAX { 0 } else { len };
            missing += 1;
        }
        files.push(FileState {
            item: item.clone(),
            size,
            ranges,
            part,
            done_path,
            file,
            done_log: Mutex::new(done_log),
            remaining: AtomicU64::new(missing),
        });
    }
    // Files whose every range was already recorded: finish them now.
    for f in &files {
        if f.remaining.load(Ordering::SeqCst) == 0 {
            finish(f)?;
        }
    }
    let files = Arc::new(files);
    let queue = Arc::new(Mutex::new(jobs));
    let moved = Arc::new(AtomicU64::new(0));
    let pace = Arc::new(Pace::new(opts.slow_after_s));
    let failed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let reporter = (opts.progress_s > 0 && todo_bytes > 0).then(|| {
        let (moved, stop, every) = (moved.clone(), stop.clone(), opts.progress_s);
        std::thread::spawn(move || {
            let mut last = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
                if last.elapsed().as_secs() >= every {
                    last = Instant::now();
                    let m = moved.load(Ordering::SeqCst);
                    eprintln!(
                        "weights: {:.1} / {:.1} GB, {:.2} GB/s",
                        m as f64 / 1e9,
                        todo_bytes as f64 / 1e9,
                        m as f64 / 1e9 / t0.elapsed().as_secs_f64()
                    );
                }
            }
        })
    });
    let workers: Vec<_> = (0..opts.jobs.max(1))
        .map(|_| {
            let (files, queue, moved, failed, agent, pace) = (
                files.clone(),
                queue.clone(),
                moved.clone(),
                failed.clone(),
                agent.clone(),
                pace.clone(),
            );
            let retries = opts.retries.max(1);
            std::thread::spawn(move || {
                loop {
                    if failed.lock().unwrap().is_some() {
                        return;
                    }
                    let Some(job) = queue.lock().unwrap().pop_front() else {
                        return;
                    };
                    let f = &files[job.file];
                    let mut attempt = 0;
                    let r = loop {
                        attempt += 1;
                        match transfer(&agent, f, &job, &moved, &pace) {
                            Ok(()) => break Ok(()),
                            Err(e) if attempt < retries => {
                                eprintln!(
                                    "weights: {} range {} attempt {attempt}: {e:#}",
                                    f.item.dest.display(),
                                    job.index
                                );
                                std::thread::sleep(Duration::from_millis(250 << attempt.min(6)));
                            }
                            Err(e) => break Err(e),
                        }
                    };
                    let r = r.and_then(|()| {
                        writeln!(f.done_log.lock().unwrap(), "{}", job.index)?;
                        if f.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                            finish(f)?;
                        }
                        Ok(())
                    });
                    if let Err(e) = r {
                        failed
                            .lock()
                            .unwrap()
                            .get_or_insert(format!("{}: {e:#}", f.item.dest.display()));
                        return;
                    }
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    stop.store(true, Ordering::SeqCst);
    if let Some(r) = reporter {
        let _ = r.join();
    }
    if let Some(e) = failed.lock().unwrap().take() {
        bail!("transfer failed: {e}");
    }
    stats.moved_bytes = moved.load(Ordering::SeqCst);
    stats.slow_restarts = pace.cuts.load(Ordering::SeqCst);
    stats.seconds = t0.elapsed().as_secs_f64();
    Ok(stats)
}

/// Recent ranged GETs' rates, for the straggler watchdog.
struct Pace {
    slow_after: f64,
    rates: Mutex<VecDeque<f64>>,
    cuts: AtomicU64,
}

impl Pace {
    fn new(slow_after: f64) -> Self {
        Self {
            slow_after,
            rates: Mutex::new(VecDeque::new()),
            cuts: AtomicU64::new(0),
        }
    }

    /// A finished GET of `bytes` in `secs`.
    fn record(&self, bytes: u64, secs: f64) {
        if bytes < 1 << 16 || secs <= 0.0 {
            return;
        }
        let mut r = self.rates.lock().unwrap();
        r.push_back(bytes as f64 / secs);
        if r.len() > 64 {
            r.pop_front();
        }
    }

    /// The median of the recent rates, once there are 8.
    fn typical(&self) -> Option<f64> {
        let mut r: Vec<f64> = self.rates.lock().unwrap().iter().copied().collect();
        if r.len() < 8 {
            return None;
        }
        r.sort_by(f64::total_cmp);
        Some(r[r.len() / 2])
    }

    /// Why a GET of `size` bytes that moved `n` in `el` seconds should be cut.
    fn verdict(&self, el: f64, n: u64, size: u64) -> Option<String> {
        if self.slow_after <= 0.0 || el < self.slow_after {
            return None;
        }
        let rate = n as f64 / el;
        let typical = self.typical();
        let slow = (el >= 20.0 && rate < 1e6)
            || typical.is_some_and(|t| el > 3.0 * size as f64 / t && rate < t / 3.0);
        slow.then(|| {
            format!("{:.2} MB/s after {el:.1} s", rate / 1e6)
                + &typical
                    .map(|t| format!(" (recent ranges {:.0} MB/s)", t / 1e6))
                    .unwrap_or_default()
        })
    }
}

/// How a read loop ended.
enum Outcome {
    Done,
    Slow(String),
}

/// One range of one file, written at its offset; on a short read the error
/// leaves the range unrecorded, so a retry or a later run fetches it again.
fn transfer(agent: &Http, f: &FileState, job: &Job, moved: &AtomicU64, pace: &Pace) -> Result<()> {
    let mut written = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    // Reads until the range is complete; `check(bytes this call, seconds)`
    // may stop it early.
    let mut put = |r: &mut dyn Read,
                   written: &mut u64,
                   check: &mut dyn FnMut(u64, f64) -> Option<String>|
     -> Result<Outcome> {
        let (t, from) = (Instant::now(), *written);
        let mut last_check = 0.0f64;
        while *written < job.len {
            let want = (job.len - *written).min(buf.len() as u64) as usize;
            let n = r.read(&mut buf[..want])?;
            if n == 0 {
                if job.len == u64::MAX {
                    return Ok(Outcome::Done);
                }
                bail!("short read: {} of {} bytes", *written, job.len);
            }
            f.file.write_all_at(&buf[..n], job.offset + *written)?;
            *written += n as u64;
            moved.fetch_add(n as u64, Ordering::Relaxed);
            let el = t.elapsed().as_secs_f64();
            if el - last_check >= 0.25 {
                last_check = el;
                if let Some(why) = check(*written - from, el) {
                    return Ok(Outcome::Slow(why));
                }
            }
        }
        Ok(Outcome::Done)
    };
    let r = match &f.item.src {
        Source::Path(p) => {
            let src = File::open(p).with_context(|| format!("{}", p.display()))?;
            put(
                &mut ReadAt {
                    f: &src,
                    pos: job.offset,
                },
                &mut written,
                &mut |_, _| None,
            )
            .map(|_| ())
        }
        Source::Url(u) => {
            let ranged = f.ranges && f.size > 0 && f.size != u64::MAX;
            let mut cuts = 0u32;
            loop {
                let (start, rest) = (job.offset + written, job.len - written);
                let range = ranged.then(|| (start, job.offset + job.len - 1));
                let t = Instant::now();
                let resp = match agent.get(u, range) {
                    Ok(r) => r,
                    Err(e) => break Err(e),
                };
                if f.ranges && f.size > 0 && resp.status() != 206 && !(start == 0 && rest == f.size)
                {
                    break Err(anyhow::anyhow!(
                        "GET {}: status {} for a range",
                        redact(u),
                        resp.status()
                    ));
                }
                let watch = ranged && cuts < 4;
                let mut check = |n: u64, el: f64| {
                    if watch {
                        pace.verdict(el, n, rest)
                    } else {
                        None
                    }
                };
                match put(&mut resp.into_reader(), &mut written, &mut check) {
                    Ok(Outcome::Done) => {
                        pace.record(rest, t.elapsed().as_secs_f64());
                        break Ok(());
                    }
                    Ok(Outcome::Slow(why)) => {
                        // Dropping the reader closes the connection; the rest
                        // of the range goes out on a fresh one.
                        cuts += 1;
                        pace.cuts.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "weights: {} range {}: cut a slow GET ({why}), {} of {} bytes in; \
                             fetching the rest again",
                            f.item.dest.display(),
                            job.index,
                            written,
                            job.len
                        );
                    }
                    Err(e) => break Err(e),
                }
            }
        }
    };
    if r.is_err() {
        // Count only what lands in a recorded range.
        moved.fetch_sub(written, Ordering::Relaxed);
    }
    r
}

struct ReadAt<'a> {
    f: &'a File,
    pos: u64,
}

impl Read for ReadAt<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.f.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

fn finish(f: &FileState) -> Result<()> {
    f.file.sync_all()?;
    std::fs::rename(&f.part, &f.item.dest).with_context(|| format!("{}", f.item.dest.display()))?;
    let _ = std::fs::remove_file(&f.done_path);
    Ok(())
}

/// Read a plan file (or `-` for stdin).
pub fn read_plan(path: &Path, dest_root: Option<&Path>) -> Result<Vec<Item>> {
    let mut text = String::new();
    if path.as_os_str() == "-" {
        BufReader::new(std::io::stdin()).read_to_string(&mut text)?;
    } else {
        File::open(path)
            .with_context(|| format!("{}", path.display()))?
            .read_to_string(&mut text)?;
    }
    plan_items(&text, dest_root)
}
