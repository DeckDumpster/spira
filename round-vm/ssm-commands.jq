# AWS-RunShellScript runs each command under /bin/sh, which may be dash: one bash -c carries the pipefail.
# The stock image has no aws CLI: the tree arrives by presigned URL ($u) and curl.
# SSM keeps the head of a long output and drops the rest, so the setup logs to a file and only the tail of a failure is printed.
"set -euo pipefail; mkdir -p /root/tree && cd /root/tree; code=$(curl -sS -o /root/tree.tgz -w '%{http_code}' \($u|@sh)); [ \"$code\" = 200 ] && [ -s /root/tree.tgz ] || { echo \"tree download failed: http $code\" >&2; exit 1; }; tar -xzf /root/tree.tgz; bash -x round-vm/ami-setup.sh /root/tree \($t) /root/.cargo" as $inner
| "L=/root/setup.log; bash -c \($inner|@sh) >\"$L\" 2>&1; rc=$?; if [ $rc -ne 0 ]; then echo \"round image setup failed rc=$rc; tail of $L:\" >&2; tail -n 60 \"$L\" | cut -c1-300 >&2; fi; exit $rc"
| {commands: ["bash -c " + @sh "\(.)"]}
