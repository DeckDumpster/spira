# AWS-RunShellScript runs each command under /bin/sh, which may be dash: one bash -c carries the pipefail.
# The stock image has no aws CLI: the tree arrives by presigned URL ($u) and curl.
"set -euo pipefail; mkdir -p /root/tree && cd /root/tree; code=$(curl -sS -o /root/tree.tgz -w '%{http_code}' \($u|@sh)); [ \"$code\" = 200 ] && [ -s /root/tree.tgz ] || { echo \"tree download failed: http $code\" >&2; exit 1; }; tar -xzf /root/tree.tgz; bash round-vm/ami-setup.sh /root/tree \($t) /root/.cargo"
| {commands: ["bash -c " + @sh "\(.)"]}
