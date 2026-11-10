// Launches the real desktop shell with a fake claude and a temp copy of the fixture repo; asserts the app renders,
// the server answers, and quitting leaves no process behind. Run: npm run desktop:smoke
import { _electron } from "playwright-core";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_ID, APP_NAME } from "../shared/identity.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "learner-desktop-"));
const repo = path.join(tmp, "repo");
fs.cpSync(path.join(root, "app", "test", "fixtures", "repo"), repo, { recursive: true });

const alive = (pid) => execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH"], { encoding: "utf8" }).includes(String(pid));
const fail = (m) => { throw new Error("SMOKE FAIL: " + m); };

const env = {
  ...process.env,
  LEARNER_REPO_ROOT: repo,
  LEARNER_DATA_ROOT: repo,
  LEARNER_PORT: "0",
  LEARNER_USER_DATA: path.join(tmp, "userdata"),
  LEARNER_CLAUDE_BIN: process.execPath, // node + fake-claude: the real claude is never run
  LEARNER_CLAUDE_BIN_ARGS: JSON.stringify([path.join(root, "app", "test", "fake-claude.mjs")]),
};
delete env.ELECTRON_RUN_AS_NODE;

let electronApp, pid;
try {
  electronApp = await _electron.launch({
    executablePath: path.join(root, "node_modules", "electron", "dist", "electron.exe"),
    args: [path.join(here, "main.mjs")],
    env,
    timeout: 240000, // first run may build the UI
  });
  pid = electronApp.process().pid;
  const deadline = Date.now() + 240000;
  let page;
  while (!page && Date.now() < deadline) {
    page = electronApp.windows().find((w) => /^http:\/\/127\.0\.0\.1:\d+/.test(w.url()));
    if (!page) await new Promise((r) => setTimeout(r, 300));
  }
  if (!page) fail("no app window appeared");
  await page.waitForFunction(() => (document.getElementById("root")?.children.length ?? 0) > 0, null, { timeout: 30000 });
  const courses = await page.evaluate(() => fetch("/api/courses").then((r) => r.json()));
  if (!Array.isArray(courses)) fail("/api/courses did not return a list");
  const title = await page.title();
  console.log(`ok: window ${page.url()} title=${JSON.stringify(title)} courses=${courses.length}`);
  if (title !== APP_NAME) fail(`window title ${JSON.stringify(title)} != ${APP_NAME}`);
  const name = await electronApp.evaluate(({ app }) => app.getName());
  if (name !== APP_ID) fail(`app name ${name} != ${APP_ID}`);
  // the native window controls follow the app's palette and mode (startup, live, persisted)
  const cssBg = () => page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--background").trim().toLowerCase());
  const mainBg = () =>
    electronApp.evaluate(({ BrowserWindow }) => {
      const w = BrowserWindow.getAllWindows().find((x) => !x.isDestroyed() && x.webContents.getURL().startsWith("http://127.0.0.1"));
      return w ? w.getBackgroundColor().toLowerCase() : null;
    });
  const allBgs = () => electronApp.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().filter((x) => !x.isDestroyed()).map((x) => x.getBackgroundColor().toLowerCase()));

  // Playwright emulates prefers-color-scheme: light in the renderer, while main resolves "system" from the real OS
  // (nativeTheme). In the real app both follow the OS: make the renderer see the OS mode too, as it would.
  const osDark = await electronApp.evaluate(({ nativeTheme }) => nativeTheme.shouldUseDarkColors);
  await page.emulateMedia({ colorScheme: osDark ? "dark" : "light" });
  await page.waitForFunction((d) => document.documentElement.dataset.theme === (d ? "dark" : "light"), osDark);
  const startCss = await cssBg();
  const startBg = await mainBg();
  if (!startBg || startBg !== startCss) fail(`startup window colour ${startBg} != --background ${startCss}`);
  console.log(`ok: startup window colour matches --background (${startCss})`);

  await page.evaluate(() => { localStorage.setItem("ia.mode", "light"); localStorage.setItem("ia.palette", "ocean"); }); // as the app does: the renderer applies it and reports chrome
  await page.reload();
  await page.waitForFunction(() => (document.getElementById("root")?.children.length ?? 0) > 0, null, { timeout: 30000 });
  const liveCss = await cssBg();
  const liveDeadline = Date.now() + 5000;
  let bgs = await allBgs();
  while (Date.now() < liveDeadline && !(bgs.length > 0 && bgs.every((c) => c === liveCss))) {
    await new Promise((r) => setTimeout(r, 200));
    bgs = await allBgs();
  }
  if (bgs.length === 0 || !bgs.every((c) => c === liveCss)) fail(`live change did not recolour every window: ${bgs} != --background ${liveCss}`);
  console.log(`ok: live change recolours every window (${liveCss})`);

  const saved = JSON.parse(fs.readFileSync(path.join(env.LEARNER_USER_DATA, "window.json"), "utf8")).chrome;
  if (saved?.modePref !== "light" || saved?.light?.color !== liveCss) fail(`window.json chrome not persisted: ${JSON.stringify(saved)} for ${liveCss}`);
  console.log("ok: chrome persisted in window.json");

  // a theme change made from inside the Settings dialog (modal open) recolours the native controls too
  await page.getByRole("button", { name: "Impostazioni", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Impostazioni" });
  await dialog.getByRole("radiogroup", { name: "Temi" }).getByRole("radio", { name: "Iris" }).click();
  await page.waitForFunction(() => document.documentElement.dataset.palette === "iris");
  const modalCss = await cssBg();
  const modalDeadline = Date.now() + 5000;
  let modalBgs = await allBgs();
  while (Date.now() < modalDeadline && !(modalBgs.length > 0 && modalBgs.every((c) => c === modalCss))) {
    await new Promise((r) => setTimeout(r, 200));
    modalBgs = await allBgs();
  }
  if (modalBgs.length === 0 || !modalBgs.every((c) => c === modalCss)) fail(`modal theme change did not recolour every window: ${modalBgs} != --background ${modalCss}`);
  const savedModal = JSON.parse(fs.readFileSync(path.join(env.LEARNER_USER_DATA, "window.json"), "utf8")).chrome;
  if (savedModal?.light?.color !== modalCss) fail(`window.json not updated after modal theme change: ${JSON.stringify(savedModal)} for ${modalCss}`);
  console.log(`ok: theme change inside the open Settings dialog recolours every window (${modalCss})`);

  await electronApp.close();
  electronApp = undefined;
  await new Promise((r) => setTimeout(r, 1000));
  if (alive(pid)) fail(`electron pid ${pid} still running after quit`);
  console.log("ok: clean quit, no leftover process");
} finally {
  if (electronApp) await electronApp.close().catch(() => {});
  if (pid && alive(pid)) execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
  fs.rmSync(tmp, { recursive: true, force: true });
}
