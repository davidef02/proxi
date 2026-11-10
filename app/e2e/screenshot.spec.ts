import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import type { CourseOverview } from "../shared/protocol.ts";

// README image. Mocked, never seeded: the e2e server is shared with the other specs.
test.use({ viewport: { width: 1280, height: 800 } });

test("README screenshot: demo course page with Continua and Ripasso", async ({ page }) => {
  await page.route("**/api/courses", async (route) => {
    const res = await route.fetch();
    const list: CourseOverview[] = await res.json();
    const demo = list.find((c) => c.slug === "demo")!;
    Object.assign(demo.lessons[0], {
      lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
      lessonNumber: 1,
      inProgress: true,
    });
    await route.fulfill({ response: res, json: list });
  });

  await page.goto("/#/c/demo");
  const next = page.getByTestId("next-action");
  const review = page.getByTestId("review-action");
  await expect(next).toBeVisible();
  await expect(next).toContainText("Continua");
  await expect(next).toContainText("Il primo passo");
  await expect(review).toBeVisible();
  await expect(review).toContainText("Ripasso");
  await expect(review).toContainText("Apri Anki per ripassare"); // Anki offline in e2e; waits out the "…"
  await page.evaluate(async () => {
    await document.fonts.ready;
  });

  const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  await page.screenshot({ path: path.join(repo, "docs/screenshot.png") });
});
