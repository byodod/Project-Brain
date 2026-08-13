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
   cargo build --release --locked -p project-brain
   ```

4. 创建并推送与 workspace 版本完全一致的标签，例如 `v0.1.0`。标签不匹配时发布流程会立即失败。

## 自动产物

发布矩阵生成以下原生归档：

- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

每个归档包含 CLI、README、变更记录和双许可证。发布任务还生成覆盖所有归档的 `SHA256SUMS`，然后通过 GitHub CLI 创建对应 tag 的 Release。

## 安装与回滚验证

下载匹配平台的归档并校验 SHA-256 后，执行：

```text
project-brain capabilities codex
project-brain install
project-brain bootstrap --codex
project-brain doctor
```

升级验证至少应覆盖新版本并排安装、Hook 路径不变和 `project-brain rollback` 恢复上一 payload。不要在未经过实际目标机器验证前删除旧版本目录。
