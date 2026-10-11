# AWS-RunShellScript runs each command under /bin/sh, which may be dash: one bash -c carries the pipefail.
"set -euo pipefail; mkdir -p /root/tree && cd /root/tree; aws s3 cp s3://\($b)/round-ami/\($r).tgz - | tar -xz; bash round-vm/ami-setup.sh /root/tree \($t) /root/.cargo"
| {commands: ["bash -c " + @sh "\(.)"]}
