#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { nativeBinaryPath } from "../lib/platform.js";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));

try {
  const binary = nativeBinaryPath(packageRoot);
  const result = spawnSync(binary, process.argv.slice(2), {
    stdio: "inherit",
    windowsHide: true,
  });

  if (result.error) {
    throw result.error;
  }
  if (result.status !== null) {
    process.exitCode = result.status;
  } else {
    const signal = result.signal ?? "unknown signal";
    console.error(`project-brain: native process terminated by ${signal}`);
    process.exitCode = 1;
  }
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`project-brain: ${message}`);
  process.exitCode = 1;
}
