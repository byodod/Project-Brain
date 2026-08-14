import { lstatSync } from "node:fs";
import { resolve, sep } from "node:path";

const TARGETS = Object.freeze({
  "darwin-arm64": Object.freeze({
    target: "aarch64-apple-darwin",
    executable: "project-brain",
  }),
  "darwin-x64": Object.freeze({
    target: "x86_64-apple-darwin",
    executable: "project-brain",
  }),
  "linux-x64": Object.freeze({
    target: "x86_64-unknown-linux-gnu",
    executable: "project-brain",
  }),
  "win32-x64": Object.freeze({
    target: "x86_64-pc-windows-msvc",
    executable: "project-brain.exe",
  }),
});

export function detectLinuxLibc(report = process.report) {
  try {
    const runtime = report?.getReport?.().header?.glibcVersionRuntime;
    return typeof runtime === "string" && runtime.length > 0 ? "glibc" : "unknown";
  } catch {
    return "unknown";
  }
}

export function selectTarget({
  platform = process.platform,
  arch = process.arch,
  libc = platform === "linux" ? detectLinuxLibc() : undefined,
} = {}) {
  if (platform === "linux" && libc !== "glibc") {
    throw new Error(
      `unsupported Linux libc ${JSON.stringify(libc)}; the npm package currently provides glibc x64 only`,
    );
  }

  const selected = TARGETS[`${platform}-${arch}`];
  if (selected === undefined) {
    throw new Error(
      `unsupported platform ${platform}/${arch}; supported targets are Windows x64, Linux glibc x64, macOS x64 and macOS arm64`,
    );
  }
  return selected;
}

export function nativeBinaryPath(packageRoot, options) {
  const selected = selectTarget(options);
  const vendorRoot = resolve(packageRoot, "vendor");
  const binary = resolve(vendorRoot, selected.target, selected.executable);
  if (!binary.startsWith(`${vendorRoot}${sep}`)) {
    throw new Error("internal platform mapping escaped the npm vendor directory");
  }

  let metadata;
  try {
    metadata = lstatSync(binary);
  } catch {
    throw new Error(
      `native binary is missing for ${selected.target}; reinstall @byodod/project-brain without omitting package files`,
    );
  }
  if (!metadata.isFile()) {
    throw new Error(`native binary path is not a regular file: ${binary}`);
  }
  return binary;
}
