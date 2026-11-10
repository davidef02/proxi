import { expect, test, type Page } from "@playwright/test";
import * as fs from "node:fs";
import * as path from "node:path";
import type { AnkiStatusResponse, CourseLesson, CourseOverview } from "../shared/protocol.ts";

// The sidebar reads courses and Anki stats from DataProvider: mock both so no live Anki or
// real lesson state can change what these tests see.
const LESSONS: CourseOverview[] = [
  {
    slug: "demo",
    title: "Demo",
    lessons: [
      { slug: "il-primo-passo", title: "Il primo passo", lessonPath: "courses/demo/lessons/01-il-primo-passo.html" },
      { slug: "il-secondo-passo", title: "Il secondo passo", lessonPath: "courses/demo/lessons/02-il-secondo-passo.html", inProgress: true },
      { slug: "il-terzo-passo", title: "Il terzo passo" },
    ],
    orphans: [],
    hasExercises: false,
    hasMission: true,
  },
  {
    slug: "zero-due",
    title: "Zero Due",
    lessons: [
      { slug: "uno", title: "Uno" },
      { slug: "due", title: "Due" },
    ],
    orphans: [],
    hasExercises: false,
    hasMission: true,
  },
  {
    slug: "big",
    title: "Corso Lungo",
    // 21 lessons: the grid caps at 20, so the row falls back to the numeric fraction.
    lessons: Array.from({ length: 21 }, (_, i) => ({
      slug: `lezione-${i + 1}`,
      title: `Lezione ${i + 1}`,
      lessonPath: i === 0 ? "courses/big/lessons/01-lezione-1.html" : undefined,
    })),
    orphans: [],
    hasExercises: false,
    hasMission: true,
  },
];

const ANKI_ONLINE: AnkiStatusResponse = {
  online: true,
  due: 5,
  learning: 2,
  newCards: 3,
  total: 20,
  byCourse: {
    demo: { due: 4, learning: 2, newCards: 3 },
    "zero-due": { due: 0, learning: 0, newCards: 5 },
    big: { due: 0, learning: 0, newCards: 0 },
  },
};

async function mockSidebar(page: Page, anki: AnkiStatusResponse) {
  await page.route("**/api/courses", (route) => route.fulfill({ json: LESSONS }));
  await page.route("**/api/anki", (route) => route.fulfill({ json: anki }));
}

const nav = (page: Page) => page.getByRole("complementary", { name: "Navigazione" });

test("sidebar shows lesson micro-grid, tooltips and the Anki due badge", async ({ page }) => {
  await mockSidebar(page, ANKI_ONLINE);
  await page.goto("/#/");

  // the link's accessible name still starts with the course title
  await expect(nav(page).getByRole("link", { name: /^Demo/ })).toBeVisible();

  const demo = nav(page).locator('a[href="#/c/demo"]');
  const grid = demo.getByTestId("lesson-grid");
  await expect(grid).toBeVisible();
  await expect(grid).toHaveAttribute("title", "1/3 lezioni fatte · in corso: Il secondo passo");
  await expect(grid).toHaveAttribute("aria-label", "1/3 lezioni fatte · in corso: Il secondo passo");

  const squares = grid.locator("[data-lesson-state]");
  await expect(squares).toHaveCount(3);
  await expect(squares.nth(0)).toHaveAttribute("data-lesson-state", "done");
  await expect(squares.nth(0)).toHaveClass(/bg-good/);
  await expect(squares.nth(1)).toHaveAttribute("data-lesson-state", "inProgress");
  await expect(squares.nth(1)).toHaveClass(/bg-warn/);
  await expect(squares.nth(1)).toHaveClass(/animate-pulse/);
  await expect(squares.nth(2)).toHaveAttribute("data-lesson-state", "todo");
  await expect(squares.nth(2)).toHaveClass(/bg-border/);

  const badge = demo.getByTestId("anki-due-badge");
  await expect(badge).toBeVisible();
  await expect(badge).toHaveText("6"); // due 4 + learning 2
  await expect(badge).toHaveAttribute("title", "6 card da ripassare oggi");
  await expect(badge).toHaveAttribute("aria-label", "6 card da ripassare oggi");

  // due + learning === 0: no badge, even though 5 new cards exist
  const zero = nav(page).locator('a[href="#/c/zero-due"]');
  const zeroGrid = zero.getByTestId("lesson-grid");
  await expect(zeroGrid).toHaveAttribute("title", "0/2 lezioni fatte");
  await expect(zeroGrid.locator('[data-lesson-state="todo"]')).toHaveCount(2);
  await expect(zero.getByTestId("anki-due-badge")).toHaveCount(0);

  // more than 20 lessons: numeric fallback instead of an unbounded grid
  const big = nav(page).locator('a[href="#/c/big"]');
  await expect(big.getByTestId("lesson-grid")).toHaveCount(0);
  const fallback = big.getByTestId("lesson-progress-fallback");
  await expect(fallback).toBeVisible();
  await expect(fallback).toHaveText("1/21");
  await expect(fallback).toHaveAttribute("title", "1/21 lezioni fatte");
  await expect(big.getByTestId("anki-due-badge")).toHaveCount(0);
});

test("sidebar renders no Anki badge when Anki is offline", async ({ page }) => {
  await mockSidebar(page, { online: false, error: "offline" });
  await page.goto("/#/");

  await expect(nav(page).getByRole("link", { name: /^Demo/ })).toBeVisible();
  await expect(nav(page).locator('[data-testid="anki-due-badge"]')).toHaveCount(0);
});

// 20 lessons is still a grid (the cap is the last included size); 0 lessons have no squares at all.
const EDGE_CASES: CourseOverview[] = [
  {
    slug: "twenty",
    title: "Venti lezioni",
    lessons: Array.from({ length: 20 }, (_, i) => ({
      slug: `venti-${i + 1}`,
      title: `Lezione ${i + 1}`,
      lessonPath: i === 0 ? "courses/twenty/lessons/01-venti-1.html" : undefined,
    })),
    orphans: [],
    hasExercises: false,
    hasMission: true,
  },
  { slug: "empty", title: "Nessuna lezione", lessons: [], orphans: [], hasExercises: false, hasMission: true },
  {
    slug: "multi",
    title: "Due in corso",
    lessons: [
      { slug: "prima", title: "Prima", lessonPath: "courses/multi/lessons/01-prima.html" },
      { slug: "seconda", title: "Seconda", lessonPath: "courses/multi/lessons/02-seconda.html", inProgress: true },
      { slug: "terza", title: "Terza", lessonPath: "courses/multi/lessons/03-terza.html", inProgress: true },
      { slug: "quarta", title: "Quarta" },
    ],
    orphans: [],
    hasExercises: false,
    hasMission: true,
  },
];

const EDGE_CASES_ANKI: AnkiStatusResponse = {
  online: true,
  due: 1,
  learning: 0,
  newCards: 0,
  total: 1,
  // Only `twenty` has reviews due; `multi` has learning cards only (no reviews). `empty` is simply absent.
  byCourse: {
    twenty: { due: 1, learning: 0, newCards: 0 },
    multi: { due: 0, learning: 2, newCards: 0 },
  },
};

test("sidebar grid stops at 20 lessons, falls back at 0, lists in-progress lessons and counts learning cards", async ({ page }) => {
  await page.route("**/api/courses", (route) => route.fulfill({ json: EDGE_CASES }));
  await page.route("**/api/anki", (route) => route.fulfill({ json: EDGE_CASES_ANKI }));
  await page.goto("/#/");

  const twenty = nav(page).locator('a[href="#/c/twenty"]');
  const grid = twenty.getByTestId("lesson-grid");
  await expect(grid).toBeVisible();
  await expect(grid.locator("[data-lesson-state]")).toHaveCount(20);
  await expect(grid.locator('[data-lesson-state="done"]')).toHaveCount(1);
  await expect(grid).toHaveAttribute("title", "1/20 lezioni fatte");
  // the summary is exposed as an image, not just as a tooltip
  await expect(nav(page).getByRole("img", { name: "1/20 lezioni fatte" })).toBeVisible();
  // ten squares per row: the first ten share a row, the next ten wrap onto a second
  const tops = await grid.locator("[data-lesson-state]").evaluateAll((els) =>
    els.map((el) => Math.round(el.getBoundingClientRect().top))
  );
  expect(new Set(tops.slice(0, 10)).size).toBe(1);
  expect(new Set(tops.slice(10)).size).toBe(1);
  expect(tops[10]).toBeGreaterThan(tops[0]);
  // due alone is enough for the badge, and the tooltip stays singular for one card
  const badge = twenty.getByTestId("anki-due-badge");
  await expect(badge).toHaveText("1");
  await expect(badge).toHaveAttribute("title", "1 card da ripassare oggi");

  // a planned course with no lessons yet: no squares, just the (zero) fraction
  const empty = nav(page).locator('a[href="#/c/empty"]');
  await expect(empty.getByTestId("lesson-grid")).toHaveCount(0);
  const fallback = empty.getByTestId("lesson-progress-fallback");
  await expect(fallback).toHaveText("0/0");
  await expect(fallback).toHaveAttribute("title", "0/0 lezioni fatte");
  await expect(empty.getByTestId("anki-due-badge")).toHaveCount(0);

  // every in-progress lesson is named, in plan order
  const multi = nav(page).locator('a[href="#/c/multi"]');
  await expect(multi.getByTestId("lesson-grid")).toHaveAttribute(
    "title",
    "1/4 lezioni fatte · in corso: Seconda, Terza"
  );
  await expect(multi.getByTestId("lesson-grid").locator('[data-lesson-state="inProgress"]')).toHaveCount(2);
  // learning cards alone are due work too: the badge counts due + learning
  const multiBadge = multi.getByTestId("anki-due-badge");
  await expect(multiBadge).toHaveText("2");
  await expect(multiBadge).toHaveAttribute("title", "2 card da ripassare oggi");
});

test("a stale Anki badge clears when Anki closes and comes back when it reopens", async ({ page }) => {
  const anki: { value: AnkiStatusResponse } = { value: ANKI_ONLINE };
  let down = false;
  await page.route("**/api/courses", (route) => route.fulfill({ json: LESSONS }));
  await page.route("**/api/anki", (route) => (down ? route.abort() : route.fulfill({ json: anki.value })));

  await page.goto("/#/");
  const badge = nav(page).locator('a[href="#/c/demo"]').getByTestId("anki-due-badge");
  await expect(badge).toHaveText("6");

  // Anki closed while the app stayed open: the next refresh (window focus) must drop the old count.
  anki.value = { online: false, error: "offline" };
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(badge).toHaveCount(0);

  // Anki reopened, with fresh numbers.
  anki.value = { ...ANKI_ONLINE, byCourse: { demo: { due: 3, learning: 1, newCards: 3 } } };
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(badge).toHaveText("4");

  // A failed request clears them too, instead of keeping yesterday's numbers.
  down = true;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(badge).toHaveCount(0);
});

test("a courses event refetches the Anki stats", async ({ page }) => {
  // Pin the count before the event, then switch the mock: only a courses-event refetch can
  // pick up the new value. No focus, no reload, no other request happens in between.
  let after = false;
  await page.route("**/api/courses", (route) => route.fulfill({ json: LESSONS }));
  await page.route("**/api/anki", (route) => {
    const byCourse = after
      ? { demo: { due: 1, learning: 0, newCards: 0 } }
      : { demo: { due: 4, learning: 2, newCards: 3 } };
    return route.fulfill({ json: { ...ANKI_ONLINE, byCourse } });
  });

  await page.goto("/#/");
  const badge = nav(page).locator('a[href="#/c/demo"]').getByTestId("anki-due-badge");
  await expect(badge).toHaveText("6");
  // let the stream's own first connect/refetch settle before flipping the mock
  await page.waitForTimeout(500);
  after = true;

  // A real courses event from the running server: saving an exercise file touches courses.
  const settings = await (await page.request.get("/api/settings")).json();
  const course = path.join(settings.dataRoot, "courses", "zz-e2e-anki-event");
  fs.mkdirSync(path.join(course, "exercises", "touch"), { recursive: true });
  try {
    await page.request.post("/api/exercises/save", {
      headers: { "x-learner-client": "1" },
      data: { course: "zz-e2e-anki-event", name: "touch", path: "main.js", content: "// e2e\n" },
    });
    await expect(badge).toHaveText("1", { timeout: 4000 });
  } finally {
    fs.rmSync(course, { recursive: true, force: true });
  }
});

// Saved and in-progress standalone lessons live in their own section, never under Corsi.
const STANDALONE: CourseLesson[] = [
  {
    slug: "trasformata-di-fourier",
    title: "La trasformata di Fourier scompone un segnale",
    lessonPath: "courses/lezioni-singole/lessons/0001-trasformata-di-fourier.html",
    inProgress: false,
  },
  {
    slug: "entropia-di-shannon",
    title: "L'entropia di Shannon misura la sorpresa media",
    lessonPath: "courses/lezioni-singole/lessons/0002-entropia-di-shannon.html",
    inProgress: true,
  },
];

test("sidebar renders Lezioni singole section with saved/in-progress standalone lessons and inline topic input launches a free teach session", async ({ page }) => {
  await mockSidebar(page, ANKI_ONLINE);
  await page.route("**/api/standalone-lessons", (route) => route.fulfill({ json: STANDALONE }));
  // let the server create the session, but keep the request body to check what the sidebar sent
  let body: unknown;
  await page.route("**/api/sessions", (route) => {
    if (route.request().method() === "POST") body = route.request().postDataJSON();
    return route.continue();
  });

  await page.goto("/#/");
  await expect(nav(page).getByRole("heading", { name: "Lezioni singole" })).toBeVisible();
  // the container is a reserved name: it never shows up in the structured course list
  await expect(nav(page).locator('a[href="#/c/lezioni-singole"]')).toHaveCount(0);

  const section = nav(page).getByTestId("standalone-lessons-section");
  const fourier = section.locator('a[href="#/l/courses%2Flezioni-singole%2Flessons%2F0001-trasformata-di-fourier.html"]');
  const entropy = section.locator('a[href="#/l/courses%2Flezioni-singole%2Flessons%2F0002-entropia-di-shannon.html"]');
  await expect(fourier).toBeVisible();
  await expect(entropy).toBeVisible();

  const doneDot = fourier.locator("[data-standalone-state]");
  await expect(doneDot).toHaveAttribute("data-standalone-state", "done");
  await expect(doneDot).toHaveClass(/bg-good/);
  const progressDot = entropy.locator("[data-standalone-state]");
  await expect(progressDot).toHaveAttribute("data-standalone-state", "inProgress");
  await expect(progressDot).toHaveClass(/bg-warn/);
  await expect(progressDot).toHaveClass(/animate-pulse/);

  const input = section.getByLabel("Argomento della lezione singola");
  await input.fill("Teorema di Bayes");
  await input.press("Enter");

  await expect(page).toHaveURL(/#\/s\//);
  await expect.poll(() => body).toEqual({
    kind: "teach",
    course: "lezioni-singole",
    target: { mode: "free" },
    note: "Teorema di Bayes",
  });
});

test("a slow, older Anki reply never overwrites a newer one", async ({ page }) => {
  let calls = 0;
  await page.route("**/api/courses", (route) => route.fulfill({ json: LESSONS }));
  await page.route("**/api/anki", async (route) => {
    const n = ++calls;
    if (n === 1) await new Promise((resolve) => setTimeout(resolve, 1500));
    const byCourse = n === 1
      ? { demo: { due: 4, learning: 2, newCards: 3 } }
      : { demo: { due: 1, learning: 0, newCards: 0 } };
    return route.fulfill({ json: { ...ANKI_ONLINE, byCourse } });
  });

  await page.goto("/#/");
  await page.waitForRequest("**/api/anki");
  // a second load starts before the first reply lands
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));

  const badge = nav(page).locator('a[href="#/c/demo"]').getByTestId("anki-due-badge");
  await expect(badge).toHaveText("1");
  // the stale first reply arrives now: it must be dropped, not restore the old count
  await page.waitForTimeout(1500);
  await expect(badge).toHaveText("1");
});
