# Four origins, one host, one weight tree: Hugging Face over plain HTTPS,
# Hugging Face over Xet, R2 over presigned ranged GETs, and a petascale-rs
# chunk store. Every arm moves the same bytes to the same relative paths,
# so the per-file SHA-256 sets must match across arms.
#
# Required: REPO REV (the workload). Optional per arm:
#   PLAN_HF  plan for `hfw`   (make_plan.py hf-plan)
#   PLAN_R2  plan for `r2w` / `r2curl8` (make_plan.py r2-plan)
#   PSCALE_ORIGIN + PSCALE_SNAPSHOT + PSCALE_NS for `pscale`
# ARMS selects a subset; J is the weights job count; HFJ the hf one.
set -u
: "${REPO:?set REPO, e.g. Wan-AI/Wan2.1-T2V-1.3B}"
: "${REV:=main}"
: "${D:=/w/t}"            # download dir, wiped before every run
: "${RES:=/w/res}"        # results
: "${J:=32}"              # weights -j
: "${HFJ:=16}"            # hf download --max-workers
: "${W:=weights}"         # the weights binary
: "${PSCALE:=pscale}"
: "${HF:=hf}"
: "${ARMS:=hf hfxet hfxethp hfw r2w r2curl8 pscale}"
: "${ROUNDS:=2}"
mkdir -p "$RES"

echo "== host"
nproc; free -g | head -2; df -h "$(dirname "$D")" | tail -1
curl --version | head -1; $W --version; $HF version 2>/dev/null | head -1
echo "== workload: $REPO @ $REV"
# The expected total, so a truncated run reads as INCOMPLETE and not as fast.
TOTAL=${TOTAL:-0}
[ "$TOTAL" = 0 ] && [ -n "${PLAN_HF:-}" ] && TOTAL=$(awk '{s+=$2} END {print s+0}' "$PLAN_HF")
[ "$TOTAL" = 0 ] && [ -n "${PLAN_R2:-}" ] && TOTAL=$(awk '{s+=$2} END {print s+0}' "$PLAN_R2")
echo "expecting $TOTAL bytes"

# Ceilings, so a slow arm can be told from a slow box.
echo "== ceilings"
# Disk write: the 2026-11-25 run could not tell a shaped link from a slow disk.
dd if=/dev/zero of="$RES/ceil" bs=1M count=4096 conv=fsync 2>&1 | tail -1
# SHA-256 on one core: this is what bounds petascale-rs's verify-on-read.
t0=$(date +%s.%N); sha256sum "$RES/ceil" >/dev/null; t1=$(date +%s.%N)
awk -v a=$t0 -v b=$t1 'BEGIN {d=b-a; printf "sha256 1 core: %.2f s for 4.29 GB = %.0f MB/s\n", d, 4294967296/1e6/d}'
rm -f "$RES/ceil"

# ---- arms -------------------------------------------------------------
# Plain HTTPS from the HF CDN: the hf client with Xet turned off.
arm_hf()      { HF_HUB_DISABLE_XET=1 HF_HUB_DISABLE_PROGRESS_BARS=1 \
                $HF download "$REPO" --revision "$REV" --local-dir "$D" --max-workers "$HFJ"; }
# HF's own fast path, so the `hf` arm above is not mistaken for the best
# Hugging Face can do. NOTE: HF_HUB_ENABLE_HF_TRANSFER is a no-op as of
# huggingface_hub 2.4.0 ("hf_transfer is not used anymore"); Xet high
# performance replaced it, and an `hf_transfer` arm measures plain `hf`.
arm_hfxethp() { HF_XET_HIGH_PERFORMANCE=1 HF_HUB_DISABLE_PROGRESS_BARS=1 \
                $HF download "$REPO" --revision "$REV" --local-dir "$D" --max-workers "$HFJ"; }
# The same client over the Xet CAS protocol (chunk-level dedup).
arm_hfxet()   { HF_HUB_DISABLE_PROGRESS_BARS=1 \
                $HF download "$REPO" --revision "$REV" --local-dir "$D" --max-workers "$HFJ"; }
# Our client, ranged and parallel, over the same HF URLs: separates the
# origin from the client.
arm_hfw()     { awk -v d="$D" '{print $1, $2, d "/" $3}' "$PLAN_HF" > "$RES/wplan-hf.txt"
                $W plan "$RES/wplan-hf.txt" -j "$J" --progress 15; }
# Our client over presigned R2 GETs: the production boot.
arm_r2w()     { awk -v d="$D" '{print $1, $2, d "/" $3}' "$PLAN_R2" > "$RES/wplan-r2.txt"
                $W plan "$RES/wplan-r2.txt" -j "$J" --progress 15; }
# The old boot, kept for continuity with 2026-11-25: one curl per file, 8 at a time.
arm_r2curl8() { awk -v d="$D" '{print $1, d "/" $3}' "$PLAN_R2" \
                | xargs -P 8 -n 2 sh -c 'mkdir -p "$(dirname "$1")" && curl -sf --retry 5 -o "$1" "$0" || echo "FAILED $1"'; }
# petascale-rs: restore a snapshot out of a chunk store over HTTP.
arm_pscale()  { $PSCALE restore "$PSCALE_SNAPSHOT" --namespace "${PSCALE_NS:-bench}" --dest "$D"; }

# Both Xet and petascale keep a local chunk cache. A warm cache turns the
# second round into a disk read, so every run starts with them empty.
cold() {
  rm -rf "${HF_HOME:-$HOME/.cache/huggingface}/xet" 2>/dev/null
  [ -n "${PSCALE_CACHE:-}" ] && rm -rf "$PSCALE_CACHE" && mkdir -p "$PSCALE_CACHE"
  sync; return 0
}

export -f arm_hf arm_hfxethp arm_hfxet arm_hfw arm_r2w arm_r2curl8 arm_pscale
export REPO REV D RES J HFJ W PSCALE HF PLAN_HF PLAN_R2 PSCALE_SNAPSHOT PSCALE_NS

# ---- harness ----------------------------------------------------------
# hf download leaves its own bookkeeping dir; it is not part of the tree.
hashes() { (cd "$D" && find . -type f -not -path './.cache/*' -printf '%P\n' \
            | sort | xargs -P "$(nproc)" -n 1 sha256sum | sort -k2) > "$RES/$1.sha"; }
bytes()  { find "$D" -type f -not -path "$D/.cache/*" -printf '%s\n' | awk '{s+=$1} END {print s+0}'; }

run() { # name armfn
  local name=$1 fn=$2
  rm -rf "$D"; mkdir -p "$D"; cold
  local t0 t1 rc got
  t0=$(date +%s.%N)
  timeout 7200 bash -c "$fn" > "$RES/$name.log" 2>&1; rc=$?
  t1=$(date +%s.%N)
  got=$(bytes)
  awk -v n="$name" -v a="$t0" -v b="$t1" -v g="$got" -v rc="$rc" -v t="$TOTAL" \
    'BEGIN {s=b-a; printf "RESULT %-14s rc=%d %8.1f s  %7.2f GB  %5.3f GB/s%s\n", \
            n, rc, s, g/1e9, g/1e9/s, (t>0 && g!=t ? "  INCOMPLETE" : "")}'
  if [ "$rc" -eq 0 ] && { [ "$TOTAL" = 0 ] || [ "$got" = "$TOTAL" ]; }; then hashes "$name"
  else echo "  (no hashes: rc=$rc, $got of $TOTAL bytes)"; sed -n '1,5p' "$RES/$name.log"; fi
}

for r in $(seq 1 "$ROUNDS"); do
  # Round 2 runs the arms in reverse, so a drifting link shows up as a
  # disagreement between rounds rather than as an ordering artefact.
  list=$ARMS
  [ "$((r % 2))" -eq 0 ] && list=$(echo $ARMS | tr ' ' '\n' | tac | tr '\n' ' ')
  echo "== round $r: $list"
  for a in $list; do
    case " $ARMS " in *" $a "*) ;; *) continue ;; esac
    [ "$a" = hfw ] && [ -z "${PLAN_HF:-}" ] && { echo "skip hfw: no PLAN_HF"; continue; }
    case "$a" in r2w|r2curl8) [ -z "${PLAN_R2:-}" ] && { echo "skip $a: no PLAN_R2"; continue; } ;; esac
    [ "$a" = pscale ] && [ -z "${PSCALE_SNAPSHOT:-}" ] && { echo "skip pscale: no PSCALE_SNAPSHOT"; continue; }
    run "$a-$r" "arm_$a"
  done
done

echo "== byte-identity across arms"
ref=""
for f in "$RES"/*.sha; do
  [ -s "$f" ] || continue
  [ "$(wc -l < "$f")" -gt 0 ] || continue
  if [ -z "$ref" ]; then ref=$f; echo "reference: $(basename "$ref" .sha) ($(wc -l < "$ref") files)"; continue; fi
  if cmp -s "$ref" "$f"; then echo "  OK    $(basename "$f" .sha)"
  else echo "  DIFFER $(basename "$f" .sha)"; diff "$ref" "$f" | head -5; fi
done
grep -h "slow GETs" "$RES"/*.log 2>/dev/null | cut -c1-160
rm -rf "$D"
