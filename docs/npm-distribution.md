# npm 分发

官方 npm 包名为 `@byodod/project-brain`。npm 只负责选择并启动已通过 GitHub Release 资格验证的 Rust
原生二进制，不重新实现 Project Brain runtime。

## 包结构

一个 npm tarball 同时包含：

- `x86_64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Node launcher 根据 `process.platform`、`process.arch` 和 Linux libc 选择精确二进制。未知架构与 musl
Linux 明确拒绝，不回退到不兼容目标。

包不声明第三方运行时依赖，也不使用 `preinstall`、`install` 或 `postinstall` 脚本。所有原生文件在
GitHub Release 中先完成能力自检和 Q1-Q7 Production Qualification，再由
`scripts/assemble_npm_package.py` 从四份不可变归档组装。

## 用户安装

```text
npm install --global @byodod/project-brain
project-brain --version
```

或：

```text
npx @byodod/project-brain --version
```

## 首次发布

npm registry 上的首个版本需要由 `byodod` 账号完成身份引导：

1. 等待对应 GitHub Release 成功，并下载 `byodod-project-brain-X.Y.Z.tgz`；
2. 本地执行 `npm login`；
3. 执行 `npm publish byodod-project-brain-X.Y.Z.tgz --access public` 并完成 2FA；
4. 打开 npm 包设置中的 Trusted Publisher，选择 GitHub Actions；
5. 填写 user `byodod`、repository `Project-Brain`、workflow filename `release.yml`、environment 留空，
   allowed action 选择 `npm publish`；
6. 在 GitHub 仓库 Actions variables 中创建
   `PROJECT_BRAIN_NPM_TRUSTED_PUBLISHING=true`。

## 后续自动发布

仓库变量启用后，`release.yml` 的独立 `publish-npm` job 在 GitHub Release 成功后运行。该 job 使用
GitHub-hosted runner、`id-token: write` 和 npm OIDC trusted publishing，不读取长期发布 Token。

若 npm 发布失败，GitHub Release 与其中的 npm tarball 仍保持可审计；修复 Trusted Publisher 配置后，
应在确认 registry 尚无同版本时重新运行失败 job。npm 版本不可覆盖。

Trusted Publisher 的平台要求与配置字段以
[npm 官方文档](https://docs.npmjs.com/trusted-publishers/)为准。
