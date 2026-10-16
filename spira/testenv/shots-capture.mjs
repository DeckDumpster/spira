// Viewport captures of the pages named in SHOTS_PAGES ("slug=/path" words) at each of
// SHOTS_WIDTHS, written to SHOTS_OUT as <slug>-<width>.png. A page that fails to capture
// exits non-zero naming it; nothing here reads an image.
import { chromium } from 'playwright';

const base = process.env.SHOTS_BASE_URL;
const out = process.env.SHOTS_OUT;
const widths = (process.env.SHOTS_WIDTHS || '').split(',').filter(Boolean).map(Number);
const pages = (process.env.SHOTS_PAGES || '').split(/\s+/).filter(Boolean).map((p) => p.split(/=(.*)/s));
if (!base || !out || !widths.length || !pages.length) {
  console.error('shots-capture: SHOTS_BASE_URL, SHOTS_OUT, SHOTS_WIDTHS and SHOTS_PAGES are required');
  process.exit(2);
}

const browser = await chromium.launch();
let failed = 0;
for (const [slug, path] of pages) {
  for (const width of widths) {
    try {
      const page = await browser.newPage({ viewport: { width, height: 900 } });
      await page.goto(new URL(path, base).href, { waitUntil: 'networkidle' });
      await page.screenshot({ path: `${out}/${slug}-${width}.png`, fullPage: false });
      await page.close();
    } catch (e) {
      failed++;
      console.error(`shots-capture: ${path} at ${width}px: ${e.message}`);
    }
  }
}
await browser.close();
process.exit(failed ? 1 : 0);
