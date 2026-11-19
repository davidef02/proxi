import { describe, it } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

function readRepoFile(relPath: string): string {
  return fs.readFileSync(path.join(ROOT, relPath), "utf8");
}

describe("CI workflow matrix (AC1)", () => {
  it("defines matrix with windows-latest, ubuntu-latest, and macos-latest", () => {
    const ciContent = readRepoFile(".github/workflows/ci.yml");
    assert.ok(ciContent.includes("windows-latest"), "missing windows-latest in ci.yml");
    assert.ok(ciContent.includes("ubuntu-latest"), "missing ubuntu-latest in ci.yml");
    assert.ok(ciContent.includes("macos-latest"), "missing macos-latest in ci.yml");
    assert.ok(
      /matrix:\s*\n\s*os:\s*\[windows-latest,\s*ubuntu-latest,\s*macos-latest\]/.test(ciContent) ||
        (ciContent.includes("matrix:") &&
          ciContent.includes("windows-latest") &&
          ciContent.includes("ubuntu-latest") &&
          ciContent.includes("macos-latest")),
      "expected matrix over the three OS runners",
    );
  });

  it("installs Linux Tauri dependencies without libsecret", () => {
    const ciContent = readRepoFile(".github/workflows/ci.yml");
    const requiredLinuxDeps = [
      "libwebkit2gtk-4.1-dev",
      "build-essential",
      "curl",
      "wget",
      "file",
      "libxdo-dev",
      "libssl-dev",
      "libayatana-appindicator3-dev",
      "librsvg2-dev",
    ];
    for (const dep of requiredLinuxDeps) {
      assert.ok(ciContent.includes(dep), `missing Linux dependency in ci.yml: ${dep}`);
    }
    assert.ok(!ciContent.includes("libsecret-1-dev"), "libsecret should not be in ci.yml (pure-Rust zbus keyring)");
  });

  it("runs the check gate and uploads traces per OS leg", () => {
    const ciContent = readRepoFile(".github/workflows/ci.yml");
    assert.ok(ciContent.includes("bash scripts/gate.sh"), "missing bash scripts/gate.sh step");
    assert.ok(
      ciContent.includes("playwright-traces-${{ matrix.os }}"),
      "artifact upload must be scoped with ${{ matrix.os }}",
    );
  });
});

describe("README build sections (AC3)", () => {
  function getBuildSection(): string {
    const readme = readRepoFile("README.md");
    const lines = readme.split("\n");
    const start = lines.findIndex((line) => line.trimEnd() === "## Build");
    assert.notEqual(start, -1, "missing ## Build section in README.md");
    const end = lines.findIndex((line, i) => i > start && line.startsWith("## "));
    return lines.slice(start, end === -1 ? undefined : end).join("\n");
  }

  it("contains sub-sections for Windows, macOS, and Linux", () => {
    const section = getBuildSection();
    assert.ok(section.includes("### Windows"), "missing ### Windows subsection under ## Build");
    assert.ok(section.includes("### macOS"), "missing ### macOS subsection under ## Build");
    assert.ok(section.includes("### Linux"), "missing ### Linux subsection under ## Build");
  });

  it("includes per-OS prerequisites and key packages", () => {
    const section = getBuildSection();
    assert.ok(section.includes("WebView2"), "missing WebView2 prerequisite under Windows");
    assert.ok(
      section.includes("Xcode") || section.includes("xcode-select"),
      "missing Xcode CLT prerequisite under macOS",
    );
    const requiredLinuxDeps = [
      "libwebkit2gtk-4.1-dev",
      "build-essential",
      "curl",
      "wget",
      "file",
      "libxdo-dev",
      "libssl-dev",
      "libayatana-appindicator3-dev",
      "librsvg2-dev",
    ];
    for (const dep of requiredLinuxDeps) {
      assert.ok(section.includes(dep), `missing Linux package in README: ${dep}`);
    }
  });
});

describe("development doc CI contract (AC4)", () => {
  it("states CI runs on every PR across Windows, macOS, and Linux", () => {
    const devDoc = readRepoFile("docs/agents/development.md");
    const lines = devDoc.split("\n");
    const start = lines.findIndex((line) => line.trimEnd() === "## Commands");
    assert.notEqual(start, -1, "missing ## Commands section in docs/agents/development.md");
    const commandsPreamble = lines.slice(start, start + 6).join("\n");
    assert.ok(
      commandsPreamble.includes("Windows") &&
        commandsPreamble.includes("macOS") &&
        commandsPreamble.includes("Linux"),
      "CI contract statement under ## Commands must name Windows, macOS, and Linux",
    );
    assert.ok(
      commandsPreamble.includes("CI runs all of these on every PR"),
      "must preserve 'CI runs all of these on every PR'",
    );
  });
});
