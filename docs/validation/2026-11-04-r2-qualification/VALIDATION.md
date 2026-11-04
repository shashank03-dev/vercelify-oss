# Live Cloudflare R2 encrypted backup and independent recovery

R2 was enabled by the user. Existing authenticated cf CLI access was reused;
no account/login/payment credentials were requested or recorded. An exclusively
owned private bucket was created, with an object read/write credential restricted
to that bucket and expiring after two hours. No public bucket access was configured.

The retained encrypted synthetic physical-x86 PostgreSQL17 backup was copied to
an encrypted restic repository on the actual R2 S3 endpoint. The original source
database and source volume had already been deleted. A complete remote repository
check passed. No production PostgreSQL15 backup or host master key was uploaded.

The product's restore-drill import API ran on the independent ARM Linux host in
an isolated nested daemon, with a fresh SQLite control store/master key. It used
explicit independently retained recovery/password/provider credentials. Successful
recovery performed full data checking and asserted 128 rows, migration db1072b,
and the known id=128 value final-128. No source database or original host control
store was used. The destination's managed database inventory remained empty.

## Negative and cleanup checks

- Wrong recovery key failed; empty recovery key was rejected with HTTP400.
- Wrong provider secret failed without marking the recovery verified.
- Immediate cancellation reached cancelled with cleanup_pending false.
- One owned R2 ciphertext pack was modified; product recovery failed integrity
  checking with no verification timestamp. Exact original bytes were restored in
  finally cleanup, and full repository integrity passed again afterward.
- No restore-drill containers remained. The owned agent and nested daemon stopped.
- Six owned objects (3036 bytes) were removed. Valid bucket access was confirmed
  before revocation; the revoked credential returned HTTP401 afterward. The owned
  bucket was deleted, and other bucket names matched the pre-test inventory.
- Local disposable credential/source-key copies were removed. Original independently
  retained backup material and the user's cf OAuth login were preserved.

A first request immediately after token creation returned401; a later retry with
that same credential succeeded. Token propagation is consistent with this result,
but was not independently isolated. The retained archive supplied a short snapshot
ID: the product rejected it, and the test supplied the complete actual R2 snapshot
ID after restic copy. Neither event was converted into an application fix.

## Limits

This qualifies R2 S3/restic behavior and PG17 recovery for this small synthetic
fixture. It does not configure ongoing production/off-site backups, qualify other
cloud providers, support additional database engines/majors, establish large-data
throughput, or provide peak RSS measurements. The outer daemon had a 1GiB cap;
141.5MiB was a point observation, not a peak. Existing physical power, public guest
lifecycle and router qualification gaps are unchanged by this test.

See summary.json, recovery.txt, corruption.txt and cleanup.json. Source scripts
expect disposable credentials/artifacts under ignored target/r2-qualification;
these are not included in Git. Credentials must never be added to this directory.

Reference: https://developers.cloudflare.com/r2/api/tokens/.
