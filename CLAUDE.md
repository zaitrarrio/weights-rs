# CLAUDE.md: weights-rs

State and invariants only.

- One crate (`crates/weights`) and one CLI (`crates/weights-cli`, binary
  `weights`). Consumers: h3-rs and fastwan-rs (their Vast boots download
  the static binary from the run's R2 folder). fastvideo-rs keeps its own
  `scripts/gpu/r2-fetch.py` (per-part SHA-256 against a pinned manifest,
  serving while the weights arrive), which this crate does not replace.
- Every transfer is ranges over one shared worker pool; ranges are written
  in place (`pwrite`) into `DEST.part`, recorded in `DEST.part.done`, and the
  file is renamed when complete. A destination already at its size is
  skipped. Keep a rerun's cost to the missing ranges.
- Straggler watchdog (`Pace`, `--slow-after`): a slow GET is cut and the
  unwritten rest of its range is requested again, at most 4 cuts per range.
  Cuts are not retries. The rule is fastvideo-rs's `r2-fetch.py` one
  (`docs/ops/r2-weights.md` there: 0.16-0.42 → 1.2-1.6 GB/s on R2).
  `a_trickling_get_is_cut_and_its_rest_fetched_again` holds it.
- Presigned GETs cannot be HEADed: sizes come from a `Range: bytes=0-0` GET
  (`Content-Range` total). A server that ignores ranges is read in one
  stream; with no `Content-Length`, to its end.
- Never log a URL's query string (`redact`, `Http::get`'s errors): presigned
  signatures are credentials. `errors_never_carry_a_presigned_signature`
  holds it.
- ureq 2 reads proxy variables but not `NO_PROXY`: `Http` keeps a direct
  agent for hosts `NO_PROXY` matches (names, domain suffixes, CIDRs).
- No speed claim without a measurement on named hardware and network, in
  `docs/runs/`.
- Measured (`docs/runs/2026-11-25-vast-r2-curl-vs-weights`, R2 → Vast, 55 GB
  incl. one 26 GB file): `-j 32` filled a 5.4 Gb/s Japan host (0.72 GB/s vs
  curl-per-file's 0.19-0.24, 2.8-3.7×); a Utah host capped every method at
  ~0.2 GB/s. `-j 64` was slower than 32 (the watchdog cut 339 GETs on a full
  link). Byte-identical to curl (SHA-256, 18 files).
- Tests: `cargo test` (a local tiny_http server with ranges, failures,
  truncation, no ranges; resume; local trees; plans).
