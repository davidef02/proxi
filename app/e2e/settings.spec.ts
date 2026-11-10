import { expect, test, type Locator, type Page } from "@playwright/test";

const SECTIONS = ["Generali", "Motore", "Dettatura vocale", "Mappa", "Sincronizzazione", "Aggiornamenti", "Dati"];

async function openFromSidebar(page: Page) {
  await page.getByRole("button", { name: "Impostazioni", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Impostazioni" });
  await expect(dialog).toBeVisible();
  return dialog;
}
const tab = (dialog: Locator, name: string) => dialog.getByRole("tab", { name, exact: true });

test("settings: seven sections, switching keeps the dialog size, only the panel scrolls", async ({ page }) => {
  await page.goto("/#/c/demo");
  const dialog = await openFromSidebar(page);
  const list = dialog.getByRole("tablist", { name: "Sezioni delle impostazioni" });
  await expect(list).toHaveAttribute("aria-orientation", "vertical");
  await expect(list.getByRole("tab")).toHaveText(SECTIONS);
  await expect(tab(dialog, "Generali")).toHaveAttribute("aria-selected", "true");
  await expect(dialog.getByRole("tabpanel", { name: "Generali" })).toBeVisible();
  await expect(dialog.getByRole("radiogroup", { name: "Tema" })).toBeVisible();
  const size = await dialog.boundingBox();
  expect(size!.width).toBeGreaterThan(600);
  for (const name of ["Motore", "Aggiornamenti", "Dati", "Mappa"]) {
    await tab(dialog, name).click();
    await expect(tab(dialog, name)).toHaveAttribute("aria-selected", "true");
    const panel = dialog.getByRole("tabpanel", { name });
    await expect(panel).toBeVisible();
    expect(await panel.evaluate((el) => getComputedStyle(el).overflowY)).toBe("auto");
    const box = await dialog.boundingBox();
    expect(Math.abs(box!.width - size!.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(box!.height - size!.height)).toBeLessThanOrEqual(1);
    expect(await dialog.evaluate((d) => d.scrollHeight - d.clientHeight)).toBeLessThanOrEqual(1);
  }
  await tab(dialog, "Dati").click();
  await expect(dialog.getByText("Cartella dei dati")).toBeVisible();
  await expect(dialog.getByText("I file non vengono spostati: copiali a mano nella nuova cartella.")).toBeVisible();
  await expect(dialog.getByText(/data -- migrate/)).toHaveCount(0);
  // reopening without a target starts on Generali again (nothing remembered)
  await dialog.getByRole("button", { name: "Chiudi" }).click();
  await expect(dialog).toBeHidden();
  await openFromSidebar(page);
  await expect(tab(dialog, "Generali")).toHaveAttribute("aria-selected", "true");
});

test("settings: keyboard moves through the tab list with a single tab stop", async ({ page }) => {
  await page.goto("/#/c/demo");
  const dialog = await openFromSidebar(page);
  await expect(tab(dialog, "Generali")).toBeFocused();
  await expect(dialog.locator('[role="tab"][tabindex="0"]')).toHaveCount(1);
  await expect(dialog.locator('[role="tab"][tabindex="-1"]')).toHaveCount(6);
  await page.keyboard.press("ArrowDown");
  await expect(tab(dialog, "Motore")).toBeFocused();
  await expect(tab(dialog, "Motore")).toHaveAttribute("aria-selected", "true");
  await expect(tab(dialog, "Motore")).toHaveAttribute("tabindex", "0");
  await expect(dialog.getByRole("tabpanel", { name: "Motore" })).toBeVisible();
  await page.keyboard.press("End");
  await expect(tab(dialog, "Dati")).toBeFocused();
  await expect(tab(dialog, "Dati")).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowDown"); // wraps
  await expect(tab(dialog, "Generali")).toBeFocused();
  await page.keyboard.press("ArrowUp"); // wraps back
  await expect(tab(dialog, "Dati")).toBeFocused();
  await page.keyboard.press("Home");
  await expect(tab(dialog, "Generali")).toBeFocused();
  await expect(tab(dialog, "Generali")).toHaveAttribute("aria-selected", "true");
  const controls = await tab(dialog, "Generali").getAttribute("aria-controls");
  expect(controls).toBe("settings-panel-generali");
  // single tab stop: Tab leaves the tab list instead of visiting the other tabs
  await page.keyboard.press("ArrowDown");
  await expect(tab(dialog, "Motore")).toBeFocused();
  await page.keyboard.press("Tab");
  expect(await page.evaluate(() => document.activeElement?.getAttribute("role"))).not.toBe("tab");
  await page.keyboard.press("Shift+Tab");
  await expect(tab(dialog, "Motore")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  // closing with Escape and reopening without a target starts on Generali, focused
  await openFromSidebar(page);
  await expect(tab(dialog, "Generali")).toHaveAttribute("aria-selected", "true");
  await expect(tab(dialog, "Generali")).toBeFocused();
});

test("settings: colour scheme and theme are independent, persist, and ignore the legacy key", async ({ page }) => {
  await page.addInitScript(() => { if (!sessionStorage.getItem("seeded")) { localStorage.clear(); localStorage.setItem("ia.theme", "light"); sessionStorage.setItem("seeded", "1"); } });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/#/c/demo");
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-theme", "dark");     // system follows the OS, legacy "light" ignored
  await expect(html).toHaveAttribute("data-palette", "neutral");
  const dialog = await openFromSidebar(page);
  const mode = dialog.getByRole("radiogroup", { name: "Schema colori del tema" });
  const themes = dialog.getByRole("radiogroup", { name: "Temi" });
  await expect(themes.getByRole("radio")).toHaveCount(5);
  await expect(mode.getByRole("radio", { name: "Sistema" })).toHaveAttribute("aria-checked", "true");
  const bg = () => page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor);
  const before = await bg();
  await themes.getByRole("radio", { name: "Oceano" }).click();
  await expect(html).toHaveAttribute("data-palette", "ocean");
  await expect(html).toHaveAttribute("data-theme", "dark");
  expect(await bg()).not.toBe(before);
  await mode.getByRole("radio", { name: "Chiaro" }).click();
  await expect(html).toHaveAttribute("data-theme", "light");
  await expect(html).toHaveAttribute("data-palette", "ocean");
  await page.reload();
  await expect(html).toHaveAttribute("data-theme", "light");
  await expect(html).toHaveAttribute("data-palette", "ocean");
  // restore: the e2e browser profile is shared by every spec
  await page.evaluate(() => { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); });
});

test("settings: Sistema follows the OS live; every theme card applies its palette and paints its own orbs", async ({ page }) => {
  await page.addInitScript(() => { if (!sessionStorage.getItem("seeded")) { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); sessionStorage.setItem("seeded", "1"); } });
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/#/c/demo");
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-theme", "light");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(html).toHaveAttribute("data-theme", "dark"); // OS flip while the app is open
  const dialog = await openFromSidebar(page);
  const mode = dialog.getByRole("radiogroup", { name: "Schema colori del tema" });
  const themes = dialog.getByRole("radiogroup", { name: "Temi" });
  await expect(themes.getByRole("radio")).toHaveText(["Neutro", "Oceano", "Bosco", "Brace", "Iris"]);
  await expect(themes.getByRole("radio", { name: "Neutro" })).toHaveAttribute("aria-checked", "true");

  // each card's orbs carry their own palette × mode, so they differ from card to card
  const orbColours = await themes.getByRole("radio").evaluateAll((cards) =>
    cards.map((c) => [...c.querySelectorAll<HTMLElement>("[data-palette][data-theme]")].map((o) => getComputedStyle(o).backgroundColor).join("|")),
  );
  expect(new Set(orbColours).size).toBe(5);

  const bgs = new Set<string>();
  for (const [name, value] of [["Oceano", "ocean"], ["Bosco", "grove"], ["Brace", "ember"], ["Iris", "iris"], ["Neutro", "neutral"]]) {
    await themes.getByRole("radio", { name }).click();
    await expect(html).toHaveAttribute("data-palette", value);
    await expect(themes.getByRole("radio", { name })).toHaveAttribute("aria-checked", "true");
    await expect(themes.locator('[aria-checked="true"]')).toHaveCount(1);
    bgs.add(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor));
  }
  expect(bgs.size).toBe(5);

  // explicit mode stops following the OS; Sistema resumes it
  await mode.getByRole("radio", { name: "Chiaro" }).click();
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(html).toHaveAttribute("data-theme", "light");
  await mode.getByRole("radio", { name: "Sistema" }).click();
  await expect(html).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(html).toHaveAttribute("data-theme", "light");
  await page.evaluate(() => { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); });
});

test("settings: the update banner opens the Aggiornamenti section", async ({ page }) => {
  // regex, not a glob: must not catch /api/update/check or /api/update/apply
  await page.route(/\/api\/update$/, (route) =>
    route.fulfill({
      json: {
        state: "available",
        behind: 2,
        activeSessions: 0,
        checkedAt: new Date().toISOString(),
        commits: [{ hash: "abc1234", subject: "feat: demo change", relativeTime: "2 ore fa" }],
      },
    }),
  );
  await page.goto("/#/c/demo");
  await page.getByRole("button", { name: "Vedi", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Impostazioni" });
  await expect(dialog).toBeVisible();
  await expect(tab(dialog, "Aggiornamenti")).toHaveAttribute("aria-selected", "true");
  await expect(tab(dialog, "Aggiornamenti")).toBeFocused();
  const panel = dialog.getByRole("tabpanel", { name: "Aggiornamenti" });
  await expect(panel.getByText("Aggiornamento disponibile (2 commit)")).toBeVisible();
  await expect(panel.getByText("feat: demo change")).toBeVisible();
  // the sidebar button opens with no target: Generali, not the last section
  await dialog.getByRole("button", { name: "Chiudi" }).click();
  await expect(dialog).toBeHidden();
  await page.getByRole("button", { name: "Impostazioni", exact: true }).click();
  await expect(tab(dialog, "Generali")).toHaveAttribute("aria-selected", "true");
});

test("settings: the app is titled Proxi and Aggiornamenti says Proxi è aggiornato", async ({ page }) => {
  await page.route(/\/api\/update$/, (route) =>
    route.fulfill({
      json: {
        state: "idle",
        behind: 0,
        activeSessions: 0,
        checkedAt: new Date().toISOString(),
        commits: [],
      },
    }),
  );
  await page.goto("/#/c/demo");
  await expect(page).toHaveTitle("Proxi");
  const dialog = await openFromSidebar(page);
  await tab(dialog, "Aggiornamenti").click();
  const panel = dialog.getByRole("tabpanel", { name: "Aggiornamenti" });
  await expect(panel.getByText("Proxi è aggiornato")).toBeVisible();
});

test("settings: every section is reachable and still saves, a key typed before switching tab is kept", async ({ page }) => {
  const initial = await (await page.request.get("/api/settings")).json();
  try {
    await page.goto("/#/c/demo");
    const dialog = await openFromSidebar(page);
    // only the selected panel is shown
    await expect(dialog.getByRole("tabpanel")).toHaveCount(1);
    await expect(dialog.locator('[role="tabpanel"][hidden]')).toHaveCount(6);

    await tab(dialog, "Dettatura vocale").click();
    const voice = dialog.getByRole("tabpanel", { name: "Dettatura vocale" });
    const stt = voice.getByRole("radiogroup", { name: "Servizio STT" });
    await stt.getByRole("radio", { name: "Deepgram" }).click();
    await expect(async () => {
      expect((await (await page.request.get("/api/settings")).json()).sttProvider).toBe("deepgram");
    }).toPass();
    await expect(voice.getByText("API Key Deepgram")).toBeVisible();
    // blur by clicking another tab: the onBlur save must still fire
    await voice.getByLabel("API Key dettatura vocale").fill("e2e-stt-key");
    await tab(dialog, "Mappa").click();
    await expect(async () => {
      expect((await (await page.request.get("/api/settings")).json()).sttApiKey).toBe("e2e-stt-key");
    }).toPass();

    const map = dialog.getByRole("tabpanel", { name: "Mappa" });
    const autoMap = map.getByRole("checkbox", { name: /Aggiorna a fine lezione/ });
    await expect(autoMap).toBeChecked({ checked: initial.autoMap });
    await autoMap.click();
    await expect(autoMap).toBeChecked({ checked: !initial.autoMap });
    await expect(async () => {
      expect((await (await page.request.get("/api/settings")).json()).autoMap).toBe(!initial.autoMap);
    }).toPass();

    await tab(dialog, "Sincronizzazione").click();
    const sync = dialog.getByRole("tabpanel", { name: "Sincronizzazione" });
    await expect(sync.getByRole("radio", { name: "Google Drive" })).toBeVisible();
    await expect(sync.getByRole("radio", { name: "Solo questo PC" })).toBeVisible();
    await expect(sync.getByText("Anki", { exact: true })).toBeVisible();
    await expect(sync.getByText("chiuso")).toBeVisible(); // e2e Anki URL is unreachable

    // switching back keeps the saved values on screen (the dialog stays mounted)
    await tab(dialog, "Dettatura vocale").click();
    await expect(voice.getByRole("radio", { name: "Deepgram" })).toHaveAttribute("aria-checked", "true");
    await expect(voice.getByLabel("API Key dettatura vocale")).toHaveValue("e2e-stt-key");
  } finally {
    // restore: the e2e server is shared by every spec
    const res = await page.request.post("/api/settings", {
      headers: { "x-learner-client": "1" },
      data: { sttProvider: initial.sttProvider, sttApiKey: initial.sttApiKey, autoMap: initial.autoMap },
    });
    expect(res.ok()).toBe(true);
    const restored = await (await page.request.get("/api/settings")).json();
    expect(restored.sttProvider).toBe(initial.sttProvider);
    expect(restored.sttApiKey).toBe(initial.sttApiKey);
    expect(restored.autoMap).toBe(initial.autoMap);
  }
});

test("settings: on a short, narrow window the dialog is clamped and only the panel scrolls", async ({ page }) => {
  await page.setViewportSize({ width: 600, height: 360 });
  await page.goto("/#/c/demo");
  const dialog = await openFromSidebar(page);
  const box = await dialog.boundingBox();
  expect(box!.width).toBeLessThanOrEqual(600 - 32 + 1);
  expect(box!.height).toBeLessThanOrEqual(360 - 48 + 1);

  await tab(dialog, "Motore").click();
  const panel = dialog.getByRole("tabpanel", { name: "Motore" });
  await expect(panel.getByLabel(/^Docente/)).toBeVisible();
  expect(await panel.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);
  const title = dialog.getByRole("heading", { name: "Impostazioni" });
  const before = { title: await title.boundingBox(), tab: await tab(dialog, "Generali").boundingBox() };
  await panel.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  expect(await panel.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  // header and tab list are pinned; the dialog itself never scrolls
  expect(await title.boundingBox()).toEqual(before.title);
  expect(await tab(dialog, "Generali").boundingBox()).toEqual(before.tab);
  expect(await dialog.evaluate((d) => d.scrollTop)).toBe(0);
  const after = await dialog.boundingBox();
  expect(after).toEqual(box);
});

test("settings: the sync provider follows the live status, not the settings loaded when the dialog opened", async ({ page }) => {
  // Snapshot: settings say GitHub (connected). The server then switches to local on "Disconnetti": the control must follow.
  let disconnected = false;
  await page.route("**/api/settings", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    await route.fulfill({ response: res, json: { ...(await res.json()), syncProvider: "github" } });
  });
  await page.route("**/api/sync", async (route) => {
    if (route.request().method() !== "GET" || disconnected) return route.fallback();
    const res = await route.fetch();
    const live = await res.json();
    await route.fulfill({ response: res, json: { ...live, provider: "github", github: { connected: true, login: "octo", repo: "octo/proxi-data" } } });
  });
  await page.route("**/api/sync/disconnect", async (route) => {
    disconnected = true;
    await route.fallback(); // the real server: nothing is connected, the provider stays local
  });

  await page.goto("/#/c/demo");
  const dialog = await openFromSidebar(page);
  await tab(dialog, "Sincronizzazione").click();
  const sync = dialog.getByRole("tabpanel", { name: "Sincronizzazione" });
  const where = sync.getByRole("radiogroup", { name: "Dove sincronizzare" });
  await expect(where.getByRole("radio", { name: "GitHub" })).toHaveAttribute("aria-checked", "true");
  await expect(sync.getByText("Connesso come octo · octo/proxi-data")).toBeVisible();

  await sync.getByRole("button", { name: "Disconnetti" }).click();
  await expect(where.getByRole("radio", { name: "Solo questo PC" })).toHaveAttribute("aria-checked", "true");
  await expect(where.getByRole("radio", { name: "GitHub" })).toHaveAttribute("aria-checked", "false");
  await expect(sync.getByRole("button", { name: "Accedi con GitHub" })).toHaveCount(0);
  await expect(sync.getByText(/I dati restano nella cartella indicata in Dati/)).toBeVisible();
});

test("settings: switching sync provider refreshes the status only after the save, so the control does not snap back", async ({ page }) => {
  // Review r2 finding 1: GET /api/sync raced POST /api/settings; a status read before the save still said the old provider.
  let saving = false;
  let saved = false;
  const syncDuringSave: string[] = [];
  let syncAfterSave = 0;
  await page.route("**/api/settings", async (route) => {
    if (route.request().method() !== "POST" || !("syncProvider" in (route.request().postDataJSON() ?? {}))) return route.fallback();
    saving = true;
    await new Promise((r) => setTimeout(r, 800)); // a slow save: a racing status read would win
    const res = await route.fetch();
    await route.fulfill({ response: res });
    saving = false;
    saved = true;
  });
  await page.route("**/api/sync", async (route) => {
    if (route.request().method() === "GET") {
      if (saving) syncDuringSave.push(new Date().toISOString());
      else if (saved) syncAfterSave++;
    }
    await route.fallback();
  });

  try {
    await page.goto("/#/c/demo");
    const dialog = await openFromSidebar(page);
    await tab(dialog, "Sincronizzazione").click();
    const sync = dialog.getByRole("tabpanel", { name: "Sincronizzazione" });
    const where = sync.getByRole("radiogroup", { name: "Dove sincronizzare" });
    await expect(where.getByRole("radio", { name: "Solo questo PC" })).toHaveAttribute("aria-checked", "true");

    await where.getByRole("radio", { name: "GitHub" }).click();
    await expect.poll(() => syncAfterSave, { timeout: 5000 }).toBeGreaterThan(0);
    expect(syncDuringSave).toEqual([]);
    await expect(where.getByRole("radio", { name: "GitHub" })).toHaveAttribute("aria-checked", "true");
    await expect(sync.getByRole("button", { name: "Accedi con GitHub" })).toBeVisible();
    // it stays there: nothing later puts the old provider back
    await page.waitForTimeout(500);
    await expect(where.getByRole("radio", { name: "GitHub" })).toHaveAttribute("aria-checked", "true");
    expect((await (await page.request.get("/api/sync")).json()).provider).toBe("github");
  } finally {
    // restore: the e2e server is shared by every spec
    const res = await page.request.post("/api/settings", { headers: { "x-learner-client": "1" }, data: { syncProvider: "local" } });
    expect(res.ok()).toBe(true);
    expect((await (await page.request.get("/api/sync")).json()).provider).toBe("local");
  }
});

test("settings: a desktop preload without setChrome does not break the app; theme changes still apply", async ({ page }) => {
  // Review r1 finding 1: an older preload exposes iaDesktop without setChrome; apply() must not throw at import.
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.addInitScript(() => {
    (window as unknown as { iaDesktop: object }).iaDesktop = {};
    if (!sessionStorage.getItem("seeded")) { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); sessionStorage.setItem("seeded", "1"); }
  });
  await page.goto("/#/c/demo");
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-palette", "neutral");
  const dialog = await openFromSidebar(page);
  await dialog.getByRole("radiogroup", { name: "Temi" }).getByRole("radio", { name: "Bosco" }).click();
  await expect(html).toHaveAttribute("data-palette", "grove");
  await dialog.getByRole("radiogroup", { name: "Schema colori del tema" }).getByRole("radio", { name: "Scuro" }).click();
  await expect(html).toHaveAttribute("data-theme", "dark");
  expect(errors).toEqual([]);
  await page.evaluate(() => { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); });
});

test("settings: a theme change inside the open dialog sends the new chrome, and the scrim leaves the 36px top bar uncovered", async ({ page }) => {
  // User requirement (review r1): native window controls follow the theme while a modal is open and match the top bar.
  type Pair = { color: string; symbolColor: string };
  type Payload = { modePref: string; light: Pair; dark: Pair };
  await page.addInitScript(() => {
    const w = window as unknown as { iaDesktop: object; __chrome: unknown[] };
    w.__chrome = [];
    w.iaDesktop = { setChrome: (c: unknown) => w.__chrome.push(JSON.parse(JSON.stringify(c))) };
    if (!sessionStorage.getItem("seeded")) { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); sessionStorage.setItem("seeded", "1"); }
  });
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/#/c/demo");
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-palette", "neutral");
  const sent = () => page.evaluate(() => (window as unknown as { __chrome: Payload[] }).__chrome);
  const last = async () => (await sent()).at(-1)!;
  // the tokens of the live <html> in its current mode, as #rrggbb (what the native overlay is painted with)
  const live = () => page.evaluate(() => {
    const s = getComputedStyle(document.documentElement);
    return { color: s.getPropertyValue("--background").trim().toLowerCase(), symbolColor: s.getPropertyValue("--muted-foreground").trim().toLowerCase() };
  });
  await expect.poll(async () => (await sent()).length).toBeGreaterThan(0);
  const neutral = await last();
  expect(neutral.modePref).toBe("system");
  for (const pair of [neutral.light, neutral.dark]) for (const c of [pair.color, pair.symbolColor]) expect(c).toMatch(/^#[0-9a-f]{6}$/);
  expect(neutral.light).toEqual(await live());

  const dialog = await openFromSidebar(page);
  const count = (await sent()).length;
  await dialog.getByRole("radiogroup", { name: "Temi" }).getByRole("radio", { name: "Iris" }).click();
  await expect(html).toHaveAttribute("data-palette", "iris");
  await expect.poll(async () => (await sent()).length).toBeGreaterThan(count);
  const iris = await last();
  expect(iris.light).toEqual(await live());
  expect(iris.light.color).not.toBe(neutral.light.color);
  expect(iris.dark.color).not.toBe(neutral.dark.color);
  // the dark pair is the one main uses when the OS turns dark: it matches the live tokens once the mode flips
  await dialog.getByRole("radiogroup", { name: "Schema colori del tema" }).getByRole("radio", { name: "Scuro" }).click();
  await expect(html).toHaveAttribute("data-theme", "dark");
  const dark = await last();
  expect(dark.modePref).toBe("dark");
  expect(dark.dark).toEqual(await live());
  expect(dark.dark).toEqual(iris.dark);

  // the scrim starts below the top bar, so the bar (and the native controls painted to match it) keep their colour
  await expect(dialog).toBeVisible();
  const bar = await page.locator("header.topbar").boundingBox();
  expect(bar!.y).toBe(0);
  expect(bar!.height).toBe(36);
  const backdrop = await dialog.evaluate((d) => {
    const s = getComputedStyle(d, "::backdrop");
    return { top: s.top, bottom: s.bottom, position: s.position };
  });
  expect(backdrop).toEqual({ top: "36px", bottom: "0px", position: "fixed" });
  // hit testing: a point on the scrim targets the dialog (its ::backdrop); a point on the bar does not
  const hitAt = (y: number) => page.evaluate((y) => document.elementFromPoint(4, y)?.tagName ?? "none", y);
  expect(await hitAt(18)).not.toBe("DIALOG");
  expect(await hitAt(36 + 4)).toBe("DIALOG");
  await page.evaluate(() => { localStorage.removeItem("ia.mode"); localStorage.removeItem("ia.palette"); });
});

test("quota: not in the top bar; Motore shows the 5 ore / 7 giorni block with bars and reset times", async ({ page }) => {
  const now = Math.floor(Date.now() / 1000);
  const fiveReset = now + 2 * 3600, sevenReset = now + 3 * 86400;
  // before the first goto: DataProvider reads /api/limits once per SSE connect
  await page.route("**/api/limits", (route) =>
    route.fulfill({ json: { fiveHour: { utilization: 0.34, resetsAt: fiveReset }, sevenDay: { utilization: 0.84, resetsAt: sevenReset }, at: new Date().toISOString() } }),
  );
  await page.goto("/#/c/demo");
  const bar = page.locator("header.topbar");
  await expect(bar.getByRole("button", { name: /barra laterale/ })).toBeVisible();
  await expect(bar.locator('a[href="#/"]', { hasText: "Proxi" })).toBeVisible();
  // the third child is the TopBarSlot target (page/session controls are portalled into it); empty here, so present but zero-size
  const slot = bar.locator(":scope > div");
  await expect(slot).toHaveCount(1);
  expect(await slot.evaluate((el) => el === el.parentElement!.lastElementChild)).toBe(true);
  await expect(bar.locator(`[aria-label="Quota dell'abbonamento Claude"]`)).toHaveCount(0);

  const dialog = await openFromSidebar(page);
  await tab(dialog, "Motore").click();
  const panel = dialog.getByRole("tabpanel", { name: "Motore" });
  const block = panel.locator(`[aria-label="Quota dell'abbonamento Claude"]`);
  await expect(block).toBeVisible();
  await expect(block.getByText("Quota dell'abbonamento Claude", { exact: true })).toBeVisible();
  await expect(block.getByText(/^rilevata alle \d{2}:\d{2}$/)).toBeVisible();
  await expect(block.getByText("5 ore", { exact: true })).toBeVisible();
  await expect(block.getByText("7 giorni", { exact: true })).toBeVisible();
  await expect(block.getByText("34%", { exact: true })).toBeVisible();
  await expect(block.getByText("84%", { exact: true })).toHaveClass(/text-warn/);
  const fills = block.locator("[data-quota-fill]");
  await expect(fills).toHaveCount(2);
  await expect(fills.nth(0)).toHaveAttribute("style", /width: 34%/);
  await expect(fills.nth(1)).toHaveAttribute("style", /width: 84%/);
  const [t5, t7] = await page.evaluate((xs) => xs.map((s) => new Date(s * 1000).toLocaleTimeString("it-IT", { hour: "2-digit", minute: "2-digit" })), [fiveReset, sevenReset]);
  await expect(block.getByText(new RegExp(`^si azzera \\S+ alle ${t5}$`))).toBeVisible();
  await expect(block.getByText(new RegExp(`^si azzera \\S+ alle ${t7}$`))).toBeVisible();
  // inside the Claude area: after the CLI row
  const cliBeforeBlock = await panel.evaluate((p) => {
    const cli = [...p.querySelectorAll("span")].find((e) => e.textContent === "Claude Code CLI")!;
    const q = p.querySelector(`[aria-label="Quota dell'abbonamento Claude"]`)!;
    return !!(cli.compareDocumentPosition(q) & Node.DOCUMENT_POSITION_FOLLOWING);
  });
  expect(cliBeforeBlock).toBe(true);
});

test("quota: edge cases — one window only, over 100% capped, no reset time, bad tone; no data hides the block", async ({ page }) => {
  let body: object = { fiveHour: { utilization: 1.234 }, at: new Date().toISOString() };
  await page.route("**/api/limits", (route) => route.fulfill({ json: body }));
  await page.goto("/#/c/demo");
  let dialog = await openFromSidebar(page);
  await tab(dialog, "Motore").click();
  let panel = dialog.getByRole("tabpanel", { name: "Motore" });
  const block = panel.locator(`[aria-label="Quota dell'abbonamento Claude"]`);
  await expect(block).toBeVisible();
  await expect(block.getByText("5 ore", { exact: true })).toBeVisible();
  await expect(block.getByText("7 giorni", { exact: true })).toHaveCount(0);
  // the text shows the real value with the bad tone, the bar is capped at 100%
  await expect(block.getByText("123%", { exact: true })).toHaveClass(/text-bad/);
  const fills = block.locator("[data-quota-fill]");
  await expect(fills).toHaveCount(1);
  await expect(fills.first()).toHaveAttribute("style", /width: 100%/);
  await expect(fills.first()).toHaveClass(/bg-bad/);
  await expect(block.getByText(/si azzera/)).toHaveCount(0);

  // below 75%: muted tone, not warn/bad
  body = { sevenDay: { utilization: 0.5, resetsAt: Math.floor(Date.now() / 1000) + 3600 }, at: new Date().toISOString() };
  await page.reload();
  dialog = await openFromSidebar(page);
  await tab(dialog, "Motore").click();
  panel = dialog.getByRole("tabpanel", { name: "Motore" });
  await expect(block).toBeVisible();
  await expect(block.getByText("5 ore", { exact: true })).toHaveCount(0);
  await expect(block.getByText("50%", { exact: true })).toHaveClass(/text-heading/);
  await expect(block.locator("[data-quota-fill]")).not.toHaveClass(/bg-(warn|bad)/);
  await expect(block.getByText(/^si azzera \S+ alle \d{2}:\d{2}$/)).toBeVisible();

  // a report without any window (API key, other providers, no turn yet): no block at all
  body = { at: new Date().toISOString() };
  await page.reload();
  dialog = await openFromSidebar(page);
  await tab(dialog, "Motore").click();
  panel = dialog.getByRole("tabpanel", { name: "Motore" });
  await expect(panel.getByText("Claude Code CLI", { exact: true })).toBeVisible();
  await expect(panel.locator(`[aria-label="Quota dell'abbonamento Claude"]`)).toHaveCount(0);
});
