// Headless rendering for the visual and demo roles: one lazy shared browser (Edge first, then bundled chromium),
// closed by closeBrowser() from JobManager.shutdown(). Tests skip when no browser can be launched.
import { chromium, type Browser } from "playwright-core";
import { withDemoCsp } from "../../shared/csp.ts";

let browser: Promise<Browser> | undefined;

function getBrowser(): Promise<Browser> {
  browser ??= (async () => {
    try {
      return await chromium.launch({ channel: "msedge" });
    } catch {
      return await chromium.launch();
    }
  })().catch((e) => {
    browser = undefined;
    throw new Error(`nessun browser disponibile per il rendering (${e instanceof Error ? e.message.split("\n")[0] : e})`);
  });
  return browser;
}

export async function closeBrowser(): Promise<void> {
  const b = browser;
  browser = undefined;
  await (await b?.catch(() => undefined))?.close().catch(() => {});
}

/** True when a browser can be launched (tests use it to skip). */
export const browserAvailable = () => getBrowser().then(() => true, () => false);

/** PNG of the SVG on a dark background (the UI is dark). JavaScript is disabled and every non-data request is aborted. */
export async function renderSvgPng(svg: string): Promise<Buffer> {
  const ctx = await (await getBrowser()).newContext({ javaScriptEnabled: false, viewport: { width: 900, height: 600 } });
  try {
    await ctx.route("**/*", (r) => (/^(about:|data:|blob:)/.test(r.request().url()) ? r.continue() : r.abort()));
    const page = await ctx.newPage();
    const csp = "default-src 'none'; style-src 'unsafe-inline'; img-src data:";
    await page.setContent(`<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"></head><body style="margin:0;padding:16px;background:#1c1c1c;display:inline-block">${svg}</body></html>`);
    return await page.screenshot({ fullPage: true, type: "png" });
  } finally {
    await ctx.close().catch(() => {});
  }
}

export interface DemoRun {
  errors: string[];
  png: Buffer;
}

/** Loads the demo under DEMO_CSP (the same policy the UI iframe applies) for ~1.5 s, with ctx.route and WebSockets
 *  blocked as a second layer; collects console errors, page errors and blocked requests. */
export async function runDemoPage(html: string, settleMs = 1500): Promise<DemoRun> {
  const ctx = await (await getBrowser()).newContext({ viewport: { width: 900, height: 600 } });
  try {
    const errors: string[] = [];
    await ctx.route("**/*", (r) => {
      const u = r.request().url();
      if (/^(about:|data:|blob:)/.test(u)) return r.continue();
      errors.push(`richiesta di rete bloccata: ${u}`);
      return r.abort();
    });
    // ctx.route does not see WebSockets (connect-src 'none' in the CSP already refuses them); close any that slip through.
    await ctx.routeWebSocket(/.*/, (ws) => {
      errors.push(`richiesta di rete bloccata: ${ws.url()}`);
      void ws.close();
    });
    const page = await ctx.newPage();
    page.on("console", (m) => m.type() === "error" && errors.push(`console.error: ${m.text()}`));
    page.on("pageerror", (e) => errors.push(`errore di pagina: ${e.message}`));
    await page.setContent(withDemoCsp(html), { waitUntil: "load" });
    await page.waitForTimeout(settleMs);
    return { errors: [...new Set(errors)], png: await page.screenshot({ fullPage: true, type: "png" }) };
  } finally {
    await ctx.close().catch(() => {});
  }
}
