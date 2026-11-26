# 2026-11-26: four origins for the same weight tree — HF, HF Xet, R2, petascale-rs

Harness for comparing the places we can pull weights from, plus a
**calibration run** of the arms that are reachable from a Claude Code
cloud container. The R2 arm has not been run yet and the petascale-rs arm
has only been run over loopback, so **this file makes no claim about R2
vs Hugging Face on production hardware**. What it does establish is that
all four arms produce byte-identical trees, what the client-side ceilings
are, and that petascale-rs's chunk download path has a latency ceiling
that no link speed will fix.

For a measurement that can carry a speed claim, run `bench.sh` on named
hardware as `docs/runs/2026-11-25-vast-r2-curl-vs-weights` did.

## The arms

Every arm moves the same bytes to the same relative paths, so the
per-file SHA-256 sets must match. `bench.sh` compares them and says
`DIFFER` if they do not.

| arm | origin | client | what it isolates |
|---|---|---|---|
| `hf` | HF CDN, plain HTTPS | `hf download`, Xet off | whole-file parallelism only — one stream per file |
| `hftransfer` | HF CDN, plain HTTPS | `hf download` + `hf_transfer` | HF's own ranged accelerator |
| `hfxet` | HF Xet CAS | `hf download` + `hf_xet` | the Xet chunk protocol |
| `hfw` | HF CDN, plain HTTPS | `weights plan -j 32` | our client against HF's origin |
| `r2w` | R2, presigned GETs | `weights plan -j 32` | the production boot |
| `r2curl8` | R2, presigned GETs | one curl per file, 8 at a time | the old boot, for continuity with 2026-11-25 |
| `pscale` | petascale-rs chunk store | `pscale restore` | CDC chunks + verify-on-read |

`hf` and `hfw` hit the *same URLs*, so the gap between them is the
client, not the origin. `hf` and `hftransfer` differ only by the
accelerator, so `hf` alone must not be read as "what Hugging Face can
do".

## Running it

```sh
# 1. The workload: an HF repo at a pinned revision defines the tree.
python3 make_plan.py hf-plan Wan-AI/Wan2.1-T2V-1.3B main /root/plan-hf.txt

# 2. Mirror that tree into R2 and presign it (needs R2 credentials).
python3 make_plan.py mirror-r2 /root/pristine fv-weights bench/wan21-1.3b
python3 make_plan.py r2-plan   fv-weights bench/wan21-1.3b /root/plan-r2.txt

# 3. The petascale origin, on a DIFFERENT machine for a WAN number.
SRC=/root/pristine ORIGIN_ADDR=0.0.0.0:8099 ./setup_pscale.sh

# 4. The run.
REPO=Wan-AI/Wan2.1-T2V-1.3B REV=main \
PLAN_HF=/root/plan-hf.txt PLAN_R2=/root/plan-r2.txt \
PSCALE_SNAPSHOT=bench-tree PSCALE_NS=bench \
PSCALE_HOME=/root/pscale-home PSCALE_CACHE=/root/ps-cache \
D=/w/t RES=/w/res J=32 HFJ=16 ROUNDS=4 ./bench.sh
```

Pipe the output through `summarize.py` for a per-arm median and range;
one round on a shared link says very little. `ARMS` selects a subset. Rounds alternate direction, so a drifting link
shows up as a disagreement between rounds rather than as an ordering
artefact. Xet and petascale both keep a local chunk cache; `bench.sh`
empties both before every run, or round 2 would be a disk read.

## Calibration run

Host: a Claude Code cloud container — 4 vCPU, 15 GB RAM, `/dev/vda`,
**all egress through an HTTPS agent proxy**. Not a GPU box and not a
clean link: the proxy terminates TLS for every arm, and the container is
shared, so round-to-round spread is large. Workload
`Qwen/Qwen2.5-1.5B @ main`, 10 files, 3.10 GB, of which one file is
3.09 GB.

Ceilings on this host, measured by `bench.sh`:

| ceiling | value |
|---|---|
| disk write (`dd`, 4.3 GB, fsync) | 603–915 MB/s |
| SHA-256, one core | 1221 MB/s |
| one HTTPS stream from the HF CDN | 18.8 MB/s |
| 8 parallel ranges, same file | 135.6 MB/s |
| best observed, 32 parallel ranges | ~315 MB/s |

The single-stream figure is the important one: at 18.8 MB/s a 26 GB file
takes 23 minutes however fast the link is. That is the same effect that
set curl's time in the 2026-11-25 run.

### Results

Four rounds, alternating direction, cold caches every run. Median and
range per arm, from `out-container.txt` via `summarize.py`:

| arm | runs | median | range | median time |
|---|---|---|---|---|
| `hfw` — `weights -j 32` over HF | 4/4 | **0.309 GB/s** | 0.173–0.327 | 10.0 s |
| `pscale` — petascale-rs, **loopback** | 4/4 | **0.294 GB/s** | 0.284–0.298 | 10.6 s |
| `hfxet` — HF Xet | 4/4 | **0.205 GB/s** | 0.172–0.221 | 15.2 s |
| `hftransfer` — ignored env var, = `hf` | 3/3 | 0.027 GB/s | 0.019–0.027 | 116.0 s |
| `hf` — plain HTTPS, Xet off | 3/3 | 0.027 GB/s | 0.027–0.027 | 115.3 s |

**All 18 runs produced byte-identical trees** — 10 files, SHA-256
compared across every arm and round.

Three things to take from this, none of which depend on the absolute
numbers:

1. **Disabling Xet costs 11×.** `hf` moved this tree at 0.027 GB/s
   against the same CDN the 0.309 GB/s arm used, because without Xet the
   client parallelises over *files* and this tree is one 3.09 GB file
   plus noise. One stream, one file, 115 s. This is the curl-per-file
   shape from 2026-11-25 all over again, and it is the default whenever
   `hf_xet` is not installed.
2. **Ranged parallelism beat the Xet client by ~1.5×** on the same
   origin — 0.309 vs 0.205 GB/s, and the gap is wider than the
   round-to-round spread of either arm. Xet is doing more work per byte
   (chunk lookup, dedup bookkeeping, verification) for a tree where
   there is nothing to dedup.
3. **petascale-rs needed loopback to draw level.** 0.294 GB/s with the
   origin on `127.0.0.1` — no network, no RTT, no TLS — is still
   *slower* than `weights` pulling over the public internet. Whatever
   the link, that is the client's own ceiling on 4 vCPU.

A caveat on the spread: `hfw` ranged 0.173–0.327, and the single slow
round was the only one in which the straggler watchdog fired (1 cut;
the three 0.29–0.33 GB/s rounds had none). That is n=1 and the cut may
be a symptom of a slow round rather than its cause, but it points the
same way as the open follow-up in the 2026-11-25 notes: on a link at its
ceiling, `--slow-after` may be paying reconnects for nothing.

## `hf_transfer` is dead — do not benchmark against it

The first version of this harness had an `hftransfer` arm setting
`HF_HUB_ENABLE_HF_TRANSFER=1`, on the assumption that it was HF's ranged
accelerator and therefore the fair opponent. It is not, as of
`huggingface_hub` 2.4.0:

```
FutureWarning: The `HF_HUB_ENABLE_HF_TRANSFER` environment variable is
deprecated as 'hf_transfer' is not used anymore. Please use
`HF_XET_HIGH_PERFORMANCE` instead to enable high performance transfer
with Xet.
```

The variable is ignored, so the arm measured plain `hf` and came in at
the same 0.027 GB/s. Xet is HF's fast path now, and
`HF_XET_HIGH_PERFORMANCE=1` is the knob — that is the `hfxethp` arm.
Anyone citing `hf_transfer` numbers for current `huggingface_hub` is
measuring the plain CDN.

## petascale-rs: the chunk path is latency-bound, not bandwidth-bound

The `weights` profile chunked this tree into 3467 chunks averaging
873 KiB (`pscale backup`, snapshot `bench-tree`). `HttpStore::get`
(`crates/petascale-store/src/http.rs`) fetches each one with **two**
requests: `GET /api/v1/cas/chunks/<hash>/presign-download` for an HMAC
URL, then a `GET` of that URL. `transfer::download_chunks` runs
`DEFAULT_PARALLELISM = 16` of those in flight.

Two requests per 873 KiB at 16 in flight puts a ceiling on the client
that the link cannot lift:

| RTT to origin | chunks/s | ceiling |
|---|---|---|
| 0.5 ms (loopback) | 16000 | 14.3 GB/s |
| 20 ms (same region) | 400 | 0.36 GB/s |
| 40 ms (cross-country) | 200 | 0.18 GB/s |
| 80 ms (cross-ocean) | 100 | 0.09 GB/s |

For scale: the Japan host in `docs/runs/2026-11-25-vast-r2-curl-vs-weights`
reached **0.72 GB/s** with `weights -j 32` against R2. A petascale origin
80 ms away would cap that host at **0.09 GB/s**, 8× slower, with the link
and the disk both idle.

Three things make it go away, and the first is free:

1. **Skip the presign hop for the `worker` backend.** The server already
   serves `GET /api/v1/cas/chunks/<hash>` in one request
   (`cas_get_direct`), reading the chunk straight off disk, and the
   presigned URL it hands back points at that same server — so the first
   round trip buys nothing. Halves the RTT cost. (Presigning *is* the
   point for the `r2` / `cloudflare` backends, where the URL offloads the
   bytes to R2; the fix belongs behind a backend check, not a blanket
   removal.)
2. **Raise the in-flight count.** The ceiling is linear in it, and 16 is
   low next to the 32 that `weights` found best on a 5 Gb/s link.
3. **Chunk larger.** The ceiling is linear in chunk size too; `weights`
   moves 64 MiB ranges, 75× larger than these chunks.

The loopback measurement is a *separate* ceiling: ~0.29 GB/s on 4 vCPU,
where RTT is ~0 and the limit is server work plus per-chunk SHA-256
verification. The binding constraint in production is the smallest of the
three — CPU, RTT, link — and on any real WAN the RTT term is the one that
bites first.

## What this run does not answer

- **R2.** `r2.cloudflarestorage.com` is not reachable from this
  container and no R2 credentials are present here, so `r2w` and
  `r2curl8` did not run. The 2026-11-25 run is still the only R2
  measurement we have. The *code* for it is exercised, though:
  `mirror-r2` → `r2-plan` → `weights plan` was run against a local
  `moto` S3 mock (`R2_ENDPOINT` overrides the account endpoint for
  exactly this) and round-tripped byte-identically, so the arm should
  work as soon as credentials exist. What is untested is real R2
  behaviour — presign expiry at scale, and whether R2 honours the
  multi-range GETs the way the mock does.
- **petascale-rs over a network.** The `pscale` arm ran against an origin
  on `127.0.0.1`, which is a client-side ceiling, not a transfer rate.
  It needs an origin on another machine to mean anything.
- **Production hardware.** 4 vCPU behind a TLS-terminating proxy is not a
  GPU box on a 5 Gb/s link. Ordering between the arms should hold; the
  absolute numbers should not be quoted.

## Next

The claim-worthy run wants a rented box, R2 credentials in its
environment, and two petascale origins (one near the client, one
deliberately far) to turn the latency table above into a measurement.
`provision.sh` + `setup_pscale.sh` + `bench.sh` are what that box needs;
`ARMS` runs the subset whose inputs are present.

One practical note for RunPod: pods deployed from `runpod-ubuntu-2404`
(and the bare `runpod/base:1.0.2-ubuntu2404` image) with `PUBLIC_KEY` in
`env` came up `RUNNING` with 22/tcp mapped but nothing listening, on
three separate machines across both community and secure cloud.
Overriding `dockerArgs` to start `sshd` by hand instead crashlooped the
container, since it replaces the image's own entrypoint. The pod's HTTP
proxy (`https://<podId>-<port>.proxy.runpod.net`) *is* reachable and
answers 404 when nothing is bound, so an unattended run that serves
`$RES` over that proxy is the fallback that needs no SSH at all.
