import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { DEFAULT_SETTINGS } from "../shared/protocol.ts";
import { maskApiKey, readSettings, writeSettings } from "../server/settings.ts";

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
});
