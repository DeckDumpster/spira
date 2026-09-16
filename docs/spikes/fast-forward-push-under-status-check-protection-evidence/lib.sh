W=$(git rev-parse --show-toplevel)
R=OWNER/REPO
S=$(cat $W/.runtime/spike/stamp)
P=spira/spike-protect-$S
LOG=$W/.runtime/spike/log.md
export GIT_INDEX_FILE=$W/.runtime/spike/index
run() { { printf '\n```console\n$ %s\n' "$*"; out=$(eval "$@" 2>&1); rc=$?; printf '%s\n[exit %s]\n```\n' "$out" "$rc"; } | tee -a "$LOG"; }
note() { printf '\n%s\n' "$*" | tee -a "$LOG"; }
g() { git -C $W "$@"; }
# mkcommit <verdict> <parent|-> <msg>
mkcommit() {
  local wf blob tree
  wf=$(g hash-object -w $W/.runtime/spike/spike-gate.yml)
  blob=$(printf '%s\n' "$1" | g hash-object -w --stdin)
  rm -f $GIT_INDEX_FILE
  g update-index --add --cacheinfo 100644,$wf,.github/workflows/spike-gate.yml
  g update-index --add --cacheinfo 100644,$blob,verdict
  tree=$(g write-tree)
  if [ "$2" = - ]; then echo "$3" | g commit-tree $tree; else echo "$3" | g commit-tree $tree -p $2; fi
}
