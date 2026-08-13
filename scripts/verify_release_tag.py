#!/usr/bin/env python3
"""Fail when a release tag does not exactly match the Cargo workspace version."""

from __future__ import annotations

import pathlib
import re
import sys


def workspace_version(manifest: pathlib.Path) -> str:
    contents = manifest.read_text(encoding="utf-8")
    workspace = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\Z)", contents)
    if workspace is None:
        raise ValueError("Cargo.toml 缺少 [workspace.package]")
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', workspace.group(1))
    if version is None:
        raise ValueError("[workspace.package] 缺少字符串 version")
    return version.group(1)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: verify_release_tag.py vMAJOR.MINOR.PATCH", file=sys.stderr)
        return 2
    tag = sys.argv[1]
    if re.fullmatch(r"v\d+\.\d+\.\d+", tag) is None:
        print(f"非法发布标签：{tag}", file=sys.stderr)
        return 2
    version = workspace_version(pathlib.Path("Cargo.toml"))
    if tag != f"v{version}":
        print(f"标签 {tag} 与 workspace version {version} 不一致", file=sys.stderr)
        return 1
    print(f"release_version={version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
