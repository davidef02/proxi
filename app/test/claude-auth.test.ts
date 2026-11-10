import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { DEFAULT_SETTINGS, type SessionStatus } from "../shared/protocol.ts";
import { maskApiKey, readSettings, writeSettings } from "../server/settings.ts";
import { createServer } from "../server/main.ts";
import { spawnClaude, CLAUDE_AUTH_ERROR_MESSAGE, CLAUDE_AUTH_ERROR_RE } from "../server/claude.ts";
import { Turns, type TurnHost } from "../server/turns.ts";
import type { EngineLaunch, EngineCallbacks, EngineHandle } from "../server/types.ts";
import { subscribeGlobal } from "../server/events.ts";

describe("claude auth settings and engine integration", () => {
  it("default settings include claudeAuth: 'subscription' and anthropicApiKey: ''", () => {
    assert.equal(DEFAULT_SETTINGS.claudeAuth, "subscription");
    assert.equal(DEFAULT_SETTINGS.anthropicApiKey, "");

    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-defaults-"));
    try {
      const s = readSettings(tmp);
      assert.equal(s.claudeAuth, "subscription");
      assert.equal(s.anthropicApiKey, "");
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("maskApiKey masking behavior", () => {
    assert.equal(maskApiKey(undefined), "");
    assert.equal(maskApiKey(""), "");
    assert.equal(maskApiKey("   "), "");
    assert.equal(maskApiKey("12345"), "••••••••");
    assert.equal(maskApiKey("1234567890"), "••••••••");
    assert.equal(maskApiKey("sk-ant-api03-1234567890"), "sk-ant-…7890");
    assert.equal(maskApiKey("sk-ant-1234"), "sk-ant-…1234");
    assert.equal(maskApiKey("   sk-ant-1234   "), "sk-ant-…1234");
  });

  it("readSettings and writeSettings with claudeAuth and anthropicApiKey", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-settings-"));
    try {
      writeSettings(tmp, { claudeAuth: "apiKey", anthropicApiKey: "sk-ant-test-123456789" });
      let s = readSettings(tmp);
      assert.equal(s.claudeAuth, "apiKey");
      assert.equal(s.anthropicApiKey, "sk-ant-test-123456789");

      writeSettings(tmp, { claudeAuth: "subscription" });
      s = readSettings(tmp);
      assert.equal(s.claudeAuth, "subscription");
      assert.equal(s.anthropicApiKey, "sk-ant-test-123456789");
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("preserves existing key when mask is sent back in patch", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-mask-"));
    try {
      const realKey = "sk-ant-api03-real-secret-key-12345";
      writeSettings(tmp, { anthropicApiKey: realKey });
      const masked = maskApiKey(realKey);
      assert.ok(masked.includes("…"));

      // Sending back masked key preserves existing key
      writeSettings(tmp, { anthropicApiKey: masked });
      assert.equal(readSettings(tmp).anthropicApiKey, realKey);

      // Sending back bullet mask preserves existing key
      writeSettings(tmp, { anthropicApiKey: "••••••••" });
      assert.equal(readSettings(tmp).anthropicApiKey, realKey);

      // Sending an empty string clears the key
      writeSettings(tmp, { anthropicApiKey: "" });
      assert.equal(readSettings(tmp).anthropicApiKey, "");
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("GET /api/settings response does not expose cleartext key", async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-srv-"));
    fs.mkdirSync(path.join(tmp, "courses"), { recursive: true });
    fs.mkdirSync(path.join(tmp, "assets"), { recursive: true });
    writeSettings(tmp, { claudeAuth: "apiKey", anthropicApiKey: "sk-ant-api03-super-secret-key-xyz" });

    const srv = await createServer({ repoRoot: tmp, dataRoot: tmp, port: 0, openBrowser: false });
    try {
      const res = await fetch(`${srv.url}/api/settings`);
      const body = (await res.json()) as any;
      assert.equal(body.claudeAuth, "apiKey");
      assert.equal(body.anthropicApiKey, maskApiKey("sk-ant-api03-super-secret-key-xyz"));
      assert.ok(!JSON.stringify(body).includes("super-secret"));

      // POST /api/settings with a new key also returns masked key
      const postRes = await fetch(`${srv.url}/api/settings`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-learner-client": "1" },
        body: JSON.stringify({ anthropicApiKey: "sk-ant-api03-another-secret-token-987" }),
      });
      const postBody = (await postRes.json()) as any;
      assert.equal(postBody.anthropicApiKey, maskApiKey("sk-ant-api03-another-secret-token-987"));
      assert.ok(!JSON.stringify(postBody).includes("another-secret"));
      assert.equal(readSettings(tmp).anthropicApiKey, "sk-ant-api03-another-secret-token-987");
    } finally {
      await srv.close();
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("GET /api/limits returns null when claudeAuth === 'apiKey'", async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-limits-"));
    fs.mkdirSync(path.join(tmp, "courses"), { recursive: true });
    fs.mkdirSync(path.join(tmp, "assets"), { recursive: true });
    const limitsFile = path.join(tmp, ".learner", "limits.json");
    fs.mkdirSync(path.dirname(limitsFile), { recursive: true });
    fs.writeFileSync(limitsFile, JSON.stringify({ fiveHour: { utilization: 0.5, resetsAt: 12345 } }));

    writeSettings(tmp, { claudeAuth: "subscription", provider: "claude" });
    const srv = await createServer({ repoRoot: tmp, dataRoot: tmp, port: 0, openBrowser: false });
    try {
      // Under subscription, limits are returned
      let res = await fetch(`${srv.url}/api/limits`);
      let data = (await res.json()) as any;
      assert.notEqual(data, null);
      assert.equal(data.fiveHour?.utilization, 0.5);

      const events: any[] = [];
      const unsub = subscribeGlobal((e) => {
        if (e.type === "limits") events.push(e);
      });

      // Switch to apiKey: clears UI quota meter and GET /api/limits becomes null
      await fetch(`${srv.url}/api/settings`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-learner-client": "1" },
        body: JSON.stringify({ claudeAuth: "apiKey" }),
      });

      res = await fetch(`${srv.url}/api/limits`);
      data = await res.json();
      assert.equal(data, null);

      assert.ok(events.length > 0);
      const last = events.at(-1);
      assert.equal(last.limits.fiveHour, undefined);
      assert.ok(last.limits.at);

      unsub();
    } finally {
      await srv.close();
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  describe("spawnClaude", () => {
    it("passes LEARNER_DATA_ROOT to the child env when dataRoot is set", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-spawn-dataroot-"));
      const script = path.join(tmp, "echo-env.mjs");
      fs.writeFileSync(script, `console.error("DATA_ROOT=" + (process.env.LEARNER_DATA_ROOT ?? "UNSET")); process.exit(0);`);
      writeSettings(tmp, { claudeAuth: "subscription" });
      try {
        const dataRoot = path.join(tmp, "data");
        const launch: EngineLaunch = {
          provider: "claude",
          bin: { cmd: process.execPath, args: [script] },
          repoRoot: tmp,
          dataRoot,
          scratchDir: path.join(tmp, "scratch"),
          model: "haiku",
          systemPrompt: "sys",
          grant: [],
          mcp: { name: "app", url: "http://127.0.0.1/mcp", token: "tok" },
          addDirs: [],
        };
        const stderr: string[] = [];
        await new Promise<void>((resolve) => {
          spawnClaude(launch, {
            onEvent: () => {},
            onStderr: (line) => stderr.push(line),
            onExit: () => resolve(),
          });
        });
        assert.ok(stderr.some((l) => l.includes(`DATA_ROOT=${dataRoot}`)));
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    it("when claudeAuth === 'subscription', deletes ANTHROPIC_API_KEY from child env", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-spawn-sub-"));
      const script = path.join(tmp, "echo-env.mjs");
      fs.writeFileSync(script, `console.error("ENV_KEY=" + (process.env.ANTHROPIC_API_KEY ?? "DELETED")); process.exit(0);`);

      writeSettings(tmp, { claudeAuth: "subscription" });

      const prevAmbient = process.env.ANTHROPIC_API_KEY;
      process.env.ANTHROPIC_API_KEY = "ambient-key-that-must-be-deleted";
      try {
        const launch: EngineLaunch = {
          provider: "claude",
          bin: { cmd: process.execPath, args: [script] },
          repoRoot: tmp,
          scratchDir: path.join(tmp, "scratch"),
          model: "haiku",
          systemPrompt: "sys",
          grant: [],
          mcp: { name: "app", url: "http://127.0.0.1/mcp", token: "tok" },
          addDirs: [],
        };

        const stderr: string[] = [];
        let exitCode: number | null = null;
        await new Promise<void>((resolve) => {
          spawnClaude(launch, {
            onEvent: () => {},
            onStderr: (line) => stderr.push(line),
            onExit: (code) => {
              exitCode = code;
              resolve();
            },
          });
        });

        assert.equal(exitCode, 0);
        assert.ok(stderr.some((l) => l.includes("ENV_KEY=DELETED")));
        assert.ok(!stderr.some((l) => l.includes("ambient-key-that-must-be-deleted")));
      } finally {
        if (prevAmbient !== undefined) process.env.ANTHROPIC_API_KEY = prevAmbient;
        else delete process.env.ANTHROPIC_API_KEY;
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    it("when claudeAuth === 'apiKey' with key, passes it in child env", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-spawn-key-"));
      const script = path.join(tmp, "echo-env.mjs");
      fs.writeFileSync(script, `console.error("ENV_KEY=" + (process.env.ANTHROPIC_API_KEY ?? "NONE")); process.exit(0);`);

      const secretKey = "sk-ant-api03-spawn-secret-123";
      writeSettings(tmp, { claudeAuth: "apiKey", anthropicApiKey: secretKey });

      try {
        const launch: EngineLaunch = {
          provider: "claude",
          bin: { cmd: process.execPath, args: [script] },
          repoRoot: tmp,
          scratchDir: path.join(tmp, "scratch"),
          model: "haiku",
          systemPrompt: "sys",
          grant: [],
          mcp: { name: "app", url: "http://127.0.0.1/mcp", token: "tok" },
          addDirs: [],
        };

        const stderr: string[] = [];
        let exitCode: number | null = null;
        await new Promise<void>((resolve) => {
          spawnClaude(launch, {
            onEvent: () => {},
            onStderr: (line) => stderr.push(line),
            onExit: (code) => {
              exitCode = code;
              resolve();
            },
          });
        });

        assert.equal(exitCode, 0);
        assert.ok(stderr.some((l) => l.includes(`ENV_KEY=${secretKey}`)));
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    it("when claudeAuth === 'apiKey' without key, exits with code 1 and readable error message", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-auth-spawn-nokey-"));
      writeSettings(tmp, { claudeAuth: "apiKey", anthropicApiKey: "" });

      try {
        const launch: EngineLaunch = {
          provider: "claude",
          bin: { cmd: process.execPath, args: [] },
          repoRoot: tmp,
          scratchDir: path.join(tmp, "scratch"),
          model: "haiku",
          systemPrompt: "sys",
          grant: [],
          mcp: { name: "app", url: "http://127.0.0.1/mcp", token: "tok" },
          addDirs: [],
        };

        const stderr: string[] = [];
        let exitCode: number | null = null;
        let exitSignal: string | null = "non-null";
        const handle = await new Promise<ReturnType<typeof spawnClaude>>((resolve) => {
          const h = spawnClaude(launch, {
            onEvent: () => {},
            onStderr: (line) => stderr.push(line),
            onExit: (code, signal) => {
              exitCode = code;
              exitSignal = signal;
              resolve(h);
            },
          });
        });

        assert.equal(handle.pid, undefined);
        assert.equal(exitCode, 1);
        assert.equal(exitSignal, null);
        assert.ok(stderr.some((l) => l.includes("Nessuna API key Anthropic configurata nelle Impostazioni.")));
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });

  describe("Claude auth error detection (Issue #22)", () => {
    it("detects various CLI authentication failure strings with CLAUDE_AUTH_ERROR_RE", () => {
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("Not logged in · Please run /login"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("Error: authentication required"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("Please run claude login to continue"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("execute `claude login`"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("API error: authentication_failed"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("oauth_org_not_allowed: forbidden"));
      assert.ok(CLAUDE_AUTH_ERROR_RE.test("Invalid API key · Fix external API key"));

      assert.ok(!CLAUDE_AUTH_ERROR_RE.test("ENOENT: file not found"));
      assert.ok(!CLAUDE_AUTH_ERROR_RE.test("SyntaxError: Unexpected token"));
      assert.ok(!CLAUDE_AUTH_ERROR_RE.test("Rate limit exceeded"));
    });

    it("surfaces clean guidance when Turns receives auth-failed event", async () => {
      let currentStatus: SessionStatus = "idle";
      let lastError: string | undefined;
      const notices: { level: string; text: string }[] = [];
      let stopped = false;

      let callbacks: EngineCallbacks;
      const fakeHandle: EngineHandle = {
        send: () => {},
        interrupt: () => {},
        stop: async () => { stopped = true; },
        get pid() { return 12345; },
      };

      const host: TurnHost = {
        spawn: async (_resume, cb) => {
          callbacks = cb;
          return fakeHandle;
        },
        event: () => {},
        status: () => currentStatus,
        setStatus: (status, error) => {
          currentStatus = status;
          lastError = error;
        },
        notice: (level, text) => {
          notices.push({ level, text });
        },
        learner: () => {},
        pending: () => 0,
        cancelPending: () => {},
        jobs: { list: () => [], markDelivered: () => {} },
      };

      const turns = new Turns(host);
      await turns.send("ciao", []);

      // Engine emits auth-failed
      callbacks!.onEvent({ type: "auth-failed" });

      assert.equal(currentStatus, "error");
      assert.equal(lastError, CLAUDE_AUTH_ERROR_MESSAGE);
      assert.ok(notices.some((n) => n.level === "error" && n.text === CLAUDE_AUTH_ERROR_MESSAGE));
      assert.ok(stopped, "turns should stop the failed process handle");

      // Later result should not reset status to idle
      callbacks!.onEvent({
        type: "result",
        isError: true,
        subtype: "api_error",
        sessionId: "s1",
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        costUsd: 0,
      });
      assert.equal(currentStatus, "error");
    });

    it("surfaces clean guidance when process exits with auth failure in stderr", async () => {
      let currentStatus: SessionStatus = "idle";
      let lastError: string | undefined;
      const notices: { level: string; text: string }[] = [];

      let callbacks: EngineCallbacks;
      const fakeHandle: EngineHandle = {
        send: () => {},
        interrupt: () => {},
        stop: async () => {},
        get pid() { return 12345; },
      };

      const host: TurnHost = {
        spawn: async (_resume, cb) => {
          callbacks = cb;
          return fakeHandle;
        },
        event: () => {},
        status: () => currentStatus,
        setStatus: (status, error) => {
          currentStatus = status;
          lastError = error;
        },
        notice: (level, text) => {
          notices.push({ level, text });
        },
        learner: () => {},
        pending: () => 0,
        cancelPending: () => {},
        jobs: { list: () => [], markDelivered: () => {} },
      };

      const turns = new Turns(host);
      await turns.send("ciao", []);

      callbacks!.onStderr("Not logged in · Please run /login");
      callbacks!.onExit(1, null);

      assert.equal(currentStatus, "error");
      assert.equal(lastError, CLAUDE_AUTH_ERROR_MESSAGE);
      assert.ok(notices.some((n) => n.level === "error" && n.text === CLAUDE_AUTH_ERROR_MESSAGE));
      assert.ok(!lastError!.includes("teacher process exited (1)"));
    });
  });
});
