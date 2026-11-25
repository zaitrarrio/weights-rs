# CLAUDE.md: weights-rs

State and invariants only.

- One crate (`crates/weights`) and one CLI (`crates/weights-cli`, binary
  `weights`). Consumers: h3-rs, fastwan-rs, fastvideo-rs (git dependency;
  boot scripts download the static binary).
- Every transfer is ranges over one shared worker pool; ranges are written
  in place (`pwrite`) into `DEST.part`, recorded in `DEST.part.done`, and the
  file is renamed when complete. A destination already at its size is
  skipped. Keep a rerun's cost to the missing ranges.
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
- Tests: `cargo test` (a local tiny_http server with ranges, failures,
  truncation, no ranges; resume; local trees; plans).
