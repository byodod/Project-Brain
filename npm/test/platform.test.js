import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { detectLinuxLibc, nativeBinaryPath, selectTarget } from "../lib/platform.js";

test("selectTarget maps every published platform", () => {
  assert.equal(selectTarget({ platform: "win32", arch: "x64" }).target, "x86_64-pc-windows-msvc");
  assert.equal(
    selectTarget({ platform: "linux", arch: "x64", libc: "glibc" }).target,
    "x86_64-unknown-linux-gnu",
  );
  assert.equal(selectTarget({ platform: "darwin", arch: "x64" }).target, "x86_64-apple-darwin");
  assert.equal(selectTarget({ platform: "darwin", arch: "arm64" }).target, "aarch64-apple-darwin");
});

test("selectTarget rejects unsupported architecture and musl", () => {
  assert.throws(() => selectTarget({ platform: "win32", arch: "arm64" }), /unsupported platform/);
  assert.throws(
    () => selectTarget({ platform: "linux", arch: "x64", libc: "unknown" }),
    /glibc x64 only/,
  );
});

test("detectLinuxLibc fails closed when the runtime report is unavailable", () => {
  assert.equal(detectLinuxLibc(null), "unknown");
  assert.equal(
    detectLinuxLibc({ getReport: () => ({ header: { glibcVersionRuntime: "2.39" } }) }),
    "glibc",
  );
});

test("nativeBinaryPath requires the selected target to be a regular file", () => {
  const root = mkdtempSync(join(tmpdir(), "project-brain-npm-"));
  try {
    assert.throws(
      () => nativeBinaryPath(root, { platform: "win32", arch: "x64" }),
      /native binary is missing/,
    );
    const directory = join(root, "vendor", "x86_64-pc-windows-msvc");
    mkdirSync(directory, { recursive: true });
    const binary = join(directory, "project-brain.exe");
    writeFileSync(binary, "fixture");
    assert.equal(nativeBinaryPath(root, { platform: "win32", arch: "x64" }), binary);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
