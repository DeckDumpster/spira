// model.test.js — the view model, derived in the page from a raw bead array.
//
//   node --test model.test.js
//
// 12 of test-loom-page.sh's 13 arms live here now, run through node's own test runner
// instead of node spawned once per assertion. The 13th — parsing what the tracker actually
// emits over a real, throwaway bd database — needs bd and stays in
// spira/test-cockpit-bd-contract.sh, which already keeps one row per real-bd query shape.
//
// WHAT THIS HOLDS, and why each one is here rather than assumed:
//
//   1. CHAINS FINDS THE COMPONENTS THE PAYLOAD USED TO NAME. Connected components, execution
//      layers, depth and width are the one part of the model that is a real algorithm rather
//      than a bucket count, and they are what the surface is FOR: a component thirty deep and
//      one wide is an epic capped at one worker however many are free, and that is invisible
//      from any single bead in it. The fixture states the shapes; the derivation must find
//      exactly those.
//   2. AND THE COMPARISON CAN SEE A DIFFERENCE. A component check that passes is worthless
//      until it has been shown to fail: the same assertion run against a fixture with one
//      edge removed must reject it (law-absence-needs-a-positive-control).
//   3. THE CONFIGURED LABELS ARE HONOURED. The escalation and CI labels are settable, and the
//      fixture pins both to something the shipped defaults are NOT — asserting against the
//      default passes just as well if the code has the literal written in, which is the thing
//      the key exists to stop.
//   4. NOTHING PRE-CHEWED SURVIVES IN THE SHIPPED PAGE. No embedded payload, no coordinates,
//      no server-computed anything — the port is only finished if the old payload is gone.
//
// defect: sp-wok.2
// covers: loom/static/model.js loom/static/app.js loom/static/loom.html loom/static/fixture.py

'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const DIR = __dirname;
const M = require(path.join(DIR, 'model.js'));
const F = require(path.join(DIR, 'fixture.json'));
const opts = Object.assign({ now: Date.parse(F.now) }, F.meta);

// ---------------------------------------------------------------------------------------
// A fixture edited by hand no longer matches the shapes its generator claims, and the claim
// is the whole assertion. Regenerating and comparing keeps the two honest without making
// every other test depend on the generator at run time.
test('fixture.json is fixture.py\'s own output', (t) => {
    let python3;
    try { python3 = execFileSync('python3', ['--version']); } catch { python3 = null; }
    if (!python3) { t.skip('no python3 on PATH'); return; }
    const regen = execFileSync('python3', [path.join(DIR, 'fixture.py')], { encoding: 'utf8' });
    const shipped = fs.readFileSync(path.join(DIR, 'fixture.json'), 'utf8');
    assert.equal(regen, shipped, 'fixture.json differs from fixture.py\'s own output — re-run fixture.py');
});

// ---------------------------------------------------------------------------------------
test('chains finds the components the fixture names', () => {
    const m = M.derive(F.beads, opts);
    assert.deepEqual(m.components.map(c => [c.size, c.depth, c.width]), F.expect.components,
        'the component shapes match exactly');
    assert.equal(m.stats.chained, F.expect.chained, 'every chained bead is in one');
    assert.equal(m.edges.length, F.expect.edges, 'the edge count is right');
    assert.equal(m.beads['ch-31'].lay, 31, 'the serial chain is 32 layers deep');
    // A diamond is the case a SHORTEST-path layering gets wrong: dia-d has two prerequisites
    // and must sit behind the slower one, on layer 2, not layer 1.
    assert.deepEqual(['dia-a', 'dia-b', 'dia-c', 'dia-d'].map(i => m.beads[i].lay), [0, 1, 1, 2],
        'a diamond lays out by longest path');
    assert.equal(m.cyclic, false, 'no cycle was reported');
});

test('what must not become an edge', () => {
    const m = M.derive(F.beads, opts);
    assert.equal(m.beads['closed-1'], undefined, 'a closed bead is dropped');
    assert.ok(!m.edges.some(e => e[0] === 'de-rej'), 'a provenance relation is not an edge');
    assert.ok(!m.edges.some(e => e[0] === 'long-since-closed' || e[1] === 'long-since-closed'),
        'a dependency on a bead not present is not');
    assert.equal(m.edges.filter(e => e[0] === 'dia-a' && e[1] === 'dia-b').length, 1,
        'the same edge from both ends is one edge');
});

// The fixture pins both labels OFF their shipped defaults, so a page with the default
// written in scores zero here rather than passing by coincidence.
test('the configured label vocabulary is honoured', () => {
    const m = M.derive(F.beads, opts);
    assert.ok(m.beads['ga-ask'].nr && m.beads['ga-ask2'].nr, 'the escalation label is read from config');
    assert.equal(m.stats.waiting, 2, 'and counted');
    assert.ok(m.beads['ga-ci-0'].ci, 'the CI label is read from config');
    assert.equal(m.stats.parked, 3, 'and counted');
});

test('counters come off the labels', () => {
    const m = M.derive(F.beads, opts);
    assert.equal(m.beads['de-rej'].att, 3, 'attempts are the largest N');
    // `sp-reclaim-4-unrecorded` is the same reclaim as `sp-reclaim-4`, recorded differently.
    assert.equal(m.beads['de-churn'].rec, 4, 'reclaims count the -unrecorded form');
    assert.equal(m.beads['de-poison'].poi, true, 'poison is a label, not a status');
});

test('grouping by repository and epic', () => {
    const m = M.derive(F.beads, opts);
    assert.deepEqual(m.repos.map(r => r.repo), ['alpha', 'beta', 'delta', 'gamma', 'unmapped'],
        'repos are ordered by size');
    assert.equal(m.repos.find(r => r.repo === 'unmapped').n, 12, 'a bead with no repo label is bucketed');
    // The epic's title comes from the parent bead when it is present, and falls back to the
    // id when it is not — an epic named by its id is legible, one named "undefined" is a bug.
    // The loose bucket goes LAST however large it is.
    assert.deepEqual(
        m.repos.find(r => r.repo === 'alpha').epics.map(e => [e.id, e.title, e.n]),
        [
            ['alpha/ch-epic', 'Everything alpha has queued', 32],
            ['alpha/gone-epic', 'gone-epic', 8],
            ['alpha/seq-epic', 'Four steps that really do run in order', 4],
            ['alpha/loose', '(no epic)', 2],
        ],
        'epics resolve, fall back, loose last'
    );
});

test('flow is arrivals and time in flight, and says so', () => {
    const m = M.derive(F.beads, opts);
    assert.equal(m.flow.days.length, 14, 'the window is fourteen days');
    // 53 of the fixture's 114 live beads carry a created_at inside the window; the rest are
    // older and must not be counted, which is the case a naive "count every bead" gets wrong.
    assert.equal(m.flow.created.reduce((a, b) => a + b, 0), 53, 'arrivals are counted');
    // Completions per day and created-to-closed cycle time are computed over closed beads,
    // which the read path does not carry. They must be ABSENT rather than approximated from
    // the open population: a number computed over the wrong population still reads as the
    // number it is named after.
    assert.equal('cycle' in m, false, 'no cycle-time field exists');
    assert.equal('closed' in m.flow, false, 'no completions series exists');
});

// ---------------------------------------------------------------------------------------
// THE SERVER LIFTS THE DEPENDENCY RECORDS OUT OF THE ROWS so it does not write each one
// twice; the tracker's own JSON leaves them on each bead. Same records, two arrangements, and
// the page must find the same graph from either — a second reader for the second arrangement
// is how the two come to disagree about which relations count.
test('both arrangements of the dependency records give the same graph', () => {
    const onBead = M.derive(JSON.parse(JSON.stringify(F.beads)), opts);

    const rows = JSON.parse(JSON.stringify(F.beads));
    const lifted = [];
    for (const b of rows) { for (const d of (b.dependencies || [])) lifted.push(d); delete b.dependencies; }
    const off = M.derive(rows, Object.assign({ edges: lifted }, opts));

    const shape = m => JSON.stringify(m.components.map(c => [c.size, c.depth, c.width]));
    assert.equal(shape(onBead), shape(off), 'lifted edges and on-bead edges agree');

    // The positive control for this arm: strip the records and send NO edges, and the graph
    // must collapse. Without it, two identical answers prove only that both paths found nothing.
    const none = M.derive(JSON.parse(JSON.stringify(rows)), opts);
    assert.equal(none.components.length, 0, 'stripped records really do drop every edge');
});

// ---------------------------------------------------------------------------------------
// Plant an offender: drop one edge from the serial chain, which splits it into two
// components. If the shape assertion above still passes against this, it proves nothing.
test('the comparison can tell a difference (positive control)', () => {
    const F2 = JSON.parse(JSON.stringify(F));
    let cut = 0;
    for (const b of F2.beads) {
        if (b.id !== 'ch-16') continue;
        b.dependencies = (b.dependencies || []).filter(d => d.depends_on_id !== 'ch-15');
        cut++;
    }
    assert.equal(cut, 1, 'the planted offender applied');

    const opts2 = Object.assign({ now: Date.parse(F2.now) }, F2.meta);
    const broken = M.derive(F2.beads, opts2);
    const bshapes = broken.components.map(c => [c.size, c.depth, c.width]);
    assert.notDeepEqual(bshapes, F2.expect.components, 'one missing edge changes the components');
    // and it splits the chain in two: 32-deep becomes two 16-deep, width-1 components.
    const splits = bshapes.filter(s => s[0] === 16 && s[1] === 16 && s[2] === 1).length;
    assert.equal(splits, 2, 'and it splits the chain in two: ' + JSON.stringify(bshapes));
});

// ---------------------------------------------------------------------------------------
test('nothing pre-chewed survives in the shipped page', () => {
    const page = fs.readFileSync(path.join(DIR, 'loom.html'), 'utf8');
    assert.ok(!page.includes('id="payload"'), 'no embedded payload block');
    assert.ok(!page.includes('"vb":'), 'no server-side layout viewbox');
    assert.ok(page.includes('<script src="model.js">'), 'the page loads the model');
    assert.ok(page.includes('<script src="app.js">'), 'and the painter');
    assert.ok(page.includes('<style>'), 'one style block, opened');
    const closes = (page.match(/^<\/style>/gm) || []).length;
    assert.equal(closes, 1, 'and closed exactly once');

    const app = fs.readFileSync(path.join(DIR, 'app.js'), 'utf8');
    assert.ok(app.includes('fetch(API'), 'the page fetches its own beads');
    assert.ok(app.includes('LoomModel.derive'), 'and derives the model itself');
    assert.ok(!app.includes('.xy'), 'no coordinates arrive precomputed');

    // EVERY ELEMENT THE PAINTER ADDRESSES MUST EXIST. A stale id is not an error the browser
    // reports: `$('#gone').textContent = x` throws mid-render, so everything after that line
    // silently does not happen and the page looks merely incomplete. One survived the port.
    const ids = new Set();
    const re = /\$\('#([A-Za-z0-9_-]+)'\)/g;
    let mm;
    while ((mm = re.exec(app))) ids.add(mm[1]);
    const missing = [];
    for (const id of ids) {
        if (page.includes(`id="${id}"`)) continue;
        // An id the painter WRITES and then addresses is fine — the crumb's unzoom button and
        // the inspector's close button exist only once something is drawn.
        if (app.includes(`id="${id}"`)) continue;
        missing.push(id);
    }
    assert.deepEqual(missing, [], 'every id the painter addresses exists');
});
