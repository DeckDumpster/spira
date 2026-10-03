#!/usr/bin/env bash
#
# archive.sh — keep every session transcript, indexed by time range and by lineage.
#
#   archive.sh sweep [--force] [path ...]
#                                    archive what has changed and rewrite the index
#   archive.sh hook                  a session-end hook payload on stdin; archive that one
#   archive.sh query [filters]       index rows matching a time window, a lineage, a slug
#   archive.sh lineage <session-id>  every transcript in that session's chain, oldest first
#   archive.sh restore <id|path>     the original bytes, on stdout, hash-checked
#   archive.sh verify [id|path ...]  re-hash archived bodies against the index
#   archive.sh where                 the archive root and what is in it
#
# WHY THIS EXISTS. The client writes each session to a transcript under its own directory,
# outside every repository, unversioned, on whatever volume the home directory sits on, with
# no retention promise to anyone. That file is the only record of every decision taken in
# conversation that never became a bead — which is precisely the material nothing else here
# keeps. This copies it somewhere durable and writes an index that can be queried, so a
# later "what did we decide that afternoon" is a command rather than an archaeology project.
#
# THE LINEAGE, AND WHY IT IS THE INDEX'S REASON FOR EXISTING. Clearing the context starts a
# NEW transcript with a new session id, so "the current session log" is only ever the tail of
# the conversation. The client records a stable `bridgeSessionId` in the first few lines of
# every file in a lineage, unchanged across those clears — so one field turns a chain of
# files into one queryable conversation. Index it and a lineage is a command; miss it and
# every consumer downstream re-implements the same guess, differently.
#
# THE BODIES NEVER GO IN A SHARED REPOSITORY. A transcript carries paths, credentials read
# aloud, and everything anyone ever said in it. The archive lives outside every checkout by
# default, and the runtime directory it defaults into is gitignored for the same reason a
# beads database is (law-beads-is-never-public). If something derived is ever wanted in git,
# it is the INDEX — metadata, no message content — and even then only where a reader can see
# it is derived.
#
# RETENTION IS A DECISION, NOT A DEFAULT. Nothing here deletes anything, ever. When the
# volume eventually says otherwise that is the operator's call, and the index is what makes
# it answerable rather than a guess: bytes per lineage, per month, per project directory.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/conf.sh"

MODE="${1:-}"; shift 2>/dev/null || true
case "$MODE" in
    sweep|hook|query|lineage|restore|verify|where) ;;
    ""|-h|--help|help)
        sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'
        exit 0 ;;
    *)  echo "archive: unknown subcommand '$MODE' — try --help" >&2; exit 2 ;;
esac

spira_require python3 || exit 1
# The compressor is resolved here rather than in the worker, so a box without either is told
# which programs would do and not left with a traceback from a subprocess call.
COMPRESS=""
for c in zstd gzip; do command -v "$c" >/dev/null 2>&1 && { COMPRESS="$c"; break; }; done
[ -n "$COMPRESS" ] || { echo "archive: neither zstd nor gzip is on PATH — one of them stores the bodies" >&2; exit 1; }

mkdir -p "$SPIRA_ARCHIVE" 2>/dev/null || {
    echo "archive: cannot create $SPIRA_ARCHIVE — set SPIRA_ARCHIVE in ${SPIRA_CONF_FILE:-spira.conf}" >&2
    exit 1; }

# THE LOCK, AND THE METER THAT SAYS WHEN IT STOPS BEING ENOUGH. Two writers exist by design —
# the timer and the session-end hook — and they can fire in the same second, on the same
# growing file, into one index. A whole-run flock is the simple correct answer rather than
# per-row merging, so it is what ships; what ships WITH it is the wait, reported whenever it
# is non-zero, so serialisation is seen in the summary before it is felt as a slow hook
# (law-take-the-simple-fix-with-a-meter). Readers take no lock: the index is replaced
# atomically, so a reader sees one whole version or the other.
WAITED=0
case "$MODE" in
sweep|hook)
    spira_require flock || exit 1
    exec 9>"$SPIRA_ARCHIVE/.lock"
    t0=$(date +%s)
    if ! flock -w 900 9; then
        echo "archive: another sweep has held the lock for 15 minutes — not starting a second" >&2
        exit 1
    fi
    WAITED=$(( $(date +%s) - t0 ))
    ;;
esac

# THE HOOK PAYLOAD IS READ HERE AND HANDED ON IN THE ENVIRONMENT, because the worker below
# IS stdin — `python3 -` reads its program from there — so a hook that tried to read its own
# payload inside it would find the stream already consumed and report an empty object, which
# looks exactly like a client that sent nothing.
PAYLOAD=""
[ "$MODE" = hook ] && PAYLOAD="$(cat)"

SPIRA_HOOK_PAYLOAD="$PAYLOAD" \
python3 - "$MODE" "$SPIRA_ARCHIVE" "$SPIRA_TOKEN_PROJECTS" "$COMPRESS" "$WAITED" "$@" <<'PY'
import fnmatch, hashlib, json, os, re, subprocess, sys, datetime as dt

MODE, ARCHIVE, PROJECTS, COMPRESS, WAITED = sys.argv[1:6]
ARGS   = sys.argv[6:]
BODIES = os.path.join(ARCHIVE, "bodies")
INDEX  = os.path.join(ARCHIVE, "index.jsonl")
EXT    = ".zst" if COMPRESS == "zstd" else ".gz"

def die(msg, code=1):
    sys.stderr.write("archive: %s\n" % msg); raise SystemExit(code)

# ---- reading a transcript -----------------------------------------------------------------
# BY REGEX OVER BYTES, NOT json.loads PER LINE. The corpus is gigabytes and every changed file
# is read in full to hash it anyway; parsing each line as JSON to reach three fields costs
# minutes per pass and buys nothing, because the fields wanted are flat string values.
RE_TS      = re.compile(rb'"timestamp"\s*:\s*"([^"]{10,40})"')
RE_ASST    = re.compile(rb'"type"\s*:\s*"assistant"')
RE_BRIDGE  = re.compile(rb'"bridgeSessionId"\s*:\s*"([^"]{1,120})"')
RE_SESSION = re.compile(rb'"sessionId"\s*:\s*"([^"]{1,120})"')
RE_PARENT  = re.compile(rb'"parentSessionId"\s*:\s*"([^"]{1,120})"')

def parse_ts(s):
    """An ISO-8601 instant -> an aware UTC datetime, or None."""
    s = (s or "").strip()
    if s.endswith(("Z", "z")): s = s[:-1] + "+00:00"
    try: d = dt.datetime.fromisoformat(s)
    except ValueError: return None
    if d.tzinfo is None: d = d.replace(tzinfo=dt.timezone.utc)
    return d.astimezone(dt.timezone.utc)

def fmt_ts(d): return d.strftime("%Y-%m-%dT%H:%M:%SZ")

# EVERY STORED TIMESTAMP IS CANONICALISED to whole seconds in UTC on the way in. The client
# writes fractional seconds, and a fraction sorts BEFORE the same instant without one under a
# string comparison — so an index that stored them raw would compare correctly almost always,
# which is the worst kind of almost. Canonical in, string compare out, one format everywhere.

def scan(path):
    """-> (sha256, first_ts, last_ts, turns, bridge, session_id) for one transcript.

    One pass, hashing as it goes: the file is read once whether the answer is the digest or
    the metadata, and reading a 40MB transcript twice per sweep is the difference between a
    timer nobody notices and one that shows up in the load average."""
    h = hashlib.sha256()
    first = last = None; turns = 0; bridge = ""; sess = ""; parent = ""
    with open(path, "rb") as fh:
        for ln in fh:
            h.update(ln)
            if RE_ASST.search(ln): turns += 1
            m = RE_TS.search(ln)
            if m:
                t = m.group(1)
                if first is None or t < first: first = t
                if last is None or t > last: last = t
            if not bridge:
                m = RE_BRIDGE.search(ln)
                if m: bridge = m.group(1).decode("utf-8", "replace")
            if not sess:
                m = RE_SESSION.search(ln)
                if m: sess = m.group(1).decode("utf-8", "replace")
            if not parent:
                m = RE_PARENT.search(ln)
                if m: parent = m.group(1).decode("utf-8", "replace")
    return (h.hexdigest(),
            first.decode() if first else None,
            last.decode() if last else None,
            turns, bridge, sess or parent)

def relpath_of(src):
    """The path a body is filed under, relative to the projects root.

    Kept whole rather than flattened to a session id, because subagent transcripts live at
    <slug>/<session>/subagents/<agent>.jsonl and two of them share no id at all. The relative
    path is the only key that is unique for every file the client writes."""
    return os.path.relpath(os.path.abspath(src), PROJECTS)

def snapshot(src, size, dest):
    """The first `size` bytes of a transcript, copied aside. -> bytes copied.

    A live transcript grows between being hashed and being compressed; hashing and storing one
    frozen copy keeps the digest the digest of the stored body, and the compressor can never
    see the file change under it."""
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    left = size
    with open(src, "rb") as fi, open(dest, "wb") as fo:
        while left > 0:
            chunk = fi.read(min(left, 1 << 20))
            if not chunk: break
            fo.write(chunk); left -= len(chunk)
    return size - left

def row_for(src, st, snap):
    rel = relpath_of(src)
    sha, first, last, turns, bridge, sess = scan(snap)
    f, l = parse_ts(first), parse_ts(last)
    src_kind = "transcript"
    if f is None or l is None:
        # NO TIMESTAMP IN THE FILE IS NOT NO TIME. Falling back to the file's own mtime keeps
        # the row sortable and findable by a window, and `ts_source` says which it is, so a
        # reader can tell a measured range from an inferred one instead of guessing.
        m = dt.datetime.fromtimestamp(st.st_mtime, dt.timezone.utc)
        f = f or m; l = l or m; src_kind = "mtime"
    return {
        "session_id": sess or os.path.splitext(os.path.basename(src))[0],
        "bridge_session_id": bridge,
        "cwd_slug": rel.split(os.sep)[0],
        "first_ts": fmt_ts(f), "last_ts": fmt_ts(l), "ts_source": src_kind,
        "turns": turns, "bytes": os.path.getsize(snap), "mtime": int(st.st_mtime),
        "sha256": sha, "source": rel, "path": os.path.join("bodies", rel + EXT),
        "archived_at": fmt_ts(dt.datetime.now(dt.timezone.utc)),
    }

def compress(src, dest):
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    tmp = dest + ".part"
    cmd = ["zstd", "-q", "-c", src] if COMPRESS == "zstd" else ["gzip", "-n", "-q", "-c", src]
    with open(tmp, "wb") as out:
        if subprocess.call(cmd, stdout=out) != 0:
            os.unlink(tmp); die("%s failed on %s" % (COMPRESS, src))
    os.replace(tmp, dest)          # atomic: a reader sees the old body or the new one
    # A GROWING TRANSCRIPT REPLACES, IT DOES NOT ACCUMULATE — and so does one whose archive
    # was written by the other compressor, which is what happens when zstd appears on a box
    # that had only gzip. Without this the old body lingers under the other extension and the
    # archive quietly holds two answers for one file.
    for other in (".zst", ".gz"):
        stale = dest[:-len(EXT)] + other
        if other != EXT and os.path.exists(stale):
            try: os.unlink(stale)
            except OSError: pass

def decompress_stream(body):
    cmd = ["zstd", "-dcq", body] if body.endswith(".zst") else ["gzip", "-dc", body]
    return subprocess.Popen(cmd, stdout=subprocess.PIPE)

# ---- the index ----------------------------------------------------------------------------
def load_index(required=True):
    if not os.path.exists(INDEX):
        if required:
            die("no index at %s — run `archive.sh sweep` first" % INDEX, 2)
        return []
    rows = []
    with open(INDEX, errors="replace") as fh:
        for ln in fh:
            ln = ln.strip()
            if not ln: continue
            try: rows.append(json.loads(ln))
            except ValueError: continue
    return rows

def write_index(rows):
    """Sorted by first_ts and rewritten whole; replaced only when the bytes differ -> True.

    NOT REWRITTEN ON EVERY PASS. A sweep that finds nothing changed must touch nothing at
    all, or "has anything happened since?" is unanswerable from the archive itself and every
    watcher over it reads a fresh mtime as news."""
    # THE BRIDGE ID IS INHERITED, and recomputed over the whole index every time rather than
    # patched. A subagent transcript records its parent session and no bridge id of its own,
    # so without this join it is invisible to the lineage query that is the point of the
    # index — present in the archive, absent from every answer about it.
    bridge = {r["session_id"]: r["bridge_session_id"]
              for r in rows if r.get("bridge_session_id")}
    for r in rows:
        if not r.get("bridge_session_id"):
            r["bridge_session_id"] = bridge.get(r["session_id"], "")
    rows.sort(key=lambda r: (r["first_ts"], r["source"]))
    body = "".join(json.dumps(r, sort_keys=True) + "\n" for r in rows)
    old = None
    if os.path.exists(INDEX):
        with open(INDEX, "rb") as fh: old = fh.read()
    if old == body.encode():
        return False
    tmp = INDEX + ".part"
    with open(tmp, "w") as fh: fh.write(body)
    os.replace(tmp, INDEX)
    return True

def sources():
    """Every transcript under the projects root, deepest included."""
    out = []
    for dirpath, _dirs, files in os.walk(PROJECTS):
        for f in files:
            if f.endswith(".jsonl"): out.append(os.path.join(dirpath, f))
    return sorted(out)

# ---- formatting ---------------------------------------------------------------------------
def emit(rows, as_json):
    if as_json:
        for r in rows: print(json.dumps(r, sort_keys=True))
        return
    for r in rows:
        print("%-21s %-21s %6d %12d  %-36s %-28s %s" % (
            r["first_ts"], r["last_ts"], r["turns"], r["bytes"],
            r["session_id"], r["bridge_session_id"] or "-", r["source"]))
    tot = sum(r["bytes"] for r in rows)
    sys.stderr.write("archive: %d row(s), %d byte(s) of transcript\n" % (len(rows), tot))

def find(rows, want):
    """Rows named by a session id or by a source path. Never an empty answer in silence."""
    hit = [r for r in rows if r["session_id"] == want or r["source"] == want
           or os.path.basename(r["source"]) == want]
    if not hit:
        die("nothing archived under '%s' — `archive.sh query` lists what there is" % want, 2)
    return hit

# ---- sweep --------------------------------------------------------------------------------
if MODE in ("sweep", "hook"):
    if MODE == "hook":
        # THE SESSION-END PAYLOAD, on stdin. Only the one transcript is touched, because this
        # runs while a human waits for their shell back.
        raw = os.environ.get("SPIRA_HOOK_PAYLOAD", "").strip()
        if not raw: die("no hook payload on stdin", 2)
        try: payload = json.loads(raw)
        except ValueError: die("the hook payload on stdin is not JSON", 2)
        if not isinstance(payload, dict): die("the hook payload on stdin is not an object", 2)
        p = payload.get("transcript_path") or ""
        if not p and payload.get("session_id"):
            m = [s for s in sources()
                 if os.path.splitext(os.path.basename(s))[0] == payload["session_id"]]
            p = m[0] if m else ""
        if not p:
            die("hook payload carries neither transcript_path nor a locatable session_id", 2)
        todo = [p]
    elif [a for a in ARGS if not a.startswith("-")]:
        todo = [a for a in ARGS if not a.startswith("-")]
    else:
        if not os.path.isdir(PROJECTS):
            # A MISSING SOURCE DIRECTORY IS NOT AN EMPTY ONE. Reported as "0 archived" it reads
            # as "nothing new", which is the reading that stops anyone looking
            # (law-absence-needs-a-positive-control).
            #
            # EXCEPT THE DEFAULT, BEFORE ANY SESSION HAS RUN. Claude creates ~/.claude/projects
            # with its first session, so on a freshly installed box the default path does not
            # exist yet; refusing there failed spira-archive on its first tick and every
            # deploy's health check refused on it (local acceptance phase B, 2026-09-26). Said
            # and exit 0 for the default only; a path someone configured still refuses.
            if os.path.abspath(PROJECTS) == os.path.abspath(os.path.join(os.path.expanduser("~"), ".claude", "projects")):
                sys.stderr.write("archive: %s does not exist yet — no Claude session has run on this box, nothing to archive\n" % PROJECTS)
                sys.exit(0)
            die("transcript directory %s does not exist — set SPIRA_TOKEN_PROJECTS" % PROJECTS)
        todo = sources()
        if not todo:
            sys.stderr.write("archive: no *.jsonl transcripts under %s\n" % PROJECTS)

    # `--force` IS THE REMEDY FOR WHAT `verify` FINDS. The fast path trusts the SOURCE's size
    # and mtime, so a body that rotted or was overwritten under an unchanged source is invisible
    # to an ordinary sweep — correct, and cheap, and no use at all once verify has gone red.
    force = "--force" in ARGS
    for a in ARGS:
        if a.startswith("-") and a != "--force": die("sweep: unknown argument '%s'" % a, 2)
    rows = load_index(required=False)
    by_rel = {r["source"]: r for r in rows}
    stored = kept = skipped = 0
    for src in todo:
        try: st = os.stat(src)
        except OSError:
            sys.stderr.write("archive: cannot read %s — skipped\n" % src); skipped += 1; continue
        if not os.path.isfile(src): skipped += 1; continue
        rel  = relpath_of(src)
        if rel.startswith(".."):
            sys.stderr.write("archive: %s is outside %s — skipped\n" % (src, PROJECTS))
            skipped += 1; continue
        prev = by_rel.get(rel)
        # THE FAST PATH IS SIZE AND MTIME, and the archive keeps the digest of what it stored.
        # An append changes both, so an unchanged file is recognised without re-reading
        # gigabytes on a timer — instrumentation that costs more than what it measures is its
        # own bug — while `verify` remains the answer to "is the stored copy still sound".
        if (not force
                and prev and prev.get("bytes") == st.st_size and prev.get("mtime") == int(st.st_mtime)
                and os.path.exists(os.path.join(ARCHIVE, prev["path"]))):
            kept += 1; continue
        snap = os.path.join(ARCHIVE, "bodies", rel + ".snap")
        try:
            snapshot(src, st.st_size, snap)
            row = row_for(src, st, snap)
            compress(snap, os.path.join(ARCHIVE, row["path"]))
        finally:
            try: os.unlink(snap)
            except OSError: pass
        by_rel[rel] = row
        stored += 1
    # ROWS WHOSE SOURCE IS GONE ARE KEPT. The client deleting a transcript is the loss this
    # exists to survive; an index that forgot it would make the archive agree with whatever
    # remains rather than with what was said.
    changed = write_index(list(by_rel.values()))
    if int(WAITED) > 0:
        sys.stderr.write("archive: waited %ss for the sweep lock\n" % WAITED)
    print("archive: %d stored, %d unchanged%s, %d row(s) in the index, %s"
          % (stored, kept, (", %d skipped" % skipped) if skipped else "",
             len(by_rel), "index rewritten" if changed else "index untouched"))
    raise SystemExit(0)

# ---- query --------------------------------------------------------------------------------
if MODE == "query":
    since = until = lineage = slug = None; as_json = False
    it = iter(ARGS)
    for a in it:
        if   a == "--since":   since = next(it, None)
        elif a == "--until":   until = next(it, None)
        elif a == "--lineage": lineage = next(it, None)
        elif a == "--slug":    slug = next(it, None)
        elif a == "--json":    as_json = True
        else: die("query: unknown argument '%s'" % a, 2)
    def bound(s, end):
        if s is None: return None
        # A BARE DATE IS A WHOLE DAY, and which end of it depends on which bound it is:
        # `--until <date>` read as midnight excludes the entire day that was asked for. This
        # is tested BEFORE the general parse rather than as its fallback, because
        # fromisoformat accepts a bare date perfectly well and answers midnight.
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}", s or ""):
            s = s + ("T23:59:59Z" if end else "T00:00:00Z")
        d = parse_ts(s)
        if d is None: die("not an ISO-8601 instant: %s" % s, 2)
        return fmt_ts(d)
    lo, hi = bound(since, False), bound(until, True)
    out = []
    for r in load_index():
        # OVERLAP, NOT CONTAINMENT. A session spanning the window is the one most worth
        # finding, and a containment test is exactly the one that would drop it.
        if lo and r["last_ts"]  < lo: continue
        if hi and r["first_ts"] > hi: continue
        if lineage and r["bridge_session_id"] != lineage: continue
        if slug and not fnmatch.fnmatch(r["cwd_slug"], slug): continue
        out.append(r)
    emit(out, as_json)
    raise SystemExit(0)

# ---- lineage ------------------------------------------------------------------------------
if MODE == "lineage":
    as_json = "--json" in ARGS
    want = [a for a in ARGS if not a.startswith("-")]
    if len(want) != 1: die("lineage: name exactly one session id", 2)
    rows = load_index()
    seed = find(rows, want[0])[0]
    bid  = seed["bridge_session_id"]
    if not bid:
        sys.stderr.write("archive: %s records no lineage id — a chain of one\n" % seed["session_id"])
        emit([seed], as_json); raise SystemExit(0)
    chain = sorted([r for r in rows if r["bridge_session_id"] == bid],
                   key=lambda r: (r["first_ts"], r["source"]))
    emit(chain, as_json)
    raise SystemExit(0)

# ---- restore ------------------------------------------------------------------------------
if MODE == "restore":
    if len(ARGS) != 1: die("restore: name exactly one session id or archived path", 2)
    hit = find(load_index(), ARGS[0])
    if len(hit) > 1:
        sys.stderr.write("archive: '%s' names %d transcripts — restore one by path:\n"
                         % (ARGS[0], len(hit)))
        for r in hit: sys.stderr.write("    %s\n" % r["source"])
        raise SystemExit(2)
    r = hit[0]
    body = os.path.join(ARCHIVE, r["path"])
    if not os.path.exists(body): die("body missing from the archive: %s" % body, 3)
    h = hashlib.sha256(); p = decompress_stream(body)
    while True:
        chunk = p.stdout.read(1 << 20)
        if not chunk: break
        h.update(chunk); sys.stdout.buffer.write(chunk)
    p.stdout.close(); rc = p.wait(); sys.stdout.buffer.flush()
    if rc != 0: die("decompressing %s failed" % body, 3)
    # THE HASH IS OF THE ORIGINAL, checked on the way out rather than trusted. A restore that
    # silently returns 40MB of something else is the failure this whole store exists against.
    if h.hexdigest() != r["sha256"]:
        die("RESTORED BYTES DO NOT MATCH the recorded sha256 for %s" % r["source"], 3)
    raise SystemExit(0)

# ---- verify -------------------------------------------------------------------------------
if MODE == "verify":
    rows = load_index()
    if ARGS:
        want = []
        for a in ARGS: want.extend(find(rows, a))
        rows = want
    if not rows:
        die("the index is empty — refusing to report every body sound over no bodies", 2)
    bad = 0
    for r in rows:
        body = os.path.join(ARCHIVE, r["path"])
        if not os.path.exists(body):
            print("MISSING %s" % r["source"]); bad += 1; continue
        h = hashlib.sha256(); p = decompress_stream(body)
        while True:
            chunk = p.stdout.read(1 << 20)
            if not chunk: break
            h.update(chunk)
        p.stdout.close()
        if p.wait() != 0 or h.hexdigest() != r["sha256"]:
            print("FAIL    %s" % r["source"]); bad += 1
        else:
            print("ok      %s" % r["source"])
    sys.stderr.write("archive: %d of %d body(s) sound\n" % (len(rows) - bad, len(rows)))
    raise SystemExit(1 if bad else 0)

# ---- where --------------------------------------------------------------------------------
if MODE == "where":
    rows = load_index(required=False)
    total = sum(r["bytes"] for r in rows)
    on_disk = 0
    for dirpath, _d, files in os.walk(BODIES):
        for f in files:
            try: on_disk += os.path.getsize(os.path.join(dirpath, f))
            except OSError: pass
    print("root      %s" % ARCHIVE)
    print("index     %s (%d row(s))" % (INDEX, len(rows)))
    print("bodies    %s (%d byte(s) stored, %d byte(s) of transcript)" % (BODIES, on_disk, total))
    print("sources   %s" % PROJECTS)
    print("stored by %s" % COMPRESS)
    raise SystemExit(0)
PY
