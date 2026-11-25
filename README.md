# weights-rs

Parallel, resumable, ranged transfers of model weight trees: R2 / S3
presigned URLs, Hugging Face `resolve` URLs, and local copies (a network
volume onto local disk). Shared by h3-rs, fastwan-rs and fastvideo-rs.

Every file is cut into ranges (64 MiB by default) and the ranges of *all*
files share one pool of workers, so a single 10 GB shard comes down over many
connections at once instead of one, and a tree of many small files keeps
every worker busy. Ranges are written in place into `DEST.part`, which is
renamed when complete; finished ranges are recorded in `DEST.part.done`, so a
rerun fetches only what is missing, and a file already at its size is skipped.

## CLI

```bash
cargo install --git https://github.com/zaitrarrio/weights-rs weights-cli
```

```bash
weights get URL_OR_PATH DEST [--size N]
weights plan PLAN [--root DIR]          # lines: SOURCE [SIZE] DEST; "-" reads stdin
weights copy PATH... --from ROOT --to DIR
```

Options for every subcommand:
- `-j` concurrent ranges (16);
- `--chunk-mb` (64);
- `--retries` per range (8);
- `--read-timeout` seconds before a stalled range is retried (20);
- `--slow-after` seconds before the straggler watchdog may cut a GET (4; 0
  turns it off). A GET that has run at under 1 MB/s for 20 s, or longer than
  3× the recent ranges' time at under a third of their median rate, is cut.
  The part of its range not yet written is requested again on a new
  connection. R2 now and then serves one GET at 1-3 MB/s for minutes.
- `--progress` seconds between progress lines (10).

On a fresh GPU box (no Rust toolchain), boot scripts download the prebuilt
static binary instead of building it.

## Library

```rust
let items = weights::read_plan(Path::new("plan.txt"), Some(Path::new("/w")))?;
let stats = weights::fetch(&items, &weights::Options::default())?;
```

- **Plans:** `weights::tree_items` lists a local tree under a root (paths
  outside it are refused), and `weights::plan_items` parses the plan format.
- **Presigned URLs:** probed with a one-byte range GET, since their signature
  does not allow a HEAD.
- **Errors:** never contain a URL's query string, so signatures stay out of
  logs.
- **Proxies:** HTTP(S) goes through the environment's proxy unless
  `NO_PROXY` matches the host.
