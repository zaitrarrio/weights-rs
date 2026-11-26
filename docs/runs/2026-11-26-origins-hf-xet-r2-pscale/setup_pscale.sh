# Build the petascale-rs arm's origin: a chunk store behind a petascale
# server, holding the same tree the other arms pull. Run this once, on the
# machine that will serve the chunks (for a WAN measurement that must NOT
# be the machine running bench.sh).
#
#   SRC=/pristine/tree ORIGIN_ADDR=0.0.0.0:8099 ./setup_pscale.sh
#
# It prints the PSCALE_SNAPSHOT / PSCALE_NS / PSCALE_ORIGIN that bench.sh wants.
set -eu
: "${SRC:?set SRC to a pristine copy of the tree}"
: "${PSCALE:=pscale}"
: "${ORIGIN_ADDR:=127.0.0.1:8099}"
: "${STORE:=$PWD/ps-store}"
: "${API_KEY:=benchkey}"
: "${PSCALE_NS:=bench}"
: "${PROFILE:=weights}"   # 256 KiB/1 MiB/4 MiB — made for parallel range GETs
export PSCALE_HOME="${PSCALE_HOME:-$PWD/pscale-home}"
mkdir -p "$PSCALE_HOME" "$STORE"

$PSCALE serve --addr "$ORIGIN_ADDR" --store "$STORE" --api-key "$API_KEY" &
sleep 3
curl -sf "http://${ORIGIN_ADDR/0.0.0.0/127.0.0.1}/health" >/dev/null && echo "origin up on $ORIGIN_ADDR"

$PSCALE config set api_base "http://${ORIGIN_ADDR/0.0.0.0/127.0.0.1}"
$PSCALE config set api_key "$API_KEY"

# A namespace whose store IS the server, so backup uploads chunks to it
# and restore pulls them back over HTTP.
$PSCALE namespace create "$PSCALE_NS" --profile "$PROFILE" || true
cat > "$PSCALE_HOME/namespaces/$PSCALE_NS.toml" <<TOML
name = "$PSCALE_NS"
visibility = "private"
chunk_profile = "$PROFILE"
store = { backend = "worker", url = "http://${ORIGIN_ADDR/0.0.0.0/127.0.0.1}", api_key = "$API_KEY" }
TOML

$PSCALE backup --namespace "$PSCALE_NS" --name bench-tree "$SRC"
echo
echo "PSCALE_NS=$PSCALE_NS"
echo "PSCALE_SNAPSHOT=bench-tree"
echo "PSCALE_ORIGIN=http://${ORIGIN_ADDR/0.0.0.0/127.0.0.1}"
echo "store: $(du -sh "$STORE" | cut -f1)"
