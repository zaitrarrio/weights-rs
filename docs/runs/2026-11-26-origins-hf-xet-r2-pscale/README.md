# 2026-11-26: four origins for the same weight tree — HF, HF Xet, R2, petascale-rs

Harness for comparing the places we can pull weights from, plus a
**calibration run** on a cloud container, now including R2. Seven arms
have run against a real R2 bucket and Hugging Face; every one produced
byte-identical trees. The host saturates at ~0.3 GB/s, so it cannot
order the fast arms (`weights` over R2, `weights` over HF, Xet in
high-performance mode) — but the slow arms are nowhere near the ceiling,
so those numbers *are* measurements of the method: curl-per-file is 13×
down and petascale-rs over R2 is 10× down on the same bucket and bytes.

For a measurement that can carry a speed claim, run `bench.sh` on named
hardware as `docs/runs/2026-11-25-vast-r2-curl-vs-weights` did.

## Which petascale, and which backend

**petascale-rs only** — the `pscale` binary built from this tree's
sibling repo. petascale4 (the Go CLI + TypeScript Cloudflare Worker) is
not benchmarked here.

petascale-rs has three chunk-store backends and they are not
interchangeable for a transfer measurement:

| backend | requests per chunk | bytes served by |
|---|---|---|
| `worker` | 2 (presign, then GET) | the petascale server |
| `r2` | 1 (client-signed SigV4) | R2 |
| `cloudflare` | — | rejected by the CLI: "cloudflare backend is server-side only" |

Both are now measured: `pscale` (the `worker` backend) over loopback,
and `pscaler2` (the `r2` backend) against a real R2 bucket. `pscaler2`
is the one to read next to `r2w`, and it comes in 10× down — see
"R2: measured".

### Serverless

petascale-rs does not run serverless today, by its own docs. README:
`worker/ (reserved; Cloudflare deployment stays a future workers-rs
build)`. `docs/deploy-rs.md`: "there is no `wrangler.toml`/
`wrangler.jsonc` yet. Until then, the supported way to run petascale-rs
'on Cloudflare' is the axum server on a VPS (or any host) behind
Cloudflare Tunnel / a proxied DNS record, with R2 as the chunk-backup
target via `rclone`." There is no `workers-rs` dependency and no wasm
target in the tree.

The serverless implementation is petascale4's Worker
(`worker/wrangler.toml`, R2 + D1 + KV bindings). So "petascale-rs
serverless" is not a thing that can be benchmarked yet; what can be
compared today is the axum server, the `r2` backend, and — if it is
wanted — petascale4's deployed Worker as a separate arm.

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
| `pscale` | petascale-rs, `worker` backend | `pscale restore` | CDC chunks, 2 requests per chunk |
| `pscaler2` | petascale-rs, `r2` backend | `pscale restore` | CDC chunks straight off R2, 1 request per chunk |

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

# 3a. petascale-rs `r2` backend — the arm comparable to r2w. Chunks the
#     tree and uploads the chunks into R2; restore GETs them back from R2.
SRC=/root/pristine R2_BUCKET=fv-weights ./setup_pscale_r2.sh

# 3b. OPTIONAL: the self-hosted `worker` backend, for what running your
#     own origin costs. On a DIFFERENT machine, or the number is loopback.
SRC=/root/pristine ORIGIN_ADDR=0.0.0.0:8099 ./setup_pscale.sh

# 4. The run.
REPO=Wan-AI/Wan2.1-T2V-1.3B REV=main \
PLAN_HF=/root/plan-hf.txt PLAN_R2=/root/plan-r2.txt \
PSCALE_SNAPSHOT=bench-tree PSCALE_NS=bench \
PSCALE_SNAPSHOT_R2=bench-tree-r2 PSCALE_NS_R2=bench-r2 \
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

Five arms, four rounds, alternating direction, chunk caches emptied
before every run. `out-container.txt`, summarised by `summarize.py`:

| arm | runs | median | range | median time |
|---|---|---|---|---|
| `hfxethp` — Xet, `HF_XET_HIGH_PERFORMANCE=1` | 4/4 | 0.311 GB/s | 0.298–0.323 | 9.9 s |
| `pscale` — petascale-rs `worker`, **loopback** | 4/4 | 0.310 GB/s | 0.302–0.311 | 10.0 s |
| `hfw` — `weights -j 32` over HF | 4/4 | 0.303 GB/s | 0.294–0.313 | 10.2 s |
| `hfxet` — Xet, default | 4/4 | 0.206 GB/s | 0.189–0.226 | 15.1 s |
| `hf` — plain HTTPS, Xet off | 4/4 | 0.027 GB/s | 0.020–0.028 | 114.3 s |

**All 20 runs produced byte-identical trees** — 10 files, SHA-256
compared against `hf-1` across every arm and round.

The top three are a tie, and that is the main thing this run says about
them: 0.303–0.311 GB/s is this container's ceiling (32 parallel ranges
peaked at ~0.315 GB/s in the standalone probe), so the measurement has
no power to order `hfw`, `hfxethp` and `pscale`. Any claim that one of
them beats another needs a host that is not saturated.

What the run does support, because the gaps are far outside the spread:

1. **Disabling Xet costs 11×.** `hf` moved this tree at 0.027 GB/s
   against the same CDN the 0.303 GB/s arm used, because without Xet
   the client parallelises over *files* and this tree is one 3.09 GB
   file plus noise — one stream, 114 s. Same shape as curl-per-file in
   2026-11-25, and it is the default wherever `hf_xet` is not installed.
2. **Xet's default mode leaves ~50 % on the table.** 0.206 vs
   0.311 GB/s for the same client with `HF_XET_HIGH_PERFORMANCE=1`.
   That gap (1.5×) is well outside both arms' ranges.
3. **petascale-rs needed loopback to reach the ceiling the others
   reached over the public internet.** 0.310 GB/s with the origin on
   `127.0.0.1` is not evidence of a fast client — it is the same ceiling,
   reached with the network removed. It sets an upper bound, not a rate,
   and the `worker` backend's two-requests-per-chunk cost is still only
   projected (see below), never measured on a real link.

An earlier 4-round attempt is **not** included here: `bench.sh` was
edited while bash was still reading it, which shifted the script's byte
offsets, ran `hf` twice in round 4 and killed the run before the
identity check. Freeze a copy of the harness before a long run.

## R2: measured

Bucket `weights-bench`, created for this in **WNAM** so it sits where
`fv-weights` sits, holding the same `Qwen/Qwen2.5-1.5B` tree twice: ten
whole objects under `bench/` for `r2w`/`r2curl8`, and 3467 CDC chunks
under `cas/` for `pscaler2`. The R2 plan was diffed against the HF plan
— same ten relative paths, same ten sizes — so every arm moves identical
bytes. Credentials are a bucket-scoped R2 API token. Two rounds,
`out-container-r2.txt`:

| arm | median | range |
|---|---|---|
| `hfw` — `weights -j 32` over HF | 0.296 GB/s | 0.293–0.299 |
| `r2w` — `weights -j 32` over R2 | **0.286 GB/s** | 0.227–0.346 |
| `pscaler2` — petascale-rs `r2` backend | **0.029 GB/s** | 0.020–0.038 |
| `r2curl8` — one curl per file, 8 at a time | **0.022 GB/s** | 0.017–0.027 |

All 7 runs byte-identical against `hfw-1`.

This run means more than the earlier one, because the arms sit on
different sides of the host's ceiling. `r2w` and `hfw` are *at* it
(~0.3 GB/s), so their real speed is higher than shown and they cannot be
ordered against each other. `pscaler2` and `r2curl8` are nowhere near
it, so **those two numbers are measurements of the method, not of the
host** — and they are an order of magnitude down on the same bucket,
same bytes, same machine, same minute.

- **R2 and Hugging Face are interchangeable for `weights`.** 0.286 vs
  0.296 GB/s, both ceiling-bound. Nothing here says one origin is
  faster; it says the client is what matters.
- **curl-per-file is 13× down** (0.022 vs 0.286). The tree is one
  3.09 GB file, so eight parallel curls are one stream. This is the
  2026-11-25 finding reproduced against a different bucket.
- **petascale-rs over R2 is 10× down** (0.029 vs 0.286) — and that is
  the comparison this whole exercise was for.

### Why petascale-rs is 10× down, quantified

Per-chunk latency from this host to R2 WNAM, measured directly:
**median 133 ms**, max 307 ms, for 875 KiB chunks. Little's Law then
bounds the client at `in_flight_bytes / latency`, and
`DEFAULT_PARALLELISM` is 16:

| in flight | ceiling at 133 ms |
|---|---|
| petascale-rs: 16 × 875 KiB = 14.3 MB | **108 MB/s** |
| `weights`: 32 × 64 MiB = 2.1 GB | far past the link — bandwidth-bound |

A sliding-window fetch of the same chunks at 16 in flight measured
**96 MB/s**, which matches the 108 MB/s prediction, so the model holds.
Two things then take petascale-rs from that 108 MB/s bound down to the
29 MB/s observed:

1. **`download_to_writer` is a lock-step barrier, not a sliding
   window.** It walks `refs.chunks(parallelism)` — await all 16, write
   all 16, then start the next 16 — so nothing is in flight while it
   writes and every batch costs the *slowest* chunk in it. Measured:
   227 ms per batch of 16 = 63 MB/s, against 96 MB/s for the same
   chunks pipelined. That is ~1.5× lost to the barrier alone, before
   per-chunk SHA-256 and the ordered write.
2. **The chunk-size × parallelism product is too small for the RTT.**
   Even perfectly pipelined, 14.3 MB in flight cannot exceed 108 MB/s
   at 133 ms. Reaching the 0.346 GB/s `weights` managed would need
   **~52 chunks in flight** at this chunk size, against the 16 the code
   uses.

So the fix is not one bug. In order of payoff: raise the in-flight
chunk count (the ceiling is linear in it — p52 ≈ 350 MB/s, p64 ≈
431 MB/s), replace the barrier with a sliding window plus a reorder
buffer so the pipeline never drains, and consider larger chunks for the
`weights` profile. The presign round trip documented below is a
*third*, separate cost that applies to the `worker` backend on top of
all this.

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
   round trip buys nothing. Halves the RTT cost.

   Presigning does **not** redeem itself on the Cloudflare path either,
   which is worth stating because it is the natural assumption. From
   petascale4's `worker/src/cas/presign.ts`: "The R2 binding does not
   expose S3-style presigned URLs inside the Workers runtime, so we mint
   short-lived HMAC-signed URLs that point back at the Worker's own
   direct PUT/GET chunk endpoints." The signed URL comes back to the
   Worker, so the bytes still flow through it and the presign is still a
   second round trip.

   The only backend that avoids the hop entirely is `r2`:
   `R2Store::get` signs SigV4 itself and issues **one** GET against
   R2's S3 endpoint, so the bytes come off R2's edge. That is the
   `pscaler2` arm, and it is the one to compare against `r2w`.
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

- **The fast arms' real speed.** `r2w`, `hfw` and `hfxethp` all sat on
  this host's ~0.3 GB/s ceiling, so they are indistinguishable here and
  their true rates are higher. Only a host that is not saturated can
  order them. The slow arms (`r2curl8`, `pscaler2`) are unaffected by
  this — they never approached the ceiling.
- **Scale.** 3.10 GB over 10 files, of which one is 3.09 GB. The
  intended workload is `Wan-AI/Wan2.1-T2V-1.3B`, 17.6 GB over 22 files
  with an 11.4 GB file, which is the shape the 2026-11-25 run used.
- **petascale-rs's `worker` backend over a network.** That arm ran
  against an origin on `127.0.0.1`, which is an upper bound, not a
  transfer rate. Its presign round trip is still projected, never
  measured on a real link. `pscaler2` (the `r2` backend) *is* now
  measured.
- **petascale4.** Not benchmarked. It is the only implementation with a
  serverless deployment (its Worker), so "petascale serverless" is
  unmeasured here and, per petascale-rs's own docs, not yet buildable
  in the Rust port.
- **Production hardware.** 4 vCPU behind a TLS-terminating proxy is not a
  GPU box on a 5 Gb/s link. Ordering between the arms should hold; the
  absolute numbers should not be quoted.

## Next

The comparison this exercise was for is answered: `pscaler2` vs `r2w`,
same bucket, same bytes, 10× apart, with a measured cause. What a rented
box would add is the *fast* end — separating `r2w`, `hfw` and `hfxethp`,
which this container cannot — and the 17.6 GB tree instead of 3.10 GB.
`provision.sh` + `setup_pscale_r2.sh` + `bench.sh` are what that box
needs; `ARMS` runs the subset whose inputs are present.

Pick the host for measured bandwidth, not price alone. Vast exposes
`inet_down`/`inet_up` per offer, so
`GET /api/v0/bundles/?q={"inet_down":{"gte":3000},...}` finds boxes that
can actually show a difference; at the time of writing, verified 1-GPU
offers with ≥3 Gb/s down and ≥150 GB disk started at $0.082/h. This
container could not order its top three arms at all — they all sat on
its ~0.31 GB/s ceiling.

### Driving a rented box from a restricted container

Both attempts to rent a bench box failed for one reason, and it took far
too long to find: **this container can only open outbound TCP on port
443.** `github.com:22` is refused while `ssh.github.com:443` connects.
Every SSH endpoint on offer is a high port — RunPod mapped 22 to 22025 /
22173 / 22022, Vast's proxy gave 24490 and 10508 — so all of them were
refused at the TCP layer, before any key was ever offered.

Test this first, before renting anything:

```sh
timeout 15 bash -c 'cat < /dev/null > /dev/tcp/github.com/22' \
  && echo "port 22 reachable" || echo "non-443 egress blocked"
```

Two earlier explanations recorded here were wrong, and are retracted:
that RunPod's images build `authorized_keys` from a single-valued
account SSH key field (so the pods were unreachable), and that two Vast
machines had broken tunnels. The account-key detail may well be true of
RunPod in general, but it was not why those pods were unreachable, and
the Vast machines were almost certainly fine. The cause was local in
every case.

What does work from here, all on 443: the provider REST APIs, R2 and the
Hugging Face CDN, and RunPod's `https://<podId>-<port>.proxy.runpod.net`
(it answers 404 when nothing is bound). So a bench box has to be driven
without an interactive shell — an unattended `onstart` that runs
`provision.sh` + `bench.sh` and `PUT`s `$RES` into an R2 bucket the
operator can read, or a small HTTP endpoint behind a 443 proxy. The
harness is already shaped for that: every step is a separate script
taking its inputs from the environment.
