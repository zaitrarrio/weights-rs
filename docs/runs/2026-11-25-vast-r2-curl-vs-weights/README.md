# 2026-11-25: R2 to a Vast box, one curl per file vs the `weights` CLI

Two Vast boxes, each billed about $0.11/h with free downloads. Each
downloaded the same 55.16 GB from the `fv-weights` R2 bucket five times
over presigned GETs:
- the `fastwan21-1.3b` tree (17 files, largest 6.0 GB);
- H3's FP8 text encoder (one 25.95 GB file).

The image was `nvidia/cuda:12.8.1-runtime-ubuntu24.04` with a 100 GB
container disk. The binary was `weights` 0.1.0, the v0.1.0 release, and
curl was 8.5.0.

The methods:
- **`curl8`** is the old boot: one `curl -sf --retry 5` per file, 8 files at
  a time (`xargs -P 8`).
- **`w32` / `w64`** are `weights plan -j 32 / -j 64`, with the defaults:
  64 MiB ranges, a 20 s read timeout and `--slow-after 4`.

Round 1 runs curl, w32, w64; round 2 runs w32, then curl. Every run
starts from an empty directory. Round 1's curl and w32 results were
SHA-256 checked file by file and **matched for all 18 files on both
hosts**. Script: `bench.sh`; plan: `make_plan.py`. The box outputs are
`out-jp.txt` and `out-ut.txt`, with the presigned URLs removed.

| host | curl8 | w32 | w64 | w32 (round 2) | curl8 (round 2) |
|---|---|---|---|---|---|
| Japan, RTX 5060 Ti, 16 CPUs, advertised 5.4 Gb/s (contract 62470881) | 287.0 s, 0.19 GB/s | **77.1 s, 0.72 GB/s** | 96.1 s, 0.57 GB/s | **79.6 s, 0.69 GB/s** | 226.3 s, 0.24 GB/s |
| Utah, RTX 3060, 24 CPUs, advertised 7.8 Gb/s (contract 62470880) | 294.4 s, 0.19 GB/s | 258.1 s, 0.21 GB/s | 271.2 s, 0.20 GB/s | 264.0 s, 0.21 GB/s | 297.5 s, 0.19 GB/s |

The slow-connection cut-off (`--slow-after`) cut, in Japan: 34 GETs (w32),
339 (w64), 66 (w32, round 2). In Utah: 2, 0 and 0.

**Japan.** `weights` was 2.8–3.7× faster than curl and filled the
advertised link (0.72 GB/s ≈ 5.8 Gb/s). curl's time is set by the
26 GB file, which it pulls as a single stream. Going from `-j 64` to 32 is
faster, not slower: at line rate every connection looks slow, so 64 only
added cuts and reconnects. Keep the boot's `-j 32`.

**Utah.** Every method ran at about 0.2 GB/s (1.6 Gb/s), so the
host, not the method, was the limit. A single curl stream on the large
file got as much as all 32 ranges together. `weights` was only 10–12 %
faster there. The disk was not measured, so the cap may be network
shaping or the disk; the advertised 7.8 Gb/s was not reached.

**Follow-up.** The cut-off should probably stand down while the total
rate is at the link's ceiling. On a full link the per-range median falls
for every range alike, and the cuts only cost reconnects. This is
untested; 339 cuts at `-j 64` is the evidence.
