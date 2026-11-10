// Starts a Vite dev server with a mock session API, takes screenshots with playwright-core (msedge), stops everything.
// Usage: node app/test/ui-harness/run.mjs [outDir]   (default: <os tmp>/ui-harness-shots)
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { chromium } from "playwright-core";
import { scenario } from "./fixtures.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = process.argv[2] ?? path.join(os.tmpdir(), "ui-harness-shots");
fs.mkdirSync(out, { recursive: true });

const states = new Map(); // id -> SessionState
const clients = new Map(); // id -> Set<res>
const get = (id) => states.get(id) ?? states.set(id, structuredClone(scenario(id))).get(id);
const push = (id, ev) => clients.get(id)?.forEach((res) => res.write(`data: ${JSON.stringify(ev)}\n\n`));
const body = (req) => new Promise((ok) => { let s = ""; req.on("data", (c) => (s += c)); req.on("end", () => ok(s ? JSON.parse(s) : {})); });

const mock = {
  name: "mock-session-api",
  configureServer(server) {
    server.middlewares.use(async (req, res, next) => {
      const m = /^\/api\/sessions\/([^/]+)(?:\/(\w+))?$/.exec(req.url.split("?")[0]);
      if (!m) return next();
      const [, id, op] = m;
      const s = get(id);
      if (op === "events") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write(`data: ${JSON.stringify({ type: "snapshot", state: s })}\n\n`);
        (clients.get(id) ?? clients.set(id, new Set()).get(id)).add(res);
        req.on("close", () => clients.get(id)?.delete(res));
        return;
      }
      if (req.method === "POST" && op === "answer") {
        const b = await body(req);
        const it = s.items.find((x) => x.requestId === b.requestId);
        if (it) {
          if (b.kind === "quiz") {
            const correct = [].concat(it.input.correctAnswer);
            it.result = { ...b.answer, correct: !b.answer.dontKnow && b.answer.selected.length === correct.length && b.answer.selected.every((v) => correct.includes(v)) };
          } else if (b.kind === "ask") it.answer = b.answer;
          else it.decision = b.answer;
          s.pendingRequestId = null;
          s.status = "idle";
          push(id, { type: "item", item: it });
          push(id, { type: "pending", requestId: null });
          push(id, { type: "status", status: "idle" });
        }
      }
      res.setHeader("content-type", "application/json");
      res.end(req.method === "GET" ? JSON.stringify(s) : '{"ok":true}');
    });
  },
};

const server = await createServer({ configFile: false, root: here, logLevel: "warn", plugins: [react(), tailwindcss(), mock], server: { host: "127.0.0.1", port: 5199, strictPort: true, fs: { allow: [path.resolve(here, "../../..")] } } });
await server.listen();
const base = "http://127.0.0.1:5199/";
const browser = await chromium.launch({ channel: "msedge" });
try {
  for (const [w, h] of [[1440, 920], [1000, 680]]) {
    states.clear(); // fresh mock state per viewport
    const page = await browser.newPage({ viewport: { width: w, height: h } });
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    const shot = async (name) => page.screenshot({ path: path.join(out, `${name}-${w}.png`) });
    const open = async (s) => { await page.goto(`${base}?s=${s}`); await page.waitForSelector("section[aria-label=Chat]"); await page.waitForTimeout(1500); };

    await open("ask0"); await shot("ask0");
    await page.keyboard.press("1"); await page.keyboard.press("Enter"); await page.waitForTimeout(800); await shot("ask0-answered");
    await open("quizlong"); await shot("quizlong"); { const b = page.getByRole("button", { name: /Invia/ }); console.log("quizlong Invia in viewport @" + w + ":", await b.evaluate((e) => { const r = e.getBoundingClientRect(); return r.bottom <= innerHeight && r.top >= 0; })); }
    await open("demo"); await page.waitForTimeout(800); await shot("demo"); console.log("demo iframe height @" + w + ":", await page.locator("iframe").evaluate((e) => e.getBoundingClientRect().height));
    await open("plan"); await page.keyboard.press("Enter"); await page.waitForTimeout(500); console.log("plan approved by bare Enter (must be false) @" + w + ":", await page.getByRole("button", { name: "Approva" }).count() === 0);
    await open("practice"); await shot("practice");
    await open("quiz"); await shot("quiz");
    await page.keyboard.press("2"); await page.keyboard.press("ArrowRight"); await page.fill("textarea", "Perché solo i lettori vengono svegliati."); await shot("quiz-filled");
    await page.keyboard.press("Enter"); await page.waitForTimeout(1000); await shot("quiz-answered");
    await open("plan"); await shot("plan-pending");
    await open("transcript"); await shot("transcript-top");
    await page.locator("#node-il-ref").evaluate((e) => e.scrollIntoView()); await page.waitForTimeout(1200); await shot("transcript-node"); await page.getByRole("log").evaluate((el) => (el.scrollTop = el.scrollHeight)); await page.waitForTimeout(600); await shot("transcript-jobs");
    await page.getByRole("button", { name: "Piano" }).click(); await page.waitForTimeout(1500); await shot("transcript-plan-panel");
    await page.getByRole("button", { name: "Ingrandisci la figura" }).click(); await page.waitForTimeout(300); await shot("visual-zoom");
    await page.keyboard.press("Escape");
    if (errors.length) console.log(`console errors @${w}:`, [...new Set(errors)].slice(0, 8));
    await page.close();
  }
} finally {
  await browser.close();
  await server.close();
}
console.log("screenshots in", out);
