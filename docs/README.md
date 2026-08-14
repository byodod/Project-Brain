# Project Brain 文档

本目录保存 Project Brain 的使用手册、架构与协议。项目首页只保留定位、能力边界和最短使用路径；
具体操作与设计细节以这里的文档为准。

## 推荐阅读顺序

1. [快速开始与 Agent 接入](getting-started.md)：安装二进制、初始化仓库并接入 Coding Agent；
2. [Provider 与 Evidence](providers.md)：为语言、工具链或框架接入可验证证据；
3. [运维、资格与发布](operations.md)：维护数据库、运行生产资格验证和发布版本；
4. [架构说明](architecture.md)：理解核心分层、数据流和安全边界；
5. [协议说明](protocol.md)：实现 Adapter 或 Provider 时使用的精确合同。

## 文档分区

### 使用与运维

- [快速开始与 Agent 接入](getting-started.md)
- [Provider 与 Evidence](providers.md)
- [运维、资格与发布](operations.md)
- [发布维护清单](RELEASING.md)

### 设计

- [架构说明](architecture.md)
- [协议说明](protocol.md)
- [架构决策记录](adr/)

ADR 记录某项设计为何成立及其验证依据；当前行为以代码、协议说明和最新 Accepted ADR 共同为准。
