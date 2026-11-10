import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import type { CourseOverview, MapNode } from "../shared/protocol.ts";

// Never click next-action unmocked: it starts a session on the shared server.
async function mockCourse(page: Page) {
  await page.route("**/api/courses", async (route) => {
    const res = await route.fetch();
    const list: CourseOverview[] = await res.json();
    const demo = list.find((c) => c.slug === "demo")!;
    demo.hasExercises = true;
    demo.orphans = [
      {
        title: "Approfondimento fuori piano",
        slug: "approfondimento-fuori-piano",
        lessonPath: "courses/demo/lessons/99-approfondimento-fuori-piano.html",
      },
    ];
    await route.fulfill({ response: res, json: list });
  });

  // Midday UTC, so the it-IT date is "28 ottobre" in any timezone.
  const nodes: MapNode[] = [
    {
      key: "demo/il-primo-passo#N1",
      course: "demo",
      lessonSlug: "il-primo-passo",
      lessonTitle: "Il primo passo",
      lessonNumber: 1,
      lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
      nodeId: "N1",
      title: "Concetto 1",
      tier: "axiom",
      attempts: 1,
      status: "solid",
      lastAt: "2026-10-20T12:00:00.000Z",
    },
    {
      key: "demo/il-primo-passo#N2",
      course: "demo",
      lessonSlug: "il-primo-passo",
      lessonTitle: "Il primo passo",
      lessonNumber: 1,
      lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
      nodeId: "N2",
      title: "Concetto 2",
      tier: "axiom",
      attempts: 1,
      status: "solid",
      lastAt: "2026-10-28T12:00:00.000Z",
    },
    {
      key: "demo/il-primo-passo#N3",
      course: "demo",
      lessonSlug: "il-primo-passo",
      lessonTitle: "Il primo passo",
      lessonNumber: 1,
      lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
      nodeId: "N3",
      title: "Concetto 3",
      tier: "axiom",
      attempts: 1,
      status: "shaky",
      lastAt: "2026-10-25T12:00:00.000Z",
    },
  ];
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes, edges: [] } }));

  await page.route("**/api/exercises/demo", (route) =>
    route.fulfill({
      json: [
        { course: "demo", name: "01-combinazioni-lineari", dir: "courses/demo/exercises/01-combinazioni-lineari", files: [] },
        { course: "demo", name: "02-basi-e-sottospazi", dir: "courses/demo/exercises/02-basi-e-sottospazi", files: [], final: true },
      ],
    }),
  );
}

const fold = (page: Page, title: string) =>
  page.locator("details").filter({ has: page.locator("summary", { hasText: title }) });

test("course page: one progress count in the hero, exercises in the quiet row, secondary sections folded", async ({ page }) => {
  await mockCourse(page);
  await page.goto("/#/c/demo");

  // AC1: the count and the last study date live once, in the hero card.
  const hero = page.getByTestId("next-action");
  await expect(hero).toContainText("ultimo studio 28 ottobre");
  await expect(hero).toContainText(/\d+ fatte · ultimo studio 28 ottobre$/);
  await expect(page.getByText(/ultimo studio/)).toHaveCount(1);
  await expect(page.getByText(/lezioni fatte/)).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Lezioni", exact: true })).toHaveText("Lezioni");

  // AC2: exercises moved to the quiet action row.
  const quiet = page.getByTestId("quiet-actions");
  await expect(quiet.getByTestId("exercises-action")).toBeVisible();
  await expect(quiet.getByTestId("exercises-action")).toHaveText("Esercizi");
  await expect(quiet.getByRole("button", { name: "Lezione libera" })).toBeVisible();
  await expect(quiet.getByRole("button", { name: "Riorganizza corso" })).toBeVisible();

  // AC3: secondary sections are closed accordions that open on click.
  for (const [title, content] of [
    ["Fuori dal piano (1)", "Approfondimento fuori piano"],
    ["Esercizi (2)", "01-combinazioni-lineari"],
    ["Come stai andando (3 concetti)", "2 solidi"],
  ] as const) {
    const d = fold(page, title);
    await expect(d).toHaveCount(1);
    await expect(d).not.toHaveAttribute("open");
    await expect(d.getByText(content)).toBeHidden();
    await d.locator("summary").click();
    await expect(d).toHaveAttribute("open", "");
    await expect(d.getByText(content)).toBeVisible();
  }
  await expect(fold(page, "Come stai andando").getByText("1 da rinforzare")).toBeVisible();
  await expect(fold(page, "Esercizi").getByText("Chiedi esercizi o lasciali scegliere")).toBeVisible();
  await expect(fold(page, "Esercizi").getByText("finale")).toBeVisible();

  // The lessons list stays open, above every accordion.
  expect(
    await page.evaluate(() => {
      const h = [...document.querySelectorAll("h2")].find((e) => e.textContent === "Lezioni")!;
      const d = document.querySelector("details")!;
      return !!(h.compareDocumentPosition(d) & Node.DOCUMENT_POSITION_FOLLOWING);
    }),
  ).toBe(true);
});

test("AC1 (#49): one Esercizi action starts a practice session", async ({ page }) => {
  await mockCourse(page);
  // the session is never created on the shared server: capture the body, fake the id
  let body: unknown;
  await page.route("**/api/sessions", (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    body = route.request().postDataJSON();
    return route.fulfill({ json: { id: "e2e-fake" } });
  });
  await page.goto("/#/c/demo");

  await expect(page.getByRole("button", { name: "Esercizi", exact: true })).toHaveCount(1);
  await expect(page.getByTestId("exercises-action")).toHaveText("Esercizi");
  await expect(page.getByRole("button", { name: /ripasso in app|fammi fare esercizi/i })).toHaveCount(0);

  await page.getByTestId("exercises-action").click();

  await expect.poll(() => body).toMatchObject({ kind: "practice", course: "demo", practiceNodes: [] });
});

test("course page: no exercises, orphans or concepts → no accordions, exercise action still in the quiet row", async ({ page }) => {
  const ex = page.waitForResponse((r) => r.url().endsWith("/api/exercises/demo"));
  const map = page.waitForResponse((r) => r.url().endsWith("/api/map"));
  await page.goto("/#/c/demo");
  await ex;
  await map;

  await expect(page.getByTestId("quiet-actions").getByTestId("exercises-action")).toBeVisible();
  await expect(page.locator("details")).toHaveCount(0);
  await expect(page.locator("summary", { hasText: /^Esercizi/ })).toHaveCount(0);
  await expect(page.getByText(/ultimo studio/)).toHaveCount(0);
});

const altro: CourseOverview = { slug: "altro", title: "Altro corso", lessons: [], orphans: [], hasExercises: false, hasMission: false };

async function addAltro(page: Page) {
  await page.route("**/api/courses", async (route) => {
    const res = await route.fetch();
    const list: CourseOverview[] = await res.json();
    await route.fulfill({ response: res, json: [...list, altro] });
  });
}

test("course page in plan state: the date follows the plan count, one concept is singular", async ({ page }) => {
  await addAltro(page);
  const node: MapNode = {
    key: "altro/x#N1",
    course: "altro",
    lessonSlug: "x",
    lessonTitle: "X",
    lessonNumber: 1,
    lessonPath: "courses/altro/lessons/01-x.html",
    nodeId: "N1",
    title: "Concetto",
    tier: "axiom",
    attempts: 1,
    status: "solid",
    lastAt: "2026-10-28T12:00:00.000Z",
  };
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes: [node], edges: [] } }));
  await page.goto("/#/c/altro");

  await expect(page.getByTestId("next-action")).toContainText("0/0 lezioni fatte · ultimo studio 28 ottobre");
  await expect(page.getByText(/ultimo studio/)).toHaveCount(1);
  await expect(page.getByTestId("quiet-actions").getByTestId("exercises-action")).toBeVisible();
  await expect(fold(page, "Come stai andando (1 concetto)")).toHaveCount(1);
});

test("course page: switching to a course without exercises drops the previous course's exercises", async ({ page }) => {
  await page.route("**/api/exercises/demo", (route) =>
    route.fulfill({ json: [{ course: "demo", name: "01-combinazioni-lineari", dir: "courses/demo/exercises/01-combinazioni-lineari", files: [] }] }),
  );
  await page.route("**/api/courses", async (route) => {
    const res = await route.fetch();
    const list: CourseOverview[] = await res.json();
    list.find((c) => c.slug === "demo")!.hasExercises = true;
    await route.fulfill({ response: res, json: [...list, altro] });
  });
  await page.goto("/#/c/demo");
  await expect(fold(page, "Esercizi (1)")).toHaveCount(1);

  await page.goto("/#/c/altro");
  await expect(page.getByText("Altro corso").first()).toBeVisible();
  await expect(page.locator("summary", { hasText: /^Esercizi/ })).toHaveCount(0);
});

test("course page: an accordion opened on one course is closed on the next", async ({ page }) => {
  await addAltro(page);
  const node = (course: string): MapNode => ({
    key: `${course}/x#N1`,
    course,
    lessonSlug: "x",
    lessonTitle: "X",
    lessonNumber: 1,
    lessonPath: `courses/${course}/lessons/01-x.html`,
    nodeId: "N1",
    title: "Concetto",
    tier: "axiom",
    attempts: 1,
    status: "solid",
  });
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes: [node("demo"), node("altro")], edges: [] } }));
  await page.goto("/#/c/demo");
  const d = fold(page, "Come stai andando");
  await d.locator("summary").click();
  await expect(d).toHaveAttribute("open", "");

  await page.goto("/#/c/altro");
  await expect(page.getByText("Altro corso").first()).toBeVisible();
  await expect(fold(page, "Come stai andando (1 concetto)")).not.toHaveAttribute("open");
});

test("AC6: course page opens with Anki unreachable, triggers import, shows no error and renders normally", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(err.message));

  const importPromise = page.waitForResponse(
    (res) => res.url().includes("/api/anki/import") && res.request().method() === "POST",
  );

  await page.goto("/#/c/demo");

  const importRes = await importPromise;
  expect(importRes.status()).toBe(200);
  const importJson = await importRes.json();
  expect(importJson.online).toBe(false);

  await expect(page.getByRole("heading", { name: "Demo" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Lezioni", exact: true })).toBeVisible();

  await expect(page.locator(".border-red-500")).toHaveCount(0);
  expect(consoleErrors).toEqual([]);

  const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const evidencePath = path.join(repo, "docs/issues/46/evidence/AC6-course-page-anki-offline.png");
  await page.screenshot({ path: evidencePath, fullPage: true });
});

test("AC3: concepts to reinforce are listed by name and open their lesson", async ({ page }) => {
  await mockCourse(page);
  await page.goto("/#/c/demo");

  const r = page.getByTestId("reinforce");
  await expect(r).toBeVisible();
  await expect(r.getByRole("heading", { name: "Da rinforzare" })).toBeVisible();
  await expect(r.getByRole("link")).toHaveCount(1);
  await expect(r.getByRole("link")).toHaveText("Concetto 3");

  await r.getByRole("link").click();
  await expect(page).toHaveURL(/#\/l\/courses%2Fdemo%2Flessons%2F01-il-primo-passo\.html#node-N3$/);
});

test("AC4: no concept to reinforce → no reinforce section", async ({ page }) => {
  const solidNode: MapNode = {
    key: "demo/il-primo-passo#N1",
    course: "demo",
    lessonSlug: "il-primo-passo",
    lessonTitle: "Il primo passo",
    lessonNumber: 1,
    lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
    nodeId: "N1",
    title: "Concetto 1",
    tier: "axiom",
    attempts: 1,
    status: "solid",
    lastAt: "2026-10-20T12:00:00.000Z",
  };
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes: [solidNode], edges: [] } }));
  await page.route("**/api/exercises/demo", (route) => route.fulfill({ json: [] }));

  const mapRes = page.waitForResponse((r) => r.url().endsWith("/api/map"));
  await page.goto("/#/c/demo");
  await mapRes;

  await expect(page.getByTestId("reinforce")).toHaveCount(0);
  await expect(page.getByText("Da rinforzare")).toHaveCount(0);
});

test("AC3: every shaky concept is listed with its own lesson link, missed ones are not", async ({ page }) => {
  const node = (over: Partial<MapNode>): MapNode => ({
    key: "demo/x#N1",
    course: "demo",
    lessonSlug: "x",
    lessonTitle: "X",
    lessonNumber: 1,
    lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
    nodeId: "N1",
    title: "Concetto",
    tier: "axiom",
    attempts: 1,
    status: "shaky",
    lastAt: "2026-10-20T12:00:00.000Z",
    ...over,
  });
  const nodes: MapNode[] = [
    node({
      key: "demo/il-primo-passo#N1",
      lessonSlug: "il-primo-passo",
      lessonTitle: "Il primo passo",
      nodeId: "N1",
      title: "Primo concetto",
    }),
    node({
      key: "demo/il-secondo-passo#N2",
      lessonSlug: "il-secondo-passo",
      lessonTitle: "Il secondo passo",
      lessonNumber: 2,
      lessonPath: "courses/demo/lessons/02-il-secondo-passo.html",
      nodeId: "N2",
      title: "Secondo concetto",
    }),
    node({
      key: "demo/il-secondo-passo#N3",
      lessonSlug: "il-secondo-passo",
      lessonTitle: "Il secondo passo",
      lessonNumber: 2,
      lessonPath: "courses/demo/lessons/02-il-secondo-passo.html",
      nodeId: "N3",
      title: "Concetto mancato",
      status: "missed",
    }),
  ];
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes, edges: [] } }));
  await page.route("**/api/exercises/demo", (route) => route.fulfill({ json: [] }));

  const mapRes = page.waitForResponse((r) => r.url().endsWith("/api/map"));
  await page.goto("/#/c/demo");
  await mapRes;

  const r = page.getByTestId("reinforce");
  const links = r.getByRole("link");
  await expect(links).toHaveCount(2);
  await expect(links.nth(0)).toHaveText("Primo concetto");
  await expect(links.nth(0)).toHaveAttribute("href", "#/l/courses%2Fdemo%2Flessons%2F01-il-primo-passo.html#node-N1");
  await expect(links.nth(1)).toHaveText("Secondo concetto");
  await expect(links.nth(1)).toHaveAttribute("href", "#/l/courses%2Fdemo%2Flessons%2F02-il-secondo-passo.html#node-N2");
  await expect(r.getByText("Concetto mancato")).toHaveCount(0);
});

test("AC3: a shaky concept of another course is not listed", async ({ page }) => {
  const node = (over: Partial<MapNode>): MapNode => ({
    key: "demo/x#N1",
    course: "demo",
    lessonSlug: "x",
    lessonTitle: "X",
    lessonNumber: 1,
    lessonPath: "courses/demo/lessons/01-il-primo-passo.html",
    nodeId: "N1",
    title: "Concetto",
    tier: "axiom",
    attempts: 1,
    status: "shaky",
    lastAt: "2026-10-20T12:00:00.000Z",
    ...over,
  });
  const nodes: MapNode[] = [
    node({ title: "Concetto del corso" }),
    node({
      key: "altro/x#N1",
      course: "altro",
      lessonPath: "courses/altro/lessons/01-altro.html",
      title: "Concetto di un altro corso",
    }),
  ];
  await page.route("**/api/map", (route) => route.fulfill({ json: { nodes, edges: [] } }));
  await page.route("**/api/exercises/demo", (route) => route.fulfill({ json: [] }));

  const mapRes = page.waitForResponse((r) => r.url().endsWith("/api/map"));
  await page.goto("/#/c/demo");
  await mapRes;

  const links = page.getByTestId("reinforce").getByRole("link");
  await expect(links).toHaveCount(1);
  await expect(links).toHaveText("Concetto del corso");
});

