#!/usr/bin/env python3
"""Plans for the four-origin benchmark. A plan line is `URL SIZE REL`.

Every arm must move the SAME bytes to the SAME relative paths, so the
HF repo tree is the definition of the workload and R2 / petascale are
mirrors of it. REL is always the repo-relative path.

  hf-plan    REPO REV OUT            -> presigned-free HF resolve URLs
  r2-plan    BUCKET PREFIX OUT       -> presigned R2 GETs (needs creds)
  mirror-r2  DIR BUCKET PREFIX       -> upload a local tree to R2
  sizes      REPO REV                -> file list and total, no plan

R2 credentials come from the environment the same way h3-rs's
scripts/vast/r2.py takes them: R2_ACCOUNT_ID plus
R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY (AWS_* also accepted).
R2_ENDPOINT overrides the account endpoint (used to test against a mock).
"""
import json, os, sys, urllib.request

HF = "https://huggingface.co"


def hf_files(repo, rev):
    """(path, size) for every file in the repo at rev, largest first."""
    url = f"{HF}/api/models/{repo}?blobs=true&revision={rev}"
    req = urllib.request.Request(url)
    tok = os.environ.get("HF_TOKEN")
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=60) as r:
        d = json.load(r)
    fs = [(f["rfilename"], f.get("size") or 0) for f in d.get("siblings", [])]
    return sorted(fs, key=lambda t: -t[1])


def r2_client():
    import boto3  # only the R2 arms need it
    from botocore.config import Config

    key = os.environ.get("R2_ACCESS_KEY_ID") or os.environ["AWS_ACCESS_KEY_ID"]
    sec = os.environ.get("R2_SECRET_ACCESS_KEY") or os.environ["AWS_SECRET_ACCESS_KEY"]
    # R2_ENDPOINT overrides the account endpoint, so the plan and mirror
    # paths can be exercised against a local S3 mock with no credentials.
    endpoint = os.environ.get("R2_ENDPOINT")
    if not endpoint:
        endpoint = f"https://{os.environ['R2_ACCOUNT_ID']}.r2.cloudflarestorage.com"
    return boto3.client(
        "s3",
        endpoint_url=endpoint,
        aws_access_key_id=key,
        aws_secret_access_key=sec,
        region_name="auto",
        config=Config(signature_version="s3v4", retries={"max_attempts": 5}),
    )


def main():
    cmd = sys.argv[1]
    if cmd == "sizes":
        repo, rev = sys.argv[2], sys.argv[3]
        fs = hf_files(repo, rev)
        for p, s in fs:
            print(f"{s:14d}  {p}")
        print(f"{len(fs)} files, {sum(s for _, s in fs) / 1e9:.2f} GB", file=sys.stderr)

    elif cmd == "hf-plan":
        repo, rev, out = sys.argv[2], sys.argv[3], sys.argv[4]
        fs = hf_files(repo, rev)
        lines = [f"{HF}/{repo}/resolve/{rev}/{p} {s} {p}" for p, s in fs]
        open(out, "w").write("\n".join(lines) + "\n")
        print(f"{len(fs)} files, {sum(s for _, s in fs) / 1e9:.2f} GB -> {out}", file=sys.stderr)

    elif cmd == "r2-plan":
        bucket, prefix, out = sys.argv[2], sys.argv[3], sys.argv[4]
        s3 = r2_client()
        lines, total = [], 0
        for page in s3.get_paginator("list_objects_v2").paginate(Bucket=bucket, Prefix=prefix):
            for o in page.get("Contents", []):
                rel = o["Key"][len(prefix):].lstrip("/")
                if not rel:
                    continue
                url = s3.generate_presigned_url(
                    "get_object",
                    Params={"Bucket": bucket, "Key": o["Key"]},
                    ExpiresIn=6 * 3600,
                )
                lines.append(f"{url} {o['Size']} {rel}")
                total += o["Size"]
        open(out, "w").write("\n".join(lines) + "\n")
        print(f"{len(lines)} files, {total / 1e9:.2f} GB -> {out}", file=sys.stderr)

    elif cmd == "mirror-r2":
        d, bucket, prefix = sys.argv[2], sys.argv[3], sys.argv[4]
        s3 = r2_client()
        n = 0
        for root, _, files in os.walk(d):
            for f in files:
                p = os.path.join(root, f)
                rel = os.path.relpath(p, d)
                s3.upload_file(p, bucket, f"{prefix.rstrip('/')}/{rel}")
                n += 1
                print(f"up {rel}", file=sys.stderr)
        print(f"{n} files -> s3://{bucket}/{prefix}", file=sys.stderr)

    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
