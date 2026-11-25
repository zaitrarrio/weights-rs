# Same trees, same box: one curl per file (the old boot, xargs -P 8) vs the
# weights CLI. Plan lines: URL SIZE REL. Rounds alternate the order.
set -u
P=/root/plan.txt; W=/w/weights; D=/w/t
for i in 1 2 3 4 5; do [ -x $W ] && $W --version >/dev/null 2>&1 && break; curl -sfL -o $W https://github.com/zaitrarrio/weights-rs/releases/download/v0.1.0/weights-x86_64-linux && chmod +x $W; sleep 20; done
echo "== host"; nproc; free -g | head -2; df -h /w | tail -1; curl --version | head -1; $W --version
total=$(awk '{s+=$2} END {print s}' $P); echo "plan: $(wc -l < $P) files, $total bytes"
run() {  # name, command...
  local name=$1; shift
  rm -rf $D; mkdir -p $D; sync
  local t0=$(date +%s.%N)
  timeout 1800 bash -c "$*" > /root/res/$name.log 2>&1; local rc=$?
  local t1=$(date +%s.%N)
  local got=$(du -sb $D | cut -f1)
  awk -v n=$name -v a=$t0 -v b=$t1 -v g=$got -v t=$total -v rc=$rc 'BEGIN {s=b-a; printf "RESULT %-12s rc=%d %7.1f s  %6.2f GB  %5.2f GB/s%s\n", n, rc, s, g/1e9, g/1e9/s, (g==t ? "" : "  INCOMPLETE")}'
}
curl8() { awk -v d=$D '{print $1, d "/" $3}' $P | xargs -P 8 -n 2 sh -c 'mkdir -p "$(dirname "$1")" && curl -sf --retry 5 -o "$1" "$0" || echo "FAILED $1"'; }
wj() { awk -v d=$D '{print $1, $2, d "/" $3}' $P > /root/wplan.txt; $W plan /root/wplan.txt -j $1 --progress 15; }
export -f curl8 wj; export P W D
hashes() { (cd $D && find . -type f | sort | xargs -P "$(nproc)" -n 1 sha256sum | sort -k2) > /root/res/$1.sha; }
mkdir -p /root/res
echo "== round 1"
run curl8-1 curl8; hashes curl8
run w32-1 wj 32; hashes w32
cmp -s /root/res/curl8.sha /root/res/w32.sha && echo "HASHES weights == curl ($(wc -l < /root/res/w32.sha) files)" || { echo "HASHES DIFFER"; diff /root/res/curl8.sha /root/res/w32.sha | head; }
run w64-1 wj 64
echo "== round 2 (reversed)"
run w32-2 wj 32
run curl8-2 curl8
grep -h "slow GETs\|weights: .* files" /root/res/w*.log | cut -c1-200
rm -rf $D
