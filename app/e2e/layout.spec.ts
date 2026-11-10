import { expect, test, type Page } from "@playwright/test";

const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64");
const PLAN = {
  topic: "Demo topic",
  approach: "Prosa.",
  dag: 'graph TD\n  N1["Nodo 1: la prima cosa è vera"] --> N2["Nodo 2: la seconda ne segue"]',
  goal: "capire la prima cosa",
};

async function startSession(page: Page) {
  await page.goto("/#/c/demo");
  await page.getByTestId("next-action").click();
  await expect(page).toHaveURL(/#\/s\//);
  // the server asks the Anki question inside the session: answer "No" through the API (the card UI belongs to the session page)
  const id = page.url().split("#/s/")[1];
  await expect(async () => {
    const s = await (await page.request.get(`/api/sessions/${id}`)).json();
    const ask = s.items.find((i: { kind: string }) => i.kind === "ask");
    expect(ask).toBeTruthy();
    const res = await page.request.post(`/api/sessions/${id}/answer`, {
      headers: { "x-learner-client": "1" },
      data: { requestId: ask.requestId, kind: "ask", answer: { selected: ["no"] } },
    });
    expect(res.ok()).toBe(true);
  }).toPass();
  await expect(page.getByText("sono il docente")).toBeVisible();
}

for (const [w, h] of [[1440, 920], [1000, 680]] as const) {
  test(`shell and course page do not overflow at ${w}x${h}`, async ({ page }) => {
    await page.setViewportSize({ width: w, height: h });
    await page.goto("/#/c/demo");
    await expect(page.getByTestId("next-action")).toBeVisible();
    for (const hash of ["#/c/demo", "#/"]) {
      await page.goto("/" + hash);
      await page.waitForTimeout(150);
      const m = await page.evaluate(() => ({
        v: document.documentElement.scrollHeight - innerHeight,
        h: document.documentElement.scrollWidth - innerWidth,
        main: (() => { const el = document.querySelector("main")!; return el.scrollWidth - el.clientWidth; })(),
      }));
      expect(m.v).toBeLessThanOrEqual(1);
      expect(m.h).toBeLessThanOrEqual(1);
      expect(m.main).toBeLessThanOrEqual(1);
    }
  });
}
const send = async (page: Page, text: string) => {
  await page.getByPlaceholder(/Scrivi al docente/).fill(text);
  await page.getByRole("button", { name: "Invia", exact: true }).click();
};
const noScroll = (page: Page) =>
  page.evaluate(() => {
    const d = document.scrollingElement!;
    return { v: d.scrollHeight - innerHeight, h: d.scrollWidth - innerWidth };
  });

test("plan card approve, Piano tab, long string, Stop", async ({ page }) => {
  await startSession(page);

  await send(page, "FAKE:tool:propose_plan:" + b64(PLAN));
  const card = page.locator("section").filter({ hasText: /Piano della lezione/ }).first();
  await expect(card.getByText("Obiettivo")).toBeVisible();
  // the decision lives in the dock, which replaces the composer while the plan is pending
  const dock = page.getByRole("region", { name: "Decisione" });
  await expect(dock).toBeVisible();
  await expect(page.getByPlaceholder(/Scrivi al docente/)).toHaveCount(0);
  await dock.getByRole("button", { name: "Chiedi modifiche" }).click();
  await dock.getByRole("button", { name: "Annulla" }).click();
  await dock.getByRole("button", { name: "Approva" }).click();
  await expect(card.getByText("Approvato")).toBeVisible();

  await page.getByRole("button", { name: "Piano", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Pannello laterale" });
  await expect(panel.locator("svg").first()).toBeVisible();
  // classDef applied: nodes carry the status classes
  await expect(panel.locator("svg g.node.current, svg g.node.todo, svg g.node.done").first()).toBeVisible();

  // long unbroken learner message: no horizontal nor document scroll
  await expect(page.getByText("Tocca a te", { exact: true })).toBeVisible();
  await send(page, "x".repeat(600) + " https://example.com/" + "a".repeat(400));
  await expect(page.getByText(/^xxxxx/).first()).toBeVisible();
  await expect(page.getByText("Tocca a te", { exact: true })).toBeVisible();
  const m = await noScroll(page);
  expect(m.v).toBeLessThanOrEqual(1);
  expect(m.h).toBeLessThanOrEqual(1);
  const log = page.getByRole("log");
  expect(await log.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);

  // Stop during a slow turn
  await send(page, "FAKE:slow");
  await expect(page.getByText("Il docente scrive…").first()).toBeVisible();
  await page.getByRole("button", { name: "Interrompi" }).first().click();
  await expect(page.getByText("Tocca a te", { exact: true })).toBeVisible();
  expect((await noScroll(page)).v).toBeLessThanOrEqual(1);

  // Quiz dock with reasoning and confidence: no window scroll
  const QUIZ = {
    kind: "check",
    nodeId: "N1",
    question: "Quale affermazione è corretta?",
    options: [{ label: "Opzione uno" }, { label: "Opzione due" }],
    explanation: "spiegazione corretta",
    correctAnswer: "Opzione uno",
  };
  await send(page, "FAKE:tool:quiz:" + b64(QUIZ));
  const quizDock = page.locator("[data-dock]");
  await expect(quizDock).toBeVisible();
  // a check quiz first asks what he was thinking while reading, then opens on demand
  await expect(quizDock.getByLabel("Cosa stavi pensando leggendo?")).toBeVisible();
  expect((await noScroll(page)).v).toBeLessThanOrEqual(1);
  await page.getByTestId("teach-check-reveal").click();
  await expect(quizDock.getByText("Il tuo ragionamento")).toBeVisible();
  let mQuiz = await noScroll(page);
  expect(mQuiz.v).toBeLessThanOrEqual(1);
  expect(mQuiz.h).toBeLessThanOrEqual(1);
  expect(await page.evaluate(() => document.scrollingElement!.scrollTop)).toBe(0);

  // Fill reasoning and select option
  await quizDock.getByPlaceholder(/Perché questa risposta/).fill("La mia ragione");
  await quizDock.locator("button[aria-pressed]").first().click();
  mQuiz = await noScroll(page);
  expect(mQuiz.v).toBeLessThanOrEqual(1);
  expect(await page.evaluate(() => document.scrollingElement!.scrollTop)).toBe(0);
  await quizDock.getByRole("button", { name: /Invia/ }).click();
  await expect(page.getByText("Perché: La mia ragione")).toBeVisible();
});

test("settings dialog displays full course folder path without truncation and copy button", async ({ page }) => {
  await page.goto("/#/c/demo");
  await page.getByRole("button", { name: "Impostazioni", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Impostazioni" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("tab", { name: "Dati", exact: true }).click();
  await expect(dialog.getByText("Cartella dei dati")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Copia percorso" })).toBeVisible();
});

test("image zoom: the scrim starts below the 36px top bar, so the bar keeps its colour; Chiudi, a click on the scrim and Escape close it", async ({ page }) => {
  // #28 review r2: the native window controls are painted with --background; a scrim over the bar would leave them as a
  // light block on a dark strip. Inject a delivered visual job into the session snapshot and open its zoom.
  await page.emulateMedia({ colorScheme: "light" });
  // any session will do (its snapshot is replaced below): do not depend on the Anki question still being pending
  await page.goto("/#/c/demo");
  await page.getByTestId("next-action").click();
  await expect(page).toHaveURL(/#\/s\//);
  const id = page.url().split("#/s/")[1];
  const state = await (await page.request.get(`/api/sessions/${id}`)).json();
  const now = new Date().toISOString();
  const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#3366cc"/></svg>';
  const job = { id: "j-zoom", kind: "visual", title: "Figura: zoom", sessionId: id, model: "sonnet", status: "done", activity: "", createdAt: now, updatedAt: now, costUsd: 0, result: { kind: "visual", svg, caption: "Figura di prova" } };
  const snapshot = { ...state, items: [...state.items, { id: "j-zoom", at: now, kind: "job", job }] };
  await page.route(`**/api/sessions/${id}/events`, (route) =>
    route.fulfill({ status: 200, headers: { "content-type": "text/event-stream", "cache-control": "no-cache" }, body: `data: ${JSON.stringify({ type: "snapshot", state: snapshot })}\n\n` }),
  );
  await page.reload();
  const open = page.getByRole("button", { name: "Ingrandisci la figura" });
  await expect(open).toBeVisible();

  const bar = page.locator("header.topbar");
  const box = await bar.boundingBox();
  expect(box!.y).toBe(0);
  expect(box!.height).toBe(36);
  const barBg = () => bar.evaluate((el) => getComputedStyle(el).backgroundColor);
  const before = await barBg();

  const zoom = page.getByRole("dialog", { name: "Figura di prova" });
  const hitAt = (y: number) => page.evaluate((y) => !!document.elementFromPoint(innerWidth / 2, y)?.closest('[role="dialog"][aria-modal="true"]'), y);
  for (const close of ["button", "scrim", "escape"] as const) {
    await open.click();
    await expect(zoom).toBeVisible();
    const z = await zoom.boundingBox();
    expect(z!.y).toBe(36); // top-9 = 36px: the overlay begins exactly where the bar ends
    expect(Math.round(z!.y + z!.height)).toBe(await page.evaluate(() => innerHeight));
    expect(await hitAt(18)).toBe(false); // the bar is not covered
    expect(await hitAt(40)).toBe(true); // just below it is the scrim
    expect(await barBg()).toBe(before);
    // the close button sits inside the overlay, below the bar, and is not hidden behind it
    const btn = zoom.getByRole("button", { name: "Chiudi" });
    const b = await btn.boundingBox();
    expect(b!.y).toBeGreaterThanOrEqual(36);
    expect(await page.evaluate(([x, y]) => !!document.elementFromPoint(x, y)?.closest('button[aria-label="Chiudi"]'), [b!.x + b!.width / 2, b!.y + b!.height / 2])).toBe(true);
    if (close === "button") await btn.click();
    else if (close === "scrim") await page.mouse.click(10, z!.y + z!.height - 10);
    else await page.keyboard.press("Escape");
    await expect(zoom).toBeHidden();
  }
  // a click on the image itself does not close the zoom
  await open.click();
  await zoom.getByRole("img", { name: "Figura di prova" }).click();
  await expect(zoom).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(zoom).toBeHidden();
});

test("image zoom: the Chiudi button is legible (icon >= 3:1 on its own surface) in every palette x mode", async ({ page }) => {
  // #28 review r3: the X inherited --foreground and was drawn straight on the black scrim (≈1:1 in light modes).
  await page.goto("/#/c/demo");
  await page.getByTestId("next-action").click();
  await expect(page).toHaveURL(/#\/s\//);
  const id = page.url().split("#/s/")[1];
  const state = await (await page.request.get(`/api/sessions/${id}`)).json();
  const now = new Date().toISOString();
  const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#3366cc"/></svg>';
  const job = { id: "j-zoom", kind: "visual", title: "Figura: zoom", sessionId: id, model: "sonnet", status: "done", activity: "", createdAt: now, updatedAt: now, costUsd: 0, result: { kind: "visual", svg, caption: "Figura di prova" } };
  const snapshot = { ...state, items: [...state.items, { id: "j-zoom", at: now, kind: "job", job }] };
  await page.route(`**/api/sessions/${id}/events`, (route) =>
    route.fulfill({ status: 200, headers: { "content-type": "text/event-stream", "cache-control": "no-cache" }, body: `data: ${JSON.stringify({ type: "snapshot", state: snapshot })}\n\n` }),
  );
  const zoom = page.getByRole("dialog", { name: "Figura di prova" });
  const failures: string[] = [];
  for (const palette of ["neutral", "ocean", "grove", "ember", "iris"]) {
    for (const mode of ["light", "dark"]) {
      await page.evaluate(([p, m]) => { localStorage.setItem("ia.palette", p); localStorage.setItem("ia.mode", m); }, [palette, mode]);
      await page.reload();
      const html = page.locator("html");
      await expect(html).toHaveAttribute("data-palette", palette);
      await expect(html).toHaveAttribute("data-theme", mode);
      await page.getByRole("button", { name: "Ingrandisci la figura" }).click();
      await expect(zoom).toBeVisible();
      const ratio = await zoom.getByRole("button", { name: "Chiudi" }).evaluate((btn) => {
        const rgb = (s: string) => s.match(/[\d.]+/g)!.slice(0, 4).map(Number);
        const lum = ([r, g, b]: number[]) => {
          const c = (v: number) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
          return 0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b);
        };
        const bg = rgb(getComputedStyle(btn).backgroundColor);
        const fg = rgb(getComputedStyle(btn.querySelector("svg")!).color);
        if (bg.length === 4 && bg[3] < 1) return -1; // the surface must be opaque, not the scrim showing through
        const [a, b] = [lum(fg), lum(bg)].sort((x, y) => y - x);
        return (a + 0.05) / (b + 0.05);
      });
      if (ratio < 3) failures.push(`${palette}-${mode}: Chiudi icon contrast ${ratio.toFixed(2)} < 3`);
      await page.keyboard.press("Escape");
      await expect(zoom).toBeHidden();
    }
  }
  expect(failures).toEqual([]);
});

// the tests above leave sessions open, and next-action resumes one instead of starting a new one: close them first, so
// startSession lands on a transcript that holds only the opening turn
async function freshSession(page: Page) {
  const list = await (await page.request.get("/api/sessions")).json();
  for (const s of list)
    if (s.status !== "closed") await page.request.post(`/api/sessions/${s.id}/close`, { headers: { "x-learner-client": "1" } });
  await startSession(page);
}

test("session top bar: title, status with token tooltip, Piano/Esercizi, Interrompi; no token text, no Chiudi", async ({ page }) => {
  await freshSession(page);
  const bar = page.locator("header.topbar");
  expect((await bar.boundingBox())!.height).toBe(36);
  await expect(bar.locator("h1")).toBeVisible();
  const status = bar.locator('span[role="status"][aria-live="polite"]');
  await expect(status).toHaveAttribute("title", /^Token di questa sessione · \d+ turni · nuovi [\d.]+ · in uscita [\d.]+ · dalla cache [\d.]+$/);
  await expect(bar.getByRole("button", { name: "Piano", exact: true })).toBeVisible();
  await expect(bar.getByRole("button", { name: /^Esercizi/ })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Chiudi", exact: true })).toHaveCount(0);
  await expect(bar.getByText(/token/i)).toHaveCount(0);
  await expect(bar.getByText("—", { exact: true })).toHaveCount(0);
  await send(page, "FAKE:slow");
  const stop = bar.getByRole("button", { name: "Interrompi", exact: true });
  await expect(stop).toBeVisible();
  await stop.click();
  await expect(bar.getByText("Tocca a te", { exact: true })).toBeVisible();
  await expect(stop).toHaveCount(0);
});

test("sidebar menu: Chiudi sessione closes the session and keeps it listed", async ({ page }) => {
  await freshSession(page);
  const id = page.url().split("#/s/")[1];
  const row = page.getByRole("complementary", { name: "Navigazione" }).locator(`a[href="#/s/${id}"]`);
  const menu = page.locator("[data-session-menu]");
  await row.click({ button: "right" });
  await expect(menu.getByRole("menuitem", { name: "Chiudi sessione" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: /elimina/i })).toBeVisible();
  await menu.getByRole("menuitem", { name: "Chiudi sessione" }).click();
  await expect(menu).toHaveCount(0);
  await expect(async () => {
    const list = await (await page.request.get("/api/sessions")).json();
    expect(list.find((s: { id: string }) => s.id === id)?.status).toBe("closed");
  }).toPass();
  await expect(row).toBeVisible();
  await expect(page.locator("header.topbar").getByText("Chiusa", { exact: true })).toBeVisible();
  await row.click({ button: "right" });
  await expect(menu.getByRole("menuitem", { name: "Elimina sessione" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Chiudi sessione" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
});
