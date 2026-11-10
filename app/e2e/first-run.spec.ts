import { expect, test } from "@playwright/test";

test("first run: an empty data folder shows the Proxi first-run screen", async ({ page }) => {
  // A fresh install: the status says the folder has no courses. The first-run screen replaces sidebar and main pane.
  let live: Record<string, unknown> = {};
  await page.route("**/api/sync", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    live = await res.json();
    await route.fulfill({ response: res, json: { ...live, needsData: true } });
  });
  // Never fall back: the real call would repoint the data folder of the shared e2e server.
  await page.route("**/api/sync/data-root", (route) =>
    route.fulfill({ json: { ...live, needsData: true, restartRequired: true } }),
  );

  await page.goto("/");
  await expect(page).toHaveTitle("Proxi");
  await expect(page.locator("header").locator('a[href="#/"]', { hasText: "Proxi" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Connetti per scaricare i tuoi corsi" })).toBeVisible();
  await expect(page.getByText("Permesso richiesto: repo (crea e aggiorna il repository privato proxi-data)")).toBeVisible();
  await expect(page.getByText("Permesso richiesto: solo i file creati da Proxi")).toBeVisible();

  await page.getByRole("button", { name: "Usa una cartella locale" }).click();
  await page.getByLabel("Cartella dei dati").fill("C:\\proxi-test");
  await page.getByRole("button", { name: "Usa questa cartella" }).click();
  await expect(page.getByText("Cartella impostata: riavvia Proxi per usarla.")).toBeVisible();
});
