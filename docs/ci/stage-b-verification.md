# 阶段 B：精确制品与迁移证据验收

日期：2026-10-25。工作区：`codex/backend-cicd-plan`，源码基线 `f4fab9ef579be2381265ed886bddc94aebaf1270`。

状态：本地实现和隔离验收完成，已获阶段 B 审查、提交、推送和创建 PR 授权，交付进行中。真实 main 制品 job 尚未运行，需合入后对应 main CI 证明；不以本地结果替代。未部署、未启用自动 CD，测试服仅接受只读查询。

## 实现范围

| 文件 | 能力 |
| --- | --- |
| `src/main.rs`、`src/deployment_migrations.rs` | 新增 `deploy-info`，仅输出二进制内嵌迁移的版本、方向、SHA384 checksum 与事务标记；不加载配置、不连接数据库、不执行迁移 |
| `ops/artifact_test_runtime.py` | 创建带本次 owner 的独立 Docker 内网、PostgreSQL/Redis、Ubuntu 应用容器与 Python HTTP 探针；不接受现有数据库/Redis DSN，不发布应用端口；按精确 ID 清理并复查 |
| `ops/release_artifact_smoke.py` | 验证 manifest、GitHub artifact 元数据、SHA/tree/run/attempt/digest，运行精确二进制，验证健康、鉴权、持久化、依赖故障和迁移失败，生成含清理状态的回执 |
| `ops/deployed_baseline.py` | 通过固定 `tshb-test` SSH 目标只读采集已接受 manifest、实际运行文件摘要与数据库账本；禁止覆盖旧回执，拒绝部署中/进程变化/摘要不符 |
| `ops/migration_compatibility.py` | 将实际部署基线与旧 CI 制品绑定，在合成库中演练整个 up/down 区间、旧程序回读；拒绝漂移、缺失、脏账本和不可支持的恢复区间 |
| `.github/workflows/ci.yml` | 保留完整原有 CI；main push/手动 main 增加 `Verify release artifact`，按构建任务输出的 artifact ID、生产 attempt 与 tree 重新下载并验收；消费任务 attempt 单独记录，支持仅重跑失败 smoke；无部署步骤、无测试服 secret |
| `ops/test_*.py` | 新增制品身份、清理、中断、迁移区间、拒绝回退、只读基线与工作流回归；不改业务 API 或数据库迁移 |

主线 CI 的六个集成分片、required check 名称、原生 hooks、部署 gate 均未删除或绕过。本轮新增 job 不是自动 CD：它只在 CI 独立环境运行和销毁测试资源，不写测试服。

## 验证结果

| 验证 | 实际结果 |
| --- | --- |
| 固定 Rust 工具链 | `cargo +1.98.0 fmt --all -- --check`、完整 `clippy --locked --all-targets --all-features -- -D warnings` 通过 |
| Rust lib/bin | 独立 PostgreSQL 18、Redis 6 下通过；lib 416 项、其他 bin 7 项，共 423 项 |
| Rust 文档测试 | 通过，当前 0 项 |
| Python 运维工具全量 | 139 项通过；最后的纯回执 owner 字段增加后，对制品/迁移 17 项定向回归再次通过 |
| CI YAML | 语法解析和 15 项结构/执行约束测试通过；没有发布到 GitHub，实际新增 job 未运行 |
| 真实旧制品 HTTP 链路 | 已部署 SHA 对应 CI 二进制在隔离环境启动，登录、refresh 轮换、词表创建、修改后新请求读取、logout 均成功 |
| 候选 Linux release | 固定 Rust 1.98.0 Linux 构建成功，默认 features；`deploy-info`、133 条内嵌迁移与实际新库账本一致 |
| 候选业务与故障 | 启动、health、ready、正常鉴权、词表写入重读通过；停止独立 Redis 后 ready 返回 503；篡改独立库迁移 checksum 后二进制启动失败 |
| 实际部署基线兼容 | `f4fab9ef579b` 旧二进制→本地候选→旧二进制，合成词表数据可读写；基线和候选最高迁移均为 `20261024090000`，无新增迁移，不执行 down |
| 历史非空区间补充 | 历史真实制品 `c1eb541d5146`，从 `20261023090000` 到 `20261024090000` 完整 5 条迁移；升级、撤回和旧程序回读通过，明确不作为当前已部署基线 |
| 真实数据守卫 | 独立库插入合成 `learning_memory_enrollments` 后，回退被现有守卫拒绝；数据/schema/序列/账本快照完全不变 |
| 清理 | 本轮应用、数据库、Redis、探针、构建容器和内网均已删除，标签独立复查无残留；不删除共享服务/缓存 |
| 测试服不变性 | 收尾只读复核 SHA 仍为 `f4fab9ef579b`、PID 仍为 `1125651`、无部署锁 |

本地候选二进制 SHA256 为 `b79f61941a913d9f4075ecdf8403fba22e88204f878342827c9a984b954057a2`。它由未提交源码构建，所有本地回执标记 `release_eligible=false`；没有伪造 main CI run/attempt 或发布资格。

旧制品来自实际 main CI `40086311339`、attempt 1、artifact `12557785282`；其 digest 与只读采集的测试服进程、文件及 manifest 一致。候选与旧版隔离演练使用相同业务夹具，不复制测试服业务库、账号或凭据。

## 环境和限制

- 实跑环境：Ubuntu 22.04 运行镜像、PostgreSQL 18.6、Redis 6.0.20；测试服基线为 PostgreSQL 18.3、Redis 6.0.16。已覆盖相同大版本/Redis 6.0 系列，未声称补丁版本完全相同。普通 CI 仍保留 PG16/Redis7；新增制品 job 补上 PG18/Redis6。
- macOS ARM 宿主运行 Linux amd64 应用容器；PG/Redis 为宿主架构容器。GitHub main job 使用 Linux x86_64 runner，真实运行结果仍需 PR/合入后的对应 CI 证明。
- 独立内网关闭外部访问与后台 worker；`COOKIE_SECURE=false` 只用于无外网/无宿主发布端口的合成 HTTP 环境，不修改测试服配置。这不替代正式 HTTPS/Secure Cookie 和 worker 验收。
- 回执中的 `rollback_schema_safe` 始终为 false。合成数据演练成功不能授权现网自动 down；PITR、恢复点、业务数据损失边界和发布前基线再核对属于后续发布门禁。
- 本地没有执行六个 Rust 集成分片全量重跑；它们在 workflow 中完整保留。已执行完整 lib/bin、doc、Clippy、运维工具测试和上述真实制品演练，不能把这些描述成 GitHub 全量 CI 已绿。
- 当前旧制品必须支持已核实的词表自定义项 wire 格式。更早、不具备该能力的基线会在实际 HTTP 验收时阻断，不把未知兼容性当作成功；本轮未新增旧版接口适配层。
- artifact 元数据应由受信执行者从对应 GitHub API 读取；本轮 CLI 校验其与 manifest 一致，但不把调用者提供的 JSON 视为独立签名。未来发布控制器负责重新查询最新来源与基线，不仅检查文件存在。
- 新库 smoke 是 main CI 的一部分；迁移兼容演练由受保护的独立执行入口消费基线与两个制品，本轮没有新增部署 workflow 或自动调用服务器写入。

## 使用入口

先准备受信的 GitHub artifact 元数据与对应下载文件。下载内容与工具目录分离；不从下载目录加载 Python 模块。工具自行使用已缓存镜像创建隔离依赖，镜像不存在则失败，不在工具内隐式拉取或升级。

```bash
python3 ops/deployed_baseline.py --output <new-baseline-json>
```

```bash
python3 ops/release_artifact_smoke.py --manifest <manifest> --artifact <binary> --artifact-id <id> --artifact-metadata <github-artifact-json> --expected-sha <full-sha> --expected-tree <tree> --run-id <id> --run-attempt <producer-attempt> --verification-attempt <current-attempt> --output <new-receipt>
```

```bash
python3 ops/migration_compatibility.py --baseline <captured-baseline> --baseline-manifest <old-manifest> --baseline-artifact <old-binary> --baseline-artifact-id <old-id> --baseline-artifact-metadata <old-github-artifact-json> --manifest <candidate-manifest> --artifact <candidate-binary> --artifact-id <id> --artifact-metadata <github-artifact-json> --expected-sha <full-sha> --expected-tree <tree> --run-id <id> --run-attempt <attempt> --output <new-receipt>
```

仅重跑失败 smoke 时，通过构建 job outputs 绑定原成功 artifact，不猜测最新制品；`--run-attempt` 始终指生产 attempt，`--verification-attempt` 指本次验收 attempt，回执分别保存。原制品丢失/过期/身份不匹配时阻断，不自动选择其他 attempt。

成功要求测试与清理都完成。输入身份错误在创建依赖前拒绝；运行中失败仍清理自有资源并保留失败回执。SIGINT/SIGTERM 会走失败清理；SIGKILL/宿主宕机不能保证进程执行 finally，后续须按 owner 标签核实并定向恢复清理，不自动删除其他 owner 的资源。

## 回执

- [实际部署只读基线](evidence/2026-10-25-stage-b-deployed-baseline.json)
- [本地候选制品 smoke 与故障注入](evidence/2026-10-25-stage-b-local-release-smoke.json)
- [实际部署基线的兼容演练](evidence/2026-10-25-stage-b-local-migration-rehearsal.json)
- [历史 5 条迁移区间演练](evidence/2026-10-25-stage-b-historical-interval.json)
- [真实数据守卫原子拒绝](evidence/2026-10-25-stage-b-real-guard-refusal.json)
- [最终运行器复验](evidence/2026-10-25-stage-b-final-runtime-recheck.json)
- [清理与测试服不变性](evidence/2026-10-25-stage-b-cleanup.json)
- [下载及 Linux 构建临时制品清理](evidence/2026-10-25-stage-b-local-artifact-cleanup.json)
- [最终检查汇总](evidence/2026-10-25-stage-b-final-verification.json)

交付状态：按授权执行精确提交的独立审查、正常 hooks、推送及 PR，实际结果记录于 PR 与会话。本文的本地验收证据不替代 GitHub CI；main 专用制品 job 在 PR 中跳过，未运行前不宣称该 job 已通过。合并、部署和启用自动 CD 不在本次授权范围。
