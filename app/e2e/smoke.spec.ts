import { expect, test } from "@playwright/test";

test("sidebar, course page, primary action opens a session that asks the Anki question", async ({ page }) => {
  await page.goto("/");
  const nav = page.getByRole("complementary", { name: "Navigazione" });
  await expect(nav.getByRole("link", { name: "Mappa" })).toBeVisible();
  await nav.getByRole("link", { name: /^Demo/ }).click();
  await expect(page).toHaveURL(/#\/c\/demo$/);

  await expect(page.getByRole("heading", { name: "Demo" })).toBeVisible();
  await expect(page.getByText("Test course.")).toBeVisible();
  await expect(page.getByText("Il secondo passo")).toBeVisible();

  // exactly one primary action; no dialog: the session is created at once
  const primary = page.getByTestId("next-action");
  // layout.spec (same server, runs first) may already have started lesson 1, so "Continua" is also right.
  await expect(primary).toContainText(/^(Inizia la lezione 1|Continua)/);
  await expect(primary).toContainText("Il primo passo");
  await primary.click();
  await expect(page).toHaveURL(/#\/s\//);
  await expect(page.getByText(/flashcard Anki/).first()).toBeVisible();
});

test("a new course starts a plan session from the sidebar", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Nuovo corso" }).click();
  await page.getByLabel("Titolo del corso").fill("Rust base");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/s\//);
});

test("settings persist", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Impostazioni", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("tab", { name: "Motore", exact: true }).click();
  const teacher = dialog.getByLabel(/^Docente/);
  await expect(teacher).toHaveValue("sonnet");
  await teacher.selectOption("haiku");
  await expect(async () => {
    const res = await page.request.get("/api/settings");
    expect((await res.json()).teacherModel).toBe("haiku");
  }).toPass();
  const teacherEffort = dialog.getByRole("radiogroup", { name: "Effort Docente" });
  await teacherEffort.getByRole("radio", { name: "high", exact: true }).click();
  await expect(async () => {
    const res = await page.request.get("/api/settings");
    expect((await res.json()).effort?.teacher).toBe("high");
  }).toPass();
  await teacherEffort.getByRole("radio", { name: "default" }).click();
  await teacher.selectOption("sonnet"); // the e2e server is shared by every spec
  await dialog.getByRole("button", { name: "Chiudi" }).click();
  await expect(dialog).toBeHidden();
});

test("the primary action twice from the course page lands in the same session", async ({ page }) => {
  await page.goto("/#/c/demo");
  const primary = page.getByTestId("next-action");
  await expect(primary).toContainText(/^(Inizia la lezione \d|Continua)/);
  await primary.click();
  await expect(page).toHaveURL(/#\/s\//);
  const first = page.url();
  await page.goto("/#/c/demo");
  await primary.click(); // same course + same target: resumes the live session, no second claude process
  await expect(page).toHaveURL(first);
});

test("a new course whose title collides with an existing slug opens that course", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Nuovo corso" }).click();
  await page.getByLabel("Titolo del corso").fill("Demo");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/c\/demo$/);
});
