// Runs in the pinned Playwright image. stdin: the one-time login URL, then the authorization URL.
// stdout: the client's redirect URL. ID_ORIGIN is the identity provider's origin, so a redirect
// back to it is never mistaken for the client's callback.
import { createRequire } from 'node:module';
import readline from 'node:readline';
const { chromium } = createRequire(import.meta.url)(process.env.PLAYWRIGHT_MODULE);

const lines = [];
for await (const l of readline.createInterface({ input: process.stdin })) { lines.push(l.trim()); if (lines.length === 2) break; }
const [loginUrl, authUrl] = lines;
const mask = (u) => String(u).replace(/[A-Za-z0-9_\-]{12,}/g, '<x>');
const log = (m) => process.stderr.write(m + '\n');
const isCallback = (u) => /^http:\/\/(localhost|127\.0\.0\.1)[:/]/.test(u) || (/[?&]code=/.test(u) && !u.startsWith(process.env.ID_ORIGIN));

const b = await chromium.launch();
const p = await (await b.newContext()).newPage();

await p.goto(loginUrl);
await p.waitForTimeout(2500);
const submit = p.getByRole('button', { name: /submit|sign in|continue/i });
if (await submit.count()) await submit.first().click();
await p.waitForTimeout(3000);
log(`after login: ${mask(new URL(p.url()).pathname)}`);

let redirect = null;
p.on('request', (r) => { if (!redirect && r.isNavigationRequest() && isCallback(r.url())) redirect = r.url(); });
await p.goto(authUrl).catch(() => {});
for (let i = 0; i < 20 && !redirect; i++) {
	await p.waitForTimeout(1000);
	if (redirect) break;
	const btn = p.getByRole('button', { name: /authorize|allow|approve|continue|sign in|accept/i });
	if (await btn.count()) { log(`clicking "${(await btn.first().innerText()).trim()}"`); await btn.first().click().catch(() => {}); }
}
await b.close();
if (!redirect) { log(`no redirect captured; page ${mask(p.url())}`); process.exit(1); }
log(`redirect captured: ${mask(redirect)}`);
process.stdout.write(redirect + '\n');
