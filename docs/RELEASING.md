# 发布流程

Project Brain 的发布产物由 GitHub Actions 从版本标签构建，不接受手工上传的替代二进制。

## 发布前

1. 更新 workspace 版本与 `Cargo.lock`。
2. 将 `CHANGELOG.md` 的相关内容从 `Unreleased` 移入带日期的版本段。
3. 在干净工作区运行：

   ```text
   cargo fmt --all -- --check
   cargo test --workspace --all-targets --locked
   cargo clippy --workspace --all-targets --locked -- -D warnings
   node --test npm/test/*.test.js
   python -B -m unittest discover -s scripts/tests -p "test_*.py" -v
   ```

4. 创建并推送与 workspace 版本完全一致的标签，例如 `v0.3.0`。标签不匹配时发布流程会立即失败。

## 自动产物

发布矩阵生成以下原生归档：

- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

每个原生归档包含 CLI、README、变更记录、顶层双许可说明和两份完整许可证正文。四份原生归档随后组装为
`byodod-project-brain-X.Y.Z.tgz`；组装任务会通过实际 npm 安装运行 CLI 与四 Adapter 能力自检。

发布任务生成覆盖四份原生归档和 npm tarball 的 `SHA256SUMS`，然后创建对应 tag 的 GitHub Release。

## npm 发布

首次发布需要 `byodod` npm 账号手动发布 GitHub Release 中的 `.tgz`，再按
[npm 分发](npm-distribution.md) 配置 Trusted Publisher。完成后将仓库 Actions variable
`PROJECT_BRAIN_NPM_TRUSTED_PUBLISHING` 设为 `true`，后续 Release 会使用 OIDC 自动发布，不需要
`NPM_TOKEN`。

## 安装与回滚验证

GitHub 原生归档路径：下载匹配平台的归档并校验 SHA-256。

npm 路径：

```text
npm install --global @byodod/project-brain
project-brain --version
```

两条路径都至少执行：

```text
project-brain capabilities codex
project-brain install
project-brain bootstrap
project-brain install-hooks codex
project-brain doctor codex
```

升级验证至少覆盖新版本并排安装、Hook 路径不变和 `project-brain rollback` 恢复上一 payload。不要在
未经过实际目标机器验证前删除旧版本目录。
