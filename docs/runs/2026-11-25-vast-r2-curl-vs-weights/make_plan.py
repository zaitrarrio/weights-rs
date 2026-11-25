import os, sys
# R2_PY: h3-rs scripts/vast/r2.py (an S3 client from the Cloudflare env vars)
exec(open(os.environ.get("R2_PY", "r2.py")).read())
TREES = ["weights/fastwan21-1.3b/25e7ed7f41fd8ce2fdd108688c65e8caf0ce3aef/", "weights/h3-base/text_encoder_fp8/"]
out = []
for src in TREES:
    name = src.rstrip("/").split("/")[1] + ("-te8" if "text_encoder" in src else "")
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket="fv-weights", Prefix=src):
        for o in page.get("Contents", []):
            url = s3.generate_presigned_url("get_object", Params={"Bucket": "fv-weights", "Key": o["Key"]}, ExpiresIn=6 * 3600)
            out.append(f"{url} {o['Size']} {name}/{o['Key'][len(src):]}")
open(sys.argv[1], "w").write("\n".join(out) + "\n")
print(len(out), "files", sum(int(l.split()[1]) for l in out) / 1e9, "GB", file=sys.stderr)
