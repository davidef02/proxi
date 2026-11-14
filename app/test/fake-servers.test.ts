import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FakeHttpServer, startFakeServers, type RecordedRequest } from "./fake-servers.ts";

describe("FakeHttpServer", () => {
  it("answers a JSON route with status, body and headers, and records the request", async () => {
    const fake = new FakeHttpServer("demo");
    const url = await fake.start();
    try {
      fake.on("POST", "/echo", { status: 201, headers: { "x-fake": "1" }, body: { ok: true } });
      const res = await fetch(url + "/echo?a=1", {
        method: "POST",
        headers: { "x-test": "yes", "content-type": "application/json" },
        body: '{"in":1}',
      });
      assert.equal(res.status, 201);
      assert.equal(res.headers.get("content-type"), "application/json");
      assert.equal(res.headers.get("x-fake"), "1");
      assert.deepEqual(await res.json(), { ok: true });

      assert.equal(fake.requests.length, 1);
      const req = fake.requests[0];
      assert.equal(req.method, "POST");
      assert.equal(req.path, "/echo?a=1");
      assert.equal(req.headers["x-test"], "yes");
      assert.equal(req.body, '{"in":1}');
    } finally {
      await fake.close();
    }
  });

  it("passes the recorded request to a handler route", async () => {
    const fake = new FakeHttpServer("demo");
    const url = await fake.start();
    let seen: RecordedRequest | undefined;
    try {
      fake.on("POST", "/hook", (req) => {
        seen = req;
        return { status: 202, body: { got: req.body } };
      });
      const res = await fetch(url + "/hook?x=1", { method: "POST", body: "hello" });
      assert.equal(res.status, 202);
      assert.deepEqual(await res.json(), { got: "hello" });
      assert.ok(seen);
      assert.equal(seen.method, "POST");
      assert.equal(seen.path, "/hook?x=1");
      assert.equal(seen.body, "hello");
    } finally {
      await fake.close();
    }
  });

  it("answers 404 with the fake's name when no route matches", async () => {
    const fake = new FakeHttpServer("demo");
    const url = await fake.start();
    try {
      fake.on("GET", "/known", { body: {} });
      const res = await fetch(url + "/unknown");
      assert.equal(res.status, 404);
      const body = (await res.json()) as { error: string };
      assert.match(body.error, /fake demo/);
      assert.match(body.error, /no route for GET \/unknown/);
    } finally {
      await fake.close();
    }
  });

  it("fallback answers unmatched paths, exact routes win, reset clears it", async () => {
    const fake = new FakeHttpServer("demo");
    const url = await fake.start();
    try {
      fake.on("GET", "/exact", { body: { from: "route" } });
      fake.onFallback((req) => {
        if (req.path === "/boom") throw new Error("fallback boom");
        return { status: 418, body: { from: "fallback", path: req.path } };
      });

      const exact = await fetch(url + "/exact");
      assert.equal(exact.status, 200);
      assert.deepEqual(await exact.json(), { from: "route" });

      const other = await fetch(url + "/other?x=1");
      assert.equal(other.status, 418);
      assert.deepEqual(await other.json(), { from: "fallback", path: "/other?x=1" });
      assert.equal(fake.requests.length, 2);
      assert.equal(fake.requests[1].path, "/other?x=1");

      assert.equal((await fetch(url + "/boom")).status, 500);
      assert.equal(fake.requests.length, 3);

      fake.reset();
      assert.equal(fake.requests.length, 0);
      assert.equal((await fetch(url + "/other")).status, 404);
    } finally {
      await fake.close();
    }
  });

  it("reset clears routes and requests", async () => {
    const fake = new FakeHttpServer("demo");
    const url = await fake.start();
    try {
      fake.on("GET", "/known", { body: {} });
      await fetch(url + "/known");
      assert.equal(fake.requests.length, 1);

      fake.reset();
      assert.equal(fake.requests.length, 0);
      assert.equal((await fetch(url + "/known")).status, 404);
    } finally {
      await fake.close();
    }
  });
});

describe("startFakeServers", () => {
  it("starts five distinct fakes, answers the Anki version, and closes them all", async () => {
    const fakes = await startFakeServers();
    let githubUrl: string;
    try {
      const urls = Object.values(fakes.urls);
      assert.equal(new Set(urls).size, 5);
      for (const url of urls) assert.match(url, /^http:\/\/127\.0\.0\.1:\d+$/);
      githubUrl = fakes.urls.github;

      const res = await fetch(fakes.urls.anki, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ action: "version", version: 6 }),
      });
      assert.deepEqual(await res.json(), { result: 6, error: null });
    } finally {
      await fakes.close();
    }
    await assert.rejects(fetch(githubUrl + "/repos/a/b"));
  });
});
