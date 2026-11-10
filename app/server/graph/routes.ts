// Map routes: GET /api/graph, POST /api/graph/positions, POST /api/graph/nodes/:id/delete.
// /api/graph/sources and /api/graph/extract live in extract.ts.
import type { Hono } from "hono";
import { emitGlobal } from "../events.ts";
import type { JobManager } from "../jobs/index.ts";
import { deleteNode, ensureLayout, nextRevision, readSnapshot, savePositions } from "./store.ts";

export function mountGraph(app: Hono, deps: { repoRoot: string; dataRoot?: string; jobs: JobManager }): void {
  const root = deps.dataRoot ?? deps.repoRoot; // graph/ lives in the data folder
  app.get("/api/graph", async (c) => {
    await ensureLayout(root);
    return c.json(await readSnapshot(root));
  });

  app.post("/api/graph/positions", async (c) => {
    const body = (await c.req.json().catch(() => null)) as { positions?: Record<string, { x: number; y: number }> } | null;
    if (!body?.positions || typeof body.positions !== "object") return c.json({ error: "positions mancante" }, 400);
    await savePositions(root, body.positions); // positions are rebuildable: no event
    return c.json({ ok: true });
  });

  app.post("/api/graph/nodes/:id/delete", async (c) => {
    const id = c.req.param("id");
    if (!/^(n|arg)_[\w-]+$/.test(id)) return c.json({ error: "id non valido" }, 400);
    await deleteNode(root, id);
    emitGlobal({ type: "graph", revision: nextRevision() });
    return c.json({ ok: true });
  });
}
