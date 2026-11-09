// Looks at loom's Factory page at 390, 1440 and 1920 px and prints one line per width.
//   node factory-probe.mjs <page-url> [fixture.json]
// With a fixture, /api/factory is answered from it. PROBE_OUT is where the PNGs go (default .);
// PROBE_WRAP=one|two sets the Layout button before the capture and tags the file names.
// The numbers are computed from the drawn SVG and the page's running animations. Nothing here asserts.
import { chromium } from 'playwright';
import { readFileSync } from 'node:fs';

const [url, fixture] = process.argv.slice(2);
if (!url) {
  console.error('usage: factory-probe.mjs <page-url> [fixture.json]');
  process.exit(2);
}
const out = process.env.PROBE_OUT || '.';
const wrap = process.env.PROBE_WRAP || '';
const body = fixture ? readFileSync(fixture, 'utf8') : null;
const sizes = [[390, 844, true], [1440, 900, false], [1920, 1080, false]];

const measure = () => {
  const num = (s) => s.match(/-?\d+(\.\d+)?/g).map(Number);
  const nodes = [...document.querySelectorAll('#floor g.node[data-id]')].map((g) => {
    const r = g.querySelector('rect.frame');
    return { id: g.dataset.id, x: +r.getAttribute('x'), y: +r.getAttribute('y'), w: +r.getAttribute('width'), h: +r.getAttribute('height') };
  });
  const routes = [...document.querySelectorAll('#floor path.edge')].map((p) => {
    const toks = p.getAttribute('d').split(/(?=[MLQ])/).map((t) => [t[0], num(t)]);
    const pts = [];
    toks.forEach(([c, n], i) => {
      if (c === 'Q') pts.push([n[0], n[1]]);
      else if (c === 'M' || toks[i + 1]?.[0] !== 'Q') pts.push([n[0], n[1]]);
    });
    return { from: p.dataset.from, to: p.dataset.to, src: p.dataset.src, dst: p.dataset.dst, pts };
  });
  const segs = routes.flatMap((r) => r.pts.slice(1).map((b, i) => [r.pts[i], b, r]));
  const span = (a, b, c, d) => Math.min(Math.max(a, b), Math.max(c, d)) - Math.max(Math.min(a, b), Math.min(c, d));
  let crossings = 0, overlaps = 0, throughNodes = 0;
  segs.forEach(([a, b, r], i) => {
    for (const p of nodes) {
      if (p.id === r.from || p.id === r.to) continue;
      if (Math.max(a[0], b[0]) > p.x + 1 && Math.min(a[0], b[0]) < p.x + p.w - 1 && Math.max(a[1], b[1]) > p.y + 1 && Math.min(a[1], b[1]) < p.y + p.h - 1) throughNodes++;
    }
    for (const [c, d, q] of segs.slice(i + 1)) {
      if (q === r) continue;
      const h1 = a[1] === b[1], h2 = c[1] === d[1];
      if (h1 !== h2) {
        const [H1, H2, V1, V2] = h1 ? [a, b, c, d] : [c, d, a, b];
        if (V1[0] > Math.min(H1[0], H2[0]) + .5 && V1[0] < Math.max(H1[0], H2[0]) - .5 && H1[1] > Math.min(V1[1], V2[1]) + .5 && H1[1] < Math.max(V1[1], V2[1]) - .5) crossings++;
      } else if (r.src !== q.src && r.dst !== q.dst) {
        if (h1 && Math.abs(a[1] - c[1]) < .5 && span(a[0], b[0], c[0], d[0]) > 1) overlaps++;
        if (!h1 && Math.abs(a[0] - c[0]) < .5 && span(a[1], b[1], c[1], d[1]) > 1) overlaps++;
      }
    }
  });
  const svg = document.getElementById('floor');
  const vb = svg.viewBox.baseVal;
  const animated = [...new Set(document.getAnimations().map((a) => a.effect?.target?.closest?.('[data-id]')?.dataset.id ?? '(outside any node)'))].sort();
  return {
    docW: document.documentElement.scrollWidth, vw: innerWidth,
    floor: `${Math.round(vb.width)}x${Math.round(vb.height)}`, scale: (+svg.getAttribute('width') / vb.width).toFixed(2),
    nodes: nodes.length, edges: routes.length, crossings, overlaps, throughNodes, animations: animated.join(',') || 'none',
  };
};

const browser = await chromium.launch();
const tag = wrap ? `factory-${wrap}` : 'factory';
for (const [width, height, mobile] of sizes) {
  const ctx = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: mobile ? 2 : 1, isMobile: mobile, hasTouch: mobile });
  const page = await ctx.newPage();
  if (body) await page.route('**/api/factory', (r) => r.fulfill({ status: 200, contentType: 'application/json', body }));
  await page.goto(url, { waitUntil: 'networkidle' });
  await page.waitForSelector('#floor g.node');
  for (let i = 0; wrap && i < 3 && !(await page.textContent('#wrapbtn')).includes(wrap === 'one' ? 'one line' : 'two rows'); i++) await page.click('#wrapbtn');
  await page.evaluate(() => scrollTo(0, 0));
  await page.screenshot({ path: `${out}/${tag}-${width}.png` });
  if (mobile) await page.screenshot({ path: `${out}/${tag}-${width}-full.png`, fullPage: true });
  const m = await page.evaluate(measure);
  console.log(`${width} ${Object.entries(m).map(([k, v]) => `${k}=${v}`).join(' ')}`);
  await ctx.close();
}
await browser.close();
