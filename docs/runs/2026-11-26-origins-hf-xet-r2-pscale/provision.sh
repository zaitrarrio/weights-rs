# Prepare a bench host: the HF client, the two binaries, and the workload
# plan. Run on the machine that will run bench.sh. The binaries are copied
# in beforehand (they are glibc-dynamic, so build them on the same Ubuntu
# release as the host).
#
#   REPO=Wan-AI/Wan2.1-T2V-1.3B W=/w/weights PSCALE=/w/pscale ./provision.sh
set -eu
: "${REPO:?set REPO}"
: "${REV:=main}"
: "${ROOT:=/w}"
mkdir -p "$ROOT"

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q python3-venv python3-pip curl

python3 -m venv "$ROOT/venv"
# hf_xet is what makes the `hfxet` / `hfxethp` arms the Xet protocol and not
# the plain CDN; without it the hub silently falls back to HTTPS.
"$ROOT/venv/bin/pip" install -q 'huggingface_hub[hf_xet]'
"$ROOT/venv/bin/python" -c "import hf_xet; print('hf_xet present')"

# The workload plan, for the arms that take one.
python3 "$(dirname "$0")/make_plan.py" hf-plan "$REPO" "$REV" "$ROOT/plan-hf.txt"

# A pristine copy, used to seed R2 and the petascale origin so that every
# arm serves identical bytes.
HF_HUB_DISABLE_PROGRESS_BARS=1 "$ROOT/venv/bin/hf" download "$REPO" \
  --revision "$REV" --local-dir "$ROOT/pristine"
rm -rf "$ROOT/pristine/.cache"
echo "pristine: $(du -sh "$ROOT/pristine" | cut -f1)"

echo
echo "link ceilings on this host:"
dd if=/dev/zero of="$ROOT/ceil" bs=1M count=4096 conv=fsync 2>&1 | tail -1
rm -f "$ROOT/ceil"
U=$(curl -s -o /dev/null -w '%{url_effective}' -L -r 0-0 \
    "https://huggingface.co/$REPO/resolve/$REV/$(awk 'NR==1{print $3}' "$ROOT/plan-hf.txt")")
curl -s -o /dev/null -w '  one stream: %{speed_download} B/s\n' --max-time 60 "$U" || true
