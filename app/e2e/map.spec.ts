import { expect, test, type Page } from "@playwright/test";

const KEY = "ia.map.settings.v3";
const stored = (page: Page) => page.evaluate((k) => localStorage.getItem(k), KEY);
const REMOVED = /Spessore archi|Punta delle frecce|Dilatazione|Sfumatura|Opacità del bordo|Dimensione etichette regioni|Regioni/;

test("map settings: five essential controls, lesson labels off by default, saved sparsely", async ({ page }) => {
  await page.goto("/");
  const open = page.getByRole("button", { name: "Impostazioni della mappa" });
  await expect(open).toBeVisible(); // the fixture graph makes the map non-empty
  await expect.poll(() => stored(page)).toBe("{}");
  await open.click();
  const panel = page.getByTestId("map-settings-panel");
  const sliders = panel.getByRole("slider");
  await expect(sliders).toHaveCount(3);
  await expect(sliders.nth(0)).toHaveAccessibleName(/^Dimensione nodi/);
  await expect(sliders.nth(1)).toHaveAccessibleName(/^Dimensione etichette/);
  await expect(sliders.nth(2)).toHaveAccessibleName(/^Etichette fino allo zoom/);
  await expect(panel.getByRole("checkbox")).toHaveCount(2);
  await expect(panel.getByRole("checkbox", { name: "Colori di sfondo" })).toBeChecked();
  await expect(panel.getByRole("checkbox", { name: "Etichette delle lezioni" })).not.toBeChecked();
  await expect(panel.getByText(REMOVED)).toHaveCount(0);

  await panel.getByRole("checkbox", { name: "Etichette delle lezioni" }).check();
  await expect.poll(() => stored(page)).toBe('{"clusterLabels":true}');
  await page.reload();
  await page.getByRole("button", { name: "Impostazioni della mappa" }).click();
  const lessons = page.getByTestId("map-settings-panel").getByRole("checkbox", { name: "Etichette delle lezioni" });
  await expect(lessons).toBeChecked();
  await lessons.uncheck();
  await expect.poll(() => stored(page)).toBe("{}");
});

test("map settings: a legacy full blob keeps its tuning, takes the new lesson-label default, and is rewritten sparse", async ({ page }) => {
  await page.addInitScript((k) => {
    if (sessionStorage.getItem("seeded")) return;
    sessionStorage.setItem("seeded", "1");
    localStorage.setItem(
      k,
      JSON.stringify({
        sizeUnits: "world",
        nodeSize: 3,
        edgeWidth: 2,
        arrowLength: 4,
        labelZoom: 0.35,
        labelSize: 4,
        clusterLabels: true,
        clusterLabelSize: 18,
        regions: { enabled: false, dilation: 40, blur: 60, edgeAlpha: 0.4 },
        physics: {},
      }),
    );
  }, KEY);
  await page.goto("/");
  await expect.poll(() => stored(page)).toBe('{"nodeSize":3,"regions":{"enabled":false}}');
  await page.getByRole("button", { name: "Impostazioni della mappa" }).click();
  const panel = page.getByTestId("map-settings-panel");
  await expect(panel.getByRole("checkbox", { name: "Etichette delle lezioni" })).not.toBeChecked();
  await expect(panel.getByRole("checkbox", { name: "Colori di sfondo" })).not.toBeChecked();
});

test("map settings: an unparsable stored value falls back to the defaults and is rewritten as {}", async ({ page }) => {
  await page.addInitScript((k) => {
    if (sessionStorage.getItem("seeded")) return;
    sessionStorage.setItem("seeded", "1");
    localStorage.setItem(k, "{not json");
  }, KEY);
  await page.goto("/");
  await expect.poll(() => stored(page)).toBe("{}");
  await page.getByRole("button", { name: "Impostazioni della mappa" }).click();
  const panel = page.getByTestId("map-settings-panel");
  await expect(panel.getByRole("checkbox", { name: "Colori di sfondo" })).toBeChecked();
  await expect(panel.getByRole("checkbox", { name: "Etichette delle lezioni" })).not.toBeChecked();
});

test("map settings: moving one slider stores only that field", async ({ page }) => {
  await page.goto("/");
  await expect.poll(() => stored(page)).toBe("{}");
  await page.getByRole("button", { name: "Impostazioni della mappa" }).click();
  const slider = page.getByTestId("map-settings-panel").getByRole("slider").nth(0);
  await slider.focus();
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => stored(page)).toBe('{"nodeSize":3}');
  await page.keyboard.press("ArrowLeft");
  await expect.poll(() => stored(page)).toBe("{}");
});

/** Records every canvas stroke (colour, width, alpha) so the rings can be asserted without reading pixels. */
async function recordStrokes(page: Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { __strokes: Array<{ style: string; width: number; alpha: number }> };
    w.__strokes = [];
    const proto = CanvasRenderingContext2D.prototype;
    const orig = proto.stroke;
    proto.stroke = function (this: CanvasRenderingContext2D, path?: Path2D) {
      w.__strokes.push({ style: String(this.strokeStyle), width: this.lineWidth, alpha: this.globalAlpha });
      return path === undefined ? (orig as () => void).call(this) : orig.call(this, path);
    };
  });
}

/** Replaces the statuses of the fetched snapshot: the Nth drawn proposition takes statuses[N]. */
async function routeStatuses(page: Page, statuses: Array<"shaky" | "missed" | undefined>) {
  await page.route("**/api/graph", async (route) => {
    const res = await route.fetch();
    const snap = await res.json();
    const real = snap.nodes.filter((n: { id: string }) => !n.id.startsWith("arg_"));
    for (const n of snap.nodes) delete n.status;
    statuses.forEach((status, i) => {
      if (status && real[i]) real[i].status = status;
    });
    await route.fulfill({ response: res, json: snap });
  });
}

const strokesOf = (page: Page) =>
  page.evaluate(() => {
    const strokes = (window as unknown as { __strokes: Array<{ style: string; width: number; alpha: number }> }).__strokes;
    const root = getComputedStyle(document.documentElement);
    const warn = root.getPropertyValue("--warn").trim();
    const bad = root.getPropertyValue("--bad").trim();
    return {
      all: strokes,
      warn: strokes.filter((s) => s.style === warn),
      bad: strokes.filter((s) => s.style === bad),
      warnColor: warn,
      badColor: bad,
    };
  });

test("AC1: the map strokes a warn ring on a shaky concept and a bad ring on a missed one", async ({ page }) => {
  await recordStrokes(page);
  await routeStatuses(page, ["shaky", "missed"]);
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Impostazioni della mappa" })).toBeVisible();

  await expect
    .poll(async () => {
      const s = await strokesOf(page);
      return { warn: s.warn.length > 0, bad: s.bad.length > 0 };
    })
    .toEqual({ warn: true, bad: true });

  const rings = [...(await strokesOf(page)).warn, ...(await strokesOf(page)).bad];
  // Visible by default: full alpha, and a positive constant-width screen-space stroke.
  for (const ring of rings) {
    expect(ring.alpha).toBe(1);
    expect(ring.width).toBeGreaterThan(0);
  }
});

test("AC1: a map with no shaky or missed concept strokes no warn or bad ring", async ({ page }) => {
  await recordStrokes(page);
  await routeStatuses(page, []);
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Impostazioni della mappa" })).toBeVisible();
  // Let the first frames paint (the map strokes node discs and arrows too), then assert no ring colour was used.
  await expect.poll(async () => (await strokesOf(page)).all.length).toBeGreaterThan(3);
  expect((await strokesOf(page)).warn).toEqual([]);
  expect((await strokesOf(page)).bad).toEqual([]);
});
