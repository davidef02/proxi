import { expect, test } from "@playwright/test";

// Never let the real /api/sessions run: it starts a session on the shared server.
test("AC5 (#49): practising selected propositions opens a practice session on them", async ({ page }) => {
  let body: any;
  await page.route("**/api/sessions", (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    body = route.request().postDataJSON();
    return route.fulfill({ json: { id: "e2e-fake" } });
  });

  await page.goto("/");
  await page.getByRole("button", { name: /Catene/ }).click();

  const overlay = page.getByTestId("chain-table-overlay");
  const cluster = overlay.getByTestId("cluster-checkbox-demo/0002-il-secondo-passo");
  await expect(cluster).toBeVisible();
  await cluster.check();

  const practice = overlay.getByTestId("chain-practice");
  await expect(practice).toHaveText("Esercizi sui selezionati");
  await practice.click();

  await expect.poll(() => body).toMatchObject({ kind: "practice", course: "demo" });
  expect(body.practicePropositions).toContain("n_fx_a");
  await expect(page).toHaveURL(/#\/s\/e2e-fake$/);
});
