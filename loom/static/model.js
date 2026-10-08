/* model.js — the whole view model, derived in the page from a raw bead array.
 *
 *   const model = LoomModel.derive(beads, {now: Date.now(), askLabel: 'needs-operator'});  // literal-ok: usage example, server overrides via meta
 *
 * THE SERVER RETURNS BEADS AND NOTHING ELSE. No coordinates, no connected components, no
 * layers, no histograms, no headline counts. Those all live here, because a surface whose
 * payload is pre-chewed has a rendering stack on the server and a second place for the two
 * to disagree — and because it was measured rather than assumed: the whole layout pass is
 * single-digit milliseconds at a few hundred beads and tens of milliseconds at twenty
 * thousand, in the browser. Component detection and bucketing are cheaper still.
 *
 * NO DOM AND NO FETCH IN THIS FILE. It is the half that can be tested without a browser, so
 * it must stay loadable under a bare JS runtime; app.js owns everything that touches a
 * document. That split is the reason there is a suite at all.
 *
 * `now` IS A PARAMETER, NEVER `Date.now()` READ TWICE. Every age on the page is measured
 * from one instant, so a render cannot show a bead as both 6 and 7 days cold depending on
 * which line computed it — and a fixture pinning `now` makes the derivation's output a
 * fixed value rather than one that drifts a day at midnight.
 */
(function (root, factory) {
    /* Both a browser global and a CommonJS module, deliberately: the page loads it with a
       plain <script> and the suite loads the same file with require(). A build step that
       produced two artifacts would let the tested one and the shipped one diverge. */
    if (typeof module === 'object' && module.exports) module.exports = factory();
    else root.LoomModel = factory();
}(typeof self !== 'undefined' ? self : this, function () {
'use strict';

/* THE LABEL VOCABULARY IS CONFIGURATION, NOT A CONSTANT. Two of these are settable in the
   harness's config file, so a page with them written in shows fewer escalations and fewer
   parked beads on any installation that renamed them — and shows them as ordinary work,
   which is the reading that stops anybody looking. The defaults here match the harness's
   own defaults; a server that knows better passes `meta` and overrides them. */
var DEFAULTS = {
    askLabel: 'needs-operator',     /* SPIRA_ASK_LABEL — waiting on the operator — literal-ok: harness default, server overrides via meta */
    ciLabel: 'awaiting-ci',         /* SPIRA_CI_LABEL — parked on a run — literal-ok: harness default, server overrides via meta */
    poisonLabel: 'spira-poison',    /* set once a bead has failed `threshold` times */
    attemptPrefix: 'sp-attempt-',   /* sp-attempt-N; the count is the largest N */
    reclaimPrefix: 'sp-reclaim-',   /* sp-reclaim-N, and sp-reclaim-N-unrecorded */
    repoPrefix: 'repo:',            /* repo:<name> — which repository the work belongs to */
    threshold: 3,                   /* SPIRA_POISON_AT */
    noRepo: 'unmapped',             /* the bucket for a bead carrying no repo label */
    coldDays: 14,                   /* untouched for longer than this reads as cold */
    flowDays: 14
};

var DAY = 86400000, HOUR = 3600000;

function cfg(opts) {
    var c = {}, k;
    for (k in DEFAULTS) if (Object.prototype.hasOwnProperty.call(DEFAULTS, k)) c[k] = DEFAULTS[k];
    if (opts) for (k in opts) {
        /* An unrecognised key is ignored rather than merged in. A typo silently accepted is
           a setting the caller believes is in force, which is the same defect the harness's
           own config allowlist exists to prevent. */
        if (Object.prototype.hasOwnProperty.call(DEFAULTS, k) && opts[k] !== undefined &&
            opts[k] !== null && opts[k] !== '') c[k] = opts[k];
    }
    return c;
}

function ms(t) { var v = t ? Date.parse(t) : NaN; return isNaN(v) ? null : v; }

/* Days elapsed, floored — "touched 0d ago" means today. Floor rather than round, or a bead
   touched thirteen hours ago reads as yesterday's. */
function daysSince(then, now) {
    return then === null ? 0 : Math.max(0, Math.floor((now - then) / DAY));
}

/* The largest N over `<prefix>N` labels, tolerating a suffix after the number: the harness
   writes both `sp-reclaim-4` and `sp-reclaim-4-unrecorded` and they are the same reclaim
   recorded two ways. Largest, not count, because an attempt withdrawn removes its label and
   a count would then disagree with the number the poison check actually reads. */
function maxSuffix(labels, prefix) {
    var n = 0, i, m,
        re = new RegExp('^' + prefix.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '(\\d+)(?:-|$)');
    for (i = 0; i < labels.length; i++) {
        m = re.exec(labels[i]);
        if (m) { var v = parseInt(m[1], 10); if (v > n) n = v; }
    }
    return n;
}

function has(labels, want) { return labels.indexOf(want) !== -1; }

function utcDay(t) { return new Date(t).toISOString().slice(0, 10); }

/* ---------------------------------------------------------------------------------------
 * per bead
 * ------------------------------------------------------------------------------------- */
function normalise(raw, c, now) {
    var labels = raw.labels || [], repo = c.noRepo, i;
    for (i = 0; i < labels.length; i++)
        if (labels[i].indexOf(c.repoPrefix) === 0) { repo = labels[i].slice(c.repoPrefix.length); break; }
    var created = ms(raw.created_at), updated = ms(raw.updated_at);
    return {
        t: raw.title || raw.id,
        d: raw.description || '',
        repo: repo,
        s: raw.status || 'open',
        ty: raw.issue_type || 'task',
        p: typeof raw.priority === 'number' ? raw.priority : 3,
        ep: raw.parent || null,
        asg: raw.assignee || '',
        age: daysSince(updated, now),
        born: daysSince(created, now),
        createdMs: created,
        updatedMs: updated,
        nr: has(labels, c.askLabel),
        ci: has(labels, c.ciLabel),
        poi: has(labels, c.poisonLabel),
        holds: Array.isArray(raw.holds) ? raw.holds : [],
        att: maxSuffix(labels, c.attemptPrefix),
        rec: maxSuffix(labels, c.reclaimPrefix),
        up: [],   /* blocked by */
        dn: [],   /* unblocks   */
        lay: 0
    };
}

/* ---------------------------------------------------------------------------------------
 * edges
 * ------------------------------------------------------------------------------------- */
/* ONLY A BLOCKING DEPENDENCY IS AN EDGE. The tracker records several relation types on the
   same field, and the provenance ones — "discovered from", and its kin — are not order.
   Drawing them as dependency edges shows work as blocked that nothing is waiting on, which
   is the same wrong picture as encoding a priority sort as edges.
   An edge is written [blocker, blocked], which is also the direction the layering reads.
   A dependency naming a bead outside the live set is dropped: it is almost always a
   prerequisite that has already closed, and half an edge leading nowhere is worse than
   none. */
var BLOCKING = { blocks: 1, 'blocked-by': 1, hard: 1, depends: 1, 'depends-on': 1, '': 1 };

/* THE DEPENDENCY RECORDS ARRIVE ONE OF TWO WAYS, and both go through here. A tracker's own
   JSON carries them on each bead under `dependencies`; a server that has already lifted them
   out to avoid writing each one twice passes the same records as one flat array. Same shape,
   same parser — a second reader for the second arrangement is how the two come to disagree
   about which relations count. */
function edgesOf(records, beads) {
    var out = [], seen = {}, j, d, blocker, blocked, key;
    for (j = 0; j < records.length; j++) {
        d = records[j];
        if (!d || !BLOCKING[d.type || '']) continue;
        blocked = d.issue_id;
        blocker = d.depends_on_id;
        if (!blocker || !blocked || blocker === blocked) continue;
        if (!beads[blocker] || !beads[blocked]) continue;
        key = blocker + ' ' + blocked;
        if (seen[key]) continue;            /* the same edge is reachable from either end */
        seen[key] = 1;
        out.push([blocker, blocked]);
        beads[blocked].up.push(blocker);
        beads[blocker].dn.push(blocked);
    }
    return out;
}

/* Every dependency record in the payload, whichever arrangement it came in. A record on a
   bead that names no `issue_id` is that bead's own, which is how the tracker writes it. */
function records(rawBeads, provided) {
    if (provided && provided.length) return provided;
    var out = [], i, j, deps;
    for (i = 0; i < rawBeads.length; i++) {
        deps = rawBeads[i].dependencies || [];
        for (j = 0; j < deps.length; j++)
            out.push(deps[j].issue_id ? deps[j]
                     : { issue_id: rawBeads[i].id, depends_on_id: deps[j].depends_on_id,
                         type: deps[j].type });
    }
    return out;
}

/* ---------------------------------------------------------------------------------------
 * components and execution layers
 * ------------------------------------------------------------------------------------- */
/* A component is the connected set in the UNDIRECTED blocking graph — everything whose
   scheduling is entangled, whichever way the arrows point. A node's layer is its LONGEST
   path from a source, not its shortest: the layer is when the bead can start, and a bead
   with two prerequisites waits for the slower one. Depth is how many rounds the component
   takes at unlimited parallelism; width is how many workers its widest round can use.
   Width 1 over a long depth is the shape worth seeing — an epic capped at one worker
   however many are free — and it is invisible from any single bead in it. */
function components(ids, edges, beads) {
    var und = {}, i, a, b;
    for (i = 0; i < ids.length; i++) und[ids[i]] = [];
    for (i = 0; i < edges.length; i++) {
        a = edges[i][0]; b = edges[i][1];
        und[a].push(b); und[b].push(a);
    }

    /* Longest path from a source, computed iteratively over a topological order. Recursion
       here is not an academic worry: a thirty-deep chain is ordinary and nothing bounds a
       pathological one. */
    var indeg = {}, lay = {}, queue = [], id, dn;
    for (i = 0; i < ids.length; i++) { indeg[ids[i]] = beads[ids[i]].up.length; lay[ids[i]] = 0; }
    for (i = 0; i < ids.length; i++) if (indeg[ids[i]] === 0) queue.push(ids[i]);
    var head = 0, settled = 0;
    while (head < queue.length) {
        id = queue[head++]; settled++;
        dn = beads[id].dn;
        for (i = 0; i < dn.length; i++) {
            if (lay[dn[i]] < lay[id] + 1) lay[dn[i]] = lay[id] + 1;
            if (--indeg[dn[i]] === 0) queue.push(dn[i]);
        }
    }
    /* A cycle cannot be laid out by longest path, and it must not silently remove beads
       from the view: anything never settled keeps layer 0 and the flag is raised, so a
       cycle reads as a drawing that looks wrong rather than as beads that are not there. */
    var cyclic = settled < ids.length;

    var seen = {}, comps = [], n, stack, members, x, k, inComp;
    for (n = 0; n < ids.length; n++) {
        if (seen[ids[n]]) continue;
        stack = [ids[n]]; members = [];
        while (stack.length) {
            x = stack.pop();
            if (seen[x]) continue;
            seen[x] = 1; members.push(x);
            for (i = 0; i < und[x].length; i++) if (!seen[und[x][i]]) stack.push(und[x][i]);
        }
        /* Rows within a layer are a drawing order, not data. Sorting by id makes the picture
           stable across refreshes; unsorted, every row reshuffles on every poll, which reads
           as movement where nothing moved. */
        members.sort();
        inComp = {};
        for (i = 0; i < members.length; i++) inComp[members[i]] = 1;
        var rows = {}, nodes = [], depth = 0, width = 0;
        for (i = 0; i < members.length; i++) {
            k = lay[members[i]];
            if (rows[k] === undefined) rows[k] = 0;
            nodes.push({ id: members[i], l: k, r: rows[k]++ });
            beads[members[i]].lay = k;
            if (k + 1 > depth) depth = k + 1;
        }
        for (k in rows) if (rows[k] > width) width = rows[k];
        var ce = [];
        for (i = 0; i < edges.length; i++) if (inComp[edges[i][0]]) ce.push(edges[i]);
        comps.push({ size: members.length, depth: depth, width: width, nodes: nodes, edges: ce });
    }
    comps.sort(function (p, q) { return q.size - p.size || q.depth - p.depth; });
    return { components: comps, cyclic: cyclic };
}

/* ---------------------------------------------------------------------------------------
 * repository and epic grouping
 * ------------------------------------------------------------------------------------- */
/* AN EPIC'S KEY IS SCOPED BY REPOSITORY. One epic legitimately holds beads in several
   repositories, and the loose bucket exists once per repository by construction, so a bare
   parent id is not unique across the grouping. Keying on one made zooming a block open a
   different repository's block of the same name, and made two blocks' overflow counts
   collide in one slot. */
function group(beads, ids, c) {
    var byRepo = {}, i, b, id, key, order = [];
    for (i = 0; i < ids.length; i++) {
        id = ids[i]; b = beads[id];
        if (!byRepo[b.repo]) {
            byRepo[b.repo] = { repo: b.repo, n: 0, epics: {}, loose: [] };
            order.push(b.repo);
        }
        var g = byRepo[b.repo];
        g.n++;
        if (b.ep) {
            key = b.repo + '/' + b.ep;
            if (!g.epics[key]) g.epics[key] = { id: key, parent: b.ep, n: 0, beads: [] };
            g.epics[key].n++; g.epics[key].beads.push(id);
        } else g.loose.push(id);
    }
    var out = [];
    for (i = 0; i < order.length; i++) {
        var gg = byRepo[order[i]], eps = [], k;
        for (k in gg.epics) if (Object.prototype.hasOwnProperty.call(gg.epics, k)) {
            /* The parent may itself be closed, or belong to a repository this reader is not
               looking at, so its title is not always available. Fall back to the id: an
               epic named by its id is legible, an epic named "undefined" is a bug. */
            var e = gg.epics[k], parent = beads[e.parent];
            eps.push({ id: e.id, title: parent ? parent.t : e.parent, n: e.n, beads: e.beads });
        }
        eps.sort(function (p, q) { return q.n - p.n || (p.id < q.id ? -1 : 1); });
        /* The loose bucket goes LAST however large it is. It is usually the biggest block in
           the repository and the one with the least to say — sorted by size it would take
           the eye first on every repository. */
        if (gg.loose.length)
            eps.push({ id: gg.repo + '/loose', title: '(no epic)', n: gg.loose.length, beads: gg.loose });
        out.push({ repo: gg.repo, n: gg.n, epics: eps });
    }
    out.sort(function (p, q) { return q.n - p.n || (p.repo < q.repo ? -1 : 1); });
    return out;
}

/* ---------------------------------------------------------------------------------------
 * flow: arrivals, and how long the open work has been open
 * ------------------------------------------------------------------------------------- */
/* WHAT IS DERIVABLE HERE, AND WHAT IS NOT. The read path is bounded to work in flight, and
   that bound is the reason a per-request API is affordable at all — so no closed bead
   reaches this file, and completions per day and created-to-closed cycle time have no
   source in it. They are not approximated from what is here: a number computed over the
   wrong population is worse than an absent one, because it reads as the number it is named
   after. What live beads DO answer exactly is arrivals per day from `created_at`, and how
   long the work now open has been open. That is what this returns, under those names. The
   durable source for completions is an append-only event log of landings and closes. */
function flow(beads, ids, c, now) {
    var days = [], created = [], byrepo = {}, index = {}, i, k, d = new Date(now);
    var end = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());
    for (i = c.flowDays - 1; i >= 0; i--) {
        k = utcDay(end - i * DAY);
        index[k] = days.length; days.push(k); created.push(0);
    }
    for (i = 0; i < ids.length; i++) {
        var b = beads[ids[i]];
        if (b.createdMs === null) continue;
        var slot = index[utcDay(b.createdMs)];
        if (slot === undefined) continue;
        created[slot]++;
        if (!byrepo[b.repo]) {
            byrepo[b.repo] = [];
            for (k = 0; k < c.flowDays; k++) byrepo[b.repo].push(0);
        }
        byrepo[b.repo][slot]++;
    }
    return { days: days, created: created, byrepo: byrepo };
}

var FLIGHT = [
    ['<1d', DAY], ['1-3d', 3 * DAY], ['3-7d', 7 * DAY],
    ['1-2w', 14 * DAY], ['2-4w', 28 * DAY], ['>4w', Infinity]
];

/* How long a bead has been open so far, bucketed. This is NOT cycle time: every bead
   counted is still open, so the distribution is right-censored and every bucket is a lower
   bound. Naming it cycle time would make a number about unfinished work read as a number
   about finished work. */
function inFlight(beads, ids, now) {
    var out = {}, i, j, hours = [];
    for (i = 0; i < FLIGHT.length; i++) out[FLIGHT[i][0]] = 0;
    for (i = 0; i < ids.length; i++) {
        var b = beads[ids[i]];
        if (b.createdMs === null) continue;
        var age = Math.max(0, now - b.createdMs);
        hours.push(age / HOUR);
        for (j = 0; j < FLIGHT.length; j++) if (age < FLIGHT[j][1]) { out[FLIGHT[j][0]]++; break; }
    }
    hours.sort(function (p, q) { return p - q; });
    return { buckets: out, p50: hours.length ? hours[Math.floor((hours.length - 1) / 2)] : 0 };
}

/* ---------------------------------------------------------------------------------------
 * derive
 * ------------------------------------------------------------------------------------- */
function derive(rawBeads, opts) {
    var c = cfg(opts), now = (opts && opts.now) || Date.now();
    if (!Array.isArray(rawBeads)) throw new TypeError('LoomModel.derive expects an array of beads');

    /* A closed bead is dropped here rather than trusted not to arrive. The bound belongs to
       the read path, but a page that silently started painting history the day that
       endpoint widened would be wrong in a way nobody would think to look for. */
    var beads = {}, ids = [], i, raw;
    for (i = 0; i < rawBeads.length; i++) {
        raw = rawBeads[i];
        if (!raw || !raw.id || raw.status === 'closed') continue;
        if (beads[raw.id]) continue;
        beads[raw.id] = normalise(raw, c, now);
        ids.push(raw.id);
    }

    var edges = edgesOf(records(rawBeads, opts && opts.edges), beads), chained = [];
    for (i = 0; i < ids.length; i++)
        if (beads[ids[i]].up.length || beads[ids[i]].dn.length) chained.push(ids[i]);
    var comp = components(chained, edges, beads);

    var moving = 0, deferred = 0, cold = 0, blocked = 0, poison = 0, rejected = 0, churning = 0,
        waiting = 0, parked = 0, holds = { wait: 0, manual: 0, ask: 0, poison: 0 };
    for (i = 0; i < ids.length; i++) {
        var b = beads[ids[i]];
        if (b.s === 'in_progress') moving++;
        else if (b.s === 'deferred') deferred++;
        else if (b.s === 'blocked') blocked++;
        if (b.age > c.coldDays) cold++;
        if (b.poi) poison++;
        if (b.att >= c.threshold) rejected++;
        if (b.rec > 0) churning++;
        if (b.nr) waiting++;
        if (b.ci) parked++;
        for (var h = 0; h < b.holds.length; h++) if (b.holds[h] in holds) holds[b.holds[h]]++;
    }
    var fl = inFlight(beads, ids, now);

    return {
        generated: new Date(now).toISOString(),
        now: now,
        config: c,
        beads: beads,
        ids: ids,
        edges: edges,
        components: comp.components,
        cyclic: comp.cyclic,
        repos: group(beads, ids, c),
        flow: flow(beads, ids, c, now),
        flight: fl,
        stats: {
            live: ids.length, moving: moving, deferred: deferred, blocked: blocked,
            cold: cold, poison: poison, rejected: rejected, churning: churning,
            waiting: waiting, parked: parked, holds: holds,
            chained: chained.length, edges: edges.length, components: comp.components.length,
            p50flight: fl.p50, threshold: c.threshold
        }
    };
}

/* The response may be a bare array or an envelope carrying the installation's own label
   vocabulary. Both are accepted: the page must render against the tracker's own JSON piped
   straight through, and must still honour a renamed escalation label when the server knows
   one. */
function unwrap(payload) {
    if (Array.isArray(payload)) return { beads: payload, edges: null, meta: {}, meter: {} };
    var rows = payload && (Array.isArray(payload.beads) ? payload.beads
                           : Array.isArray(payload.issues) ? payload.issues : null);
    if (!rows) throw new TypeError('unrecognised payload: expected an array of beads');
    return {
        beads: rows,
        /* Lifted out of the rows by the server, or still on them. Null, not [], so "the
           server sent none" and "the server does not lift them" stay distinguishable. */
        edges: Array.isArray(payload.edges) ? payload.edges : null,
        /* The installation's own label vocabulary, when it knows one. */
        meta: payload.meta || {},
        /* THE METER IS THE SERVER'S, NOT OURS TO RECOMPUTE. Round-trip time measured here
           includes the browser's queue and the network; the query's own wall time and the
           budget it is held to are facts only the server has, and they are the numbers that
           say when reading on every request stops being cheap. */
        meter: {
            generatedAtMs: payload.generated_at_ms,
            ageMs: payload.age_ms,
            queryMs: payload.query_ms,
            budgetMs: payload.budget_ms,
            cacheS: payload.cache_s,
            count: payload.count,
            droppedEdges: payload.dropped_edges
        }
    };
}

return { derive: derive, unwrap: unwrap, DEFAULTS: DEFAULTS, FLIGHT: FLIGHT };
}));
