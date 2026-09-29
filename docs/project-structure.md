# 后端项目结构

本文描述当前 Rust 仓库的组织方式，不混放未来目录草案。

## 入口与公共设施

| 位置 | 职责 |
| --- | --- |
| [src/main.rs](../src/main.rs) | 进程入口、连接依赖、部署迁移子命令分派 |
| [src/lib.rs](../src/lib.rs) | 模块出口、应用装配、启动迁移与监听 |
| [src/config.rs](../src/config.rs) | 环境配置与校验 |
| [src/error.rs](../src/error.rs) | 业务错误与 HTTP Problem Details |
| [src/openapi.rs](../src/openapi.rs) | OpenAPI 装配 |
| [src/api/](../src/api/) | 公共提取器与分页等 API 支持 |
| [src/platform/](../src/platform/) | 数据库、Redis、对象存储等基础设施 |
| [src/bin/](../src/bin/) | 显式运行的一次性工具，不等同于服务启动 |

## 业务模块

| 位置 | 职责 |
| --- | --- |
| [src/auth/](../src/auth/)、[src/user/](../src/user/) | 用户认证与用户业务 |
| [src/session/](../src/session/)、[src/otp/](../src/otp/) | 刷新会话与验证码 |
| [src/admin/](../src/admin/) | 管理端身份、账号、资料与权限 |
| [src/catalog/](../src/catalog/) | 词性、词形类型等目录配置 |
| [src/lexicon/](../src/lexicon/) | 词条、发布、引用、共享例句与音频资产 |
| [src/speech/](../src/speech/) | 语音目录、发音与合成相关能力 |

模块按实际大小采用单文件或子目录，不要求所有业务都生成同样的骨架。现有仓储使用具体实现；不能根据历史教程要求补出 trait/fake 双层仓储。

## 配套目录

- [migrations/](../migrations/)：数据库结构演进；部署前核实实际迁移区间与恢复方案。
- [tests/](../tests/)：跨模块、HTTP、数据库等测试；具体方法见[测试指南](testing-guide.md)。
- [.sqlx/](../.sqlx/)：SQLx 离线查询缓存，不能替代测试数据库。
- [ops/](../ops/)：部署、制品验证、导入与运维工具。
- [.github/workflows/](../.github/workflows/)：实际 CI 定义。
- [docs/README.md](README.md)：现行文档入口；`features/` 仅保留未关闭的任务，已完成计划通过 Git 历史追溯。

开发与交付门禁只维护在 [AGENTS.md](../AGENTS.md) 及其项目技能中，不在本页复制。
