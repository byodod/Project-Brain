#!/usr/bin/env python3
"""Assemble one npm package from the four qualified native release archives."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import shutil
import stat
import tarfile
import zipfile


PACKAGE_NAME = "@byodod/project-brain"
DEVELOPMENT_VERSION = "0.0.0-development"
TARGETS = (
    ("aarch64-apple-darwin", "darwin", "arm64", "tar.gz", "project-brain"),
    ("x86_64-apple-darwin", "darwin", "x64", "tar.gz", "project-brain"),
    ("x86_64-pc-windows-msvc", "win32", "x64", "zip", "project-brain.exe"),
    ("x86_64-unknown-linux-gnu", "linux", "x64", "tar.gz", "project-brain"),
)


def workspace_version(manifest: pathlib.Path) -> str:
    contents = manifest.read_text(encoding="utf-8")
    workspace = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\Z)", contents)
    if workspace is None:
        raise ValueError("Cargo.toml 缺少 [workspace.package]")
    version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', workspace.group(1), re.MULTILINE)
    if version is None:
        raise ValueError("[workspace.package] 缺少字符串 version")
    return version.group(1)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def archive_binary(archive: pathlib.Path, member: str) -> bytes:
    if archive.name.endswith(".tar.gz"):
        with tarfile.open(archive, "r:gz") as bundle:
            entry = bundle.getmember(member)
            if not entry.isfile():
                raise ValueError(f"归档成员不是普通文件：{archive.name}:{member}")
            stream = bundle.extractfile(entry)
            if stream is None:
                raise ValueError(f"无法读取归档成员：{archive.name}:{member}")
            return stream.read()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            info = bundle.getinfo(member)
            if info.is_dir():
                raise ValueError(f"归档成员不是普通文件：{archive.name}:{member}")
            return bundle.read(info)
    raise ValueError(f"不支持的发布归档：{archive}")


def assemble(repo: pathlib.Path, dist: pathlib.Path, output: pathlib.Path, version: str) -> None:
    expected = workspace_version(repo / "Cargo.toml")
    if version != expected:
        raise ValueError(f"npm version {version} 与 workspace version {expected} 不一致")
    if output.exists():
        raise ValueError(f"npm 输出目录已存在：{output}")

    source = repo / "npm"
    package = json.loads((source / "package.json").read_text(encoding="utf-8"))
    if package.get("name") != PACKAGE_NAME:
        raise ValueError(f"npm package name 必须为 {PACKAGE_NAME}")
    if package.get("version") != DEVELOPMENT_VERSION or package.get("private") is not True:
        raise ValueError("npm 源模板必须保持 development version 且 private=true")

    shutil.copytree(source, output, ignore=shutil.ignore_patterns("test"))
    package["version"] = version
    package.pop("private")
    (output / "package.json").write_text(
        json.dumps(package, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    for license_name in ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE"):
        shutil.copyfile(repo / license_name, output / license_name)

    binaries = []
    for target, platform, architecture, extension, executable in TARGETS:
        archive = dist / f"project-brain-{version}-{target}.{extension}"
        if not archive.is_file():
            raise ValueError(f"缺少原生发布归档：{archive.name}")
        member = f"project-brain-{version}-{target}/{executable}"
        data = archive_binary(archive, member)
        destination = output / "vendor" / target / executable
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        if platform != "win32":
            destination.chmod(destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        binaries.append(
            {
                "target": target,
                "platform": platform,
                "architecture": architecture,
                "file": destination.relative_to(output).as_posix(),
                "sha256": sha256(data),
                "size": len(data),
            }
        )

    manifest = {"schema_version": 1, "package": PACKAGE_NAME, "version": version, "binaries": binaries}
    (output / "vendor" / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dist", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--version")
    args = parser.parse_args()
    repo = pathlib.Path(__file__).resolve().parent.parent
    version = args.version or workspace_version(repo / "Cargo.toml")
    assemble(repo, args.dist.resolve(), args.output.resolve(), version)
    print(f"npm_package={PACKAGE_NAME} version={version} output={args.output.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
