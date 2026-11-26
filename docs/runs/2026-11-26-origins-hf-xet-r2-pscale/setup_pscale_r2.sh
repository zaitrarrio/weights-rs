# Build the petascale-rs `r2`-backend arm: a namespace whose chunk store IS
# the R2 bucket, so `restore` has the client sign SigV4 and GET each chunk
# straight from R2 (one request per chunk, bytes off R2's edge). This is the
# configuration comparable to weights-rs's r2w arm; the `worker` backend in
# setup_pscale.sh is a different thing and should not be confused with it.
#
#   SRC=/w/pristine R2_BUCKET=fv-weights R2_PREFIX=bench/pscale ./setup_pscale_r2.sh
#
# Needs R2_ACCOUNT_ID + R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY in the
# environment (AWS_* also accepted; R2_ENDPOINT overrides the endpoint).
set -eu
: "${SRC:?set SRC to a pristine copy of the tree}"
: "${R2_BUCKET:?set R2_BUCKET}"
: "${R2_ACCOUNT_ID:?set R2_ACCOUNT_ID}"
: "${PSCALE:=pscale}"
: "${PSCALE_NS_R2:=bench-r2}"
: "${PROFILE:=weights}"
export PSCALE_HOME="${PSCALE_HOME:-$PWD/pscale-home}"
mkdir -p "$PSCALE_HOME/namespaces"

ENDPOINT="${R2_ENDPOINT:-https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com}"

$PSCALE namespace create "$PSCALE_NS_R2" --profile "$PROFILE" || true
# The store is R2 itself. Credentials are read from the environment by
# petascale-store's R2Store, not written into this file.
cat > "$PSCALE_HOME/namespaces/$PSCALE_NS_R2.toml" <<TOML
name = "$PSCALE_NS_R2"
visibility = "private"
chunk_profile = "$PROFILE"
store = { backend = "r2", endpoint = "$ENDPOINT", bucket = "$R2_BUCKET", region = "auto" }
TOML

# Chunk the tree and upload the chunks into R2 (SigV4, client-side).
$PSCALE backup --namespace "$PSCALE_NS_R2" --name bench-tree-r2 "$SRC"
echo
echo "PSCALE_NS_R2=$PSCALE_NS_R2"
echo "PSCALE_SNAPSHOT_R2=bench-tree-r2"
