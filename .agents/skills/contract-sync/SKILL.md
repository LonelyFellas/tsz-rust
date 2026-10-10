---
name: contract-sync
description: 对齐 tsz 与 tsz-rust 的 OpenAPI、wire 类型、端点快照和严格运行时契约，验证接口变化及混合版本兼容。用于跨仓接口新增、变更、契约漂移或配套发布准备；不用于无 API 变化的页面调整。
---

# TSZ 前后端契约同步

通用授权与任务记录遵循 [AGENTS.md](../../../AGENTS.md)。契约核对、生成与兼容证据记入主任务，各阶段复用；不为两仓分别重做设计、确认或最终门。

目标是让后端实现、生成契约和实际前端消费者一致。只要求核对时保持只读；要求同步/修复时完成本地改动与验证，不自动提交或部署。

## 1. 固定本次输入

- 从用户目标、当前 git 根目录、remotes 和项目文件识别 tsz / tsz-rust 的实际 checkout。相邻目录只是候选；worktree 场景不沿用默认主 checkout。
- 记录两仓绝对路径、分支、HEAD、相关未提交差异和端点范围。未提交源码生成的结果标为「SHA + 工作区差异」，不能归属于纯提交。
- 先读取相关 handler/DTO、`docs/openapi.json`、前端请求方法、`packages/types` 和直接消费者。`docs/frontend-integration.md` 帮助定位背景，其中历史部署 SHA/端口不代表当前环境。
- 有现成完整证据就复用；仅核对时比较现有产物及源码，不运行写文件的导出/同步。生成目标已有用户改动时先保留原内容并检查归属，不能用生成器覆盖未理解的改动。

## 2. 按仓库生成链同步

在选定的后端根目录运行：

```bash
SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi
```

该二进制生成 `docs/openapi.json`，不启动服务。SQLx 离线编译依赖当前 `.sqlx`；缓存缺失时定位 SQL/schema 变化，按已确认隔离数据库准备缓存，不能随手连接共享库或改用裸 `cargo run`。
build/bugfix 已完成同输入导出且实现、配置、依赖未变时复用该产物与证据，不再重复导出；同步也只在输入或生成物需要变化时执行。

在选定的前端根目录运行；`tsz_backend_root` 必须是上一步核实的绝对路径：

```bash
env -u SYNC_OPENAPI_RUNTIME_ONLY OPENAPI_SOURCE="$tsz_backend_root/docs/openapi.json" pnpm --filter @tsz/api-client sync:openapi
```

- 显式指定 `OPENAPI_SOURCE`，避免 worktree 默读另一个后端。普通同步清除继承的 `SYNC_OPENAPI_RUNTIME_ONLY`，防止只更新 runtime bundle、遗漏 endpoint 快照。
- 审查后端 spec、前端 `src/openapi.snapshot.json` 与 `src/admin-word-v3.runtime-schema.json` 的实际 diff（后两者在 `packages/api-client`）。复用原生生成器，不手工修生成字段。
- 核实生成物的 `_source` 指向所选输入，runtime bundle 的 `_source_sha256` 匹配原始 spec 内容；路径元数据变化与契约变化分开判断。
- 按变化更新 `@tsz/types` 的 snake_case wire 镜像、`@tsz/api-client` 请求与相关消费者；不增加命名转换层。实现与文档不一致时查清正确行为，不让生成快照为错误实现背书。

## 3. 证明契约与兼容性

选择本次命中的 method/path、状态码、Problem Details、必填/可选/nullable、枚举及权限分支。
重点看 `endpoints.contract.test.ts`、`runtime-schema.test.ts` 和相关业务契约测试；按现有测试位置补必要断言。
`PENDING` 仅用于后端确实未实现且有原因的端点；已实现接口不准靠扩大白名单通过。

V3 runtime validator 会拒绝未声明字段。新增响应字段也可能让旧前端报错；同时检查新前端访问旧 API 的缺字段/新枚举行为。
兼容性必须基于实际 schema 与消费者，不能默认「新增字段安全」或「一律前端先发」。

迭代运行受影响契约文件及 Rust handler/序列化测试；完成同步须有当前输入的 `pnpm --filter @tsz/api-client test` 和受影响类型检查通过证据，可复用同状态已完成结果，紧接 ship 时由原生 hooks 承接其覆盖项。
数据库集成测试先核实依赖隔离。最终全量质量门复用项目 test/ship 的安排，不在这里再叠一轮。
需要证明浏览器连到真实后端时，读取选定前端 checkout 的 `.agents/skills/test/references/real-backend-acceptance.md`。

## 4. 交接与配套发布

报告端点变化、两仓版本/脏状态、spec 来源与哈希、生成物、消费者、验证结果和剩余兼容风险。
涉及两个仓库的发布准备或执行时，读取 [配套发布清单](references/paired-release.md)，把经过验证的发布顺序交给各仓 ship/deploy。
配套清单只协调依赖，不替代原生发布门禁，也不扩大原请求的提交、合并或部署范围。
