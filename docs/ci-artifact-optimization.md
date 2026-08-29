# 后端 CI 与部署 Artifact Phase 0/1 设计

## 1. 目标与边界

本轮只做不需要新增外部凭据的低风险改造：

- Phase 0：把 Cargo 依赖获取、缓存命中、Clippy、lib/bins、doc、各 integration
  模块、release build、artifact 大小以及部署下载/传输耗时变成日志和摘要中的显式数据；
- Phase 1：继续使用仓库已经采用并钉死版本的 `Swatinem/rust-cache`，加固缓存
  指纹、可信写入和失败回退；
- 为 main 的 release binary 生成可验证的构建 manifest，绑定源码、工具链、features、
  SQLx 离线元数据和二进制摘要；
- 修复部署预检把多个 `SELECT` 放进一次 `psql -c` 后误读空输出的问题；
- 保持所有格式、Clippy、lib/bins、doc、integration、迁移、安全、smoke 与
  required check 语义。

本轮明确不做：

- 不缓存或复用“测试已经通过”的结论，不缓存测试报告来跳过测试；
- 不减少测试模块、features、迁移或安全门，不把 cache hit 当 PASS；
- 不引入远程 `sccache` 服务、OSS、GitHub Environment secret、OIDC、
  `pull_request_target` 或 `workflow_run` 晋升；
- 不修改服务器、OSS 或任何环境，也不执行部署；
- 不把 release artifact 加入 PR required summary，避免改变现有 required check 拓扑。

## 2. 当前证据与 Phase 0 基线

Phase 0 的测量样本是 `ce52e6802d203fc8a3da7bb5a5781203de859b21`，对应 GitHub CI
run `33237420372`，于 2026-08-29 完成且结论为 success。它只用于改造前耗时对比；当前
权威 `origin/main` 必须在实施、提交和部署各关卡重新读取，不能由本文固定。

| 项目 | 当前证据 |
| --- | ---: |
| CI 总墙钟时间 | 4 分 43 秒 |
| Rust toolchain 安装 | 8–9 秒/Job |
| Cargo cache restore | 3–11 秒/Job；日志中没有统一命中摘要 |
| Clippy | 33 秒 |
| lib/bins | 39 秒 |
| doc tests | 2 秒 |
| integration: platform | 66 秒 |
| integration: admin | 136 秒 |
| integration: identity | 155 秒 |
| integration: lexicon | 245 秒 |
| release build | 142 秒 |
| 上传后的 GitHub artifact | 11,501,299 字节 |

当前 release job 已在 Ubuntu 22.04 容器中构建并上传 binary 与 SHA256，避免
Ubuntu 24.04 runner 产物在 glibc 2.35 服务器上无法运行。缺口是：没有显式 Cargo
fetch/命中指标，没有严格的构建输入 manifest，artifact 名没有绑定 SHA，部署下载后只验
binary SHA，没有同时验证源码 tree、工具链、features 和 SQLx 指纹。

近期真实部署还暴露了一个确定性缺陷：预检把 entries 聚合与新 migration 列表放在一次
`psql -Atqc` 中；该调用只留下最后一个查询结果。最后一条查询为空时，空输出被误解释为
`entries=0`，而独立查询证明当时有 5 条 V3 草稿。修复必须让每个标量查询成为独立 psql
调用，并输出机器可解析 JSON；空输出、非整数或命令失败一律 fail closed。

当前部署已经消费 CI artifact，因此服务器构建时间应明确记录为 `0 ms (ci-artifact)`；
部署性能数据由 GitHub artifact 下载和 rsync 二进制传输两段组成，不再把服务器编译时间
混入传输指标。

## 3. 缓存设计

### 3.1 缓存对象

继续缓存 Cargo registry/git 下载内容，但显式关闭 `target` 缓存。原因是本轮把“缓存损坏
必须完整重建”置于命中率之上：archive 成功解包后若 registry/git 内容仍不可用，显式 fetch
会改用全新的 `CARGO_HOME` 与 `CARGO_TARGET_DIR` 重试；第二次仍失败则保持失败。缓存
`target` 会让“有效 archive 内的坏编译产物”和真实编译/测试失败难以安全区分，自动重跑测试
又会掩盖 flaky，因此 Phase 1 不接受。缓存从不保存测试 PASS。本轮也不引入 `sccache`：
增加编译器 wrapper、安装与另一层缓存故障面不符合低风险边界。

### 3.2 Key 与隔离

每个 job 生成可审计的 cache key，至少绑定：

- runner OS 与 arch；
- `rustc -vV` 的 release、host、commit hash和根 `rust-toolchain.toml`；
- `Cargo.lock`；
- `Cargo.toml`、`.cargo/config.toml`、`build.rs`；
- features、profile、`SQLX_OFFLINE`；
- `.sqlx/**/*.json` 的排序路径与内容指纹。

Quality、unit/doc、四个 integration 模块和 release profile 保持独立 job key；即使本阶段
只保存 registry/git，profile/features 仍显式入 key，避免未来恢复编译缓存时无声扩大边界。
`rust-cache` 自带的 rust environment hash 继续启用，显式 key 是额外的 fail-safe。

### 3.3 信任与回退

- 只有 `push` 到权威仓库 `refs/heads/main` 的成功 job 可以保存 cache；
- PR、fork PR、手动非 main 运行只 restore，不写 cache；
- `cache-on-failure=false`、`cache-bin=false`，不保存失败态或 Cargo bin；
- restore 失败允许 job 继续；显式 fetch 首次失败时切到 runner 临时目录中的全新 Cargo
  home/target 重试，随后所有原始 Cargo 命令从干净目录完整重建；
- cache miss、损坏或服务不可用都不能跳过任何测试，也不能成为新的 required check。

缓存 key 只使用仓库文件摘要、固定构建参数和公开 runner/toolchain 元数据，不读取或写入
数据库 URL、token、cookie、SSH、OSS 或其他 secret。

## 4. CI 指标与 required checks

每个 Rust job 在 cache restore 后记录精确命中值，并用同一轻量工具包裹 Cargo 命令，
输出 `{name, elapsed_ms, exit_code}` 到 job 日志和 `$GITHUB_STEP_SUMMARY`。release job 额外
记录 binary 的字节数与 SHA256。

`Format, lint and test` 仍只依赖 `quality`、`unit-doc` 与完整 `integration` matrix；名字、
`always()` 和逐项 success 断言不变。release artifact 继续只影响 main/workflow_dispatch
的整体 CI 结论，不改变 PR required check 的语义。

## 5. Release artifact 契约

release job 只允许权威仓库的 `refs/heads/main` push 或 main 上的可信手动触发，artifact
名称包含目标平台、完整 Git SHA 和 run attempt。上传内容固定为：

- `tsz-rust`；
- `tsz-rust.sha256`；
- `tsz-rust.manifest.json`。

manifest 严格绑定：repository、ref、Git SHA/tree、CI run id/attempt/URL、rustc/cargo/
toolchain、target、profile、features、`SQLX_OFFLINE`、Cargo.lock/build config/SQLx 指纹、
artifact size 和 SHA256。上传前本 job 自验；部署下载后再用仓库内相同工具验证 manifest、
当前 exact-main SHA/tree、CI run 和 binary，不能只校验裸 SHA 文件。

GitHub Actions artifact 是本轮唯一落点，保留 30 天。未来若需要 OSS 或服务器直接拉取，
必须另开授权，选择短期 OIDC 或最小权限写入凭据、不可覆盖的 SHA 路径、保留/清理策略与
审计日志；本 PR 不创建 OSS bucket、secret、environment 或 deploy job。

## 6. 部署预检修复

新增只读预检工具，在服务器进程环境中从 `DATABASE_URL` 连接，并对 entries、V3 entries、
成功 migration 总数和最新 migration 逐项执行单独的 `psql -Atqc`。每个结果必须恰好是
一个整数；任何空输出、多行、非整数或 psql 非零退出都失败。工具只打印 JSON 和字段名，
绝不打印 DSN。

部署 skill 通过 SSH stdin 运行该工具，不把脚本或查询结果持久写入服务器；报告中包含
预检 JSON、artifact 下载/rsync 毫秒数，以及固定的 `server_build_ms=0`。本轮只修改并
测试这些仓库文件，不运行 SSH 或部署。

部署消费端在任何服务器写入前完成 SHA-qualified、attempt-qualified artifact 的下载和完整
manifest 验证；本地 state/staging 使用 session 隔离，并以 owner 文件互斥。服务器变更
窗口另有持久 owner lock，锁内重读正式 manifest 防竞态；成功 manifest 或完整回退验证前
均不释放。所有会在本地执行或发往服务器的部署辅助脚本，都先从已验证 `deploy_sha` 的
Git blob 提取到 session staging，不能消费长窗口后的当前工作树。已有锁只能只读报告并
停门，不能自动抢占。

## 7. 用例矩阵

| ID | 层级 | 场景 | 预期 | 优先级 |
| --- | --- | --- | --- | --- |
| CI-01 | unit | fingerprint 输入含 OS/arch/rustc/toolchain/lock/features/profile/SQLx/build config | 每项进入结构化输出与 cache key | P0 |
| CI-02 | unit | 修改 Cargo.lock、build config 或 `.sqlx` 任一内容 | 对应摘要和 cache key 改变 | P0 |
| CI-03 | unit | metrics 包裹成功/失败命令 | 保留真实退出码并输出耗时；失败不能被吞掉 | P0 |
| CI-04 | static | PR/fork PR cache | `save-if=false`，仍运行完整 Cargo 命令 | P0 |
| CI-05 | static | trusted main cache | 仅权威 repo main push 成功后可写，失败不写 | P0 |
| CI-06 | unit/static | cache restore miss/损坏 | registry/git 首次 fetch 失败切全新 Cargo home/target；第二次失败阻断；全部门仍运行 | P0 |
| CI-07 | static | required summary | 仍依赖 quality/unit-doc/integration，并逐项要求 success | P0 |
| CI-08 | static | release 触发 | 仅 exact main/可信 main 手动触发，不在 PR 构建 | P0 |
| ART-01 | unit | 创建并验证 manifest | SHA/tree/toolchain/features/SQLx/size/SHA256 全部一致 | P0 |
| ART-02 | unit | binary 被篡改 | verify fail closed | P0 |
| ART-03 | unit | manifest 多字段、缺字段、错 ref/SHA/run | 严格拒绝 | P0 |
| ART-04 | static | upload 内容与名称 | SHA-qualified 且恰好上传 binary/SHA/manifest | P0 |
| DB-01 | unit | 模拟 psql 对多语句只回最后结果 | 工具仍逐查询得到正确 entries，不会误判 0 | P0 |
| DB-02 | unit | 某个标量为空、非整数、多行或命令失败 | 整体非零退出，不输出伪造快照 | P0 |
| SEC-01 | static | workflow/manifest/preflight | 无 secret 进入 cache key、artifact、日志或命令摘要 | P0 |
| SEC-02 | static | 长部署窗口中的辅助脚本 | 只从 `deploy_sha` Git blob 提取并执行/传输，不消费当前工作树文件 | P0 |
| REG-01 | local | workflow 工具单测 | 全绿 | P0 |
| REG-02 | local | fmt/check/clippy/all-features test | 全绿，不减少测试范围 | P0 |
| DEP-01 | design only | OSS/OIDC/Environment/server 变更 | 保持停门，不在本任务执行 | P0 |

## 8. 回滚与剩余风险

workflow、辅助脚本和 skill 可按单一 commit 回滚；不会产生 schema、业务数据或服务器状态
变化。缓存 key 使用新 schema 前缀，回滚后旧 key 仍由 GitHub 自然淘汰，不需要删除缓存。

本地静态测试可以证明拓扑、触发条件、严格解析和 manifest 契约，不能证明 GitHub 首次
真实 cache miss/hit 的耗时收益；该数据只能在 PR/main run 中观察。OSS、OIDC、服务器
直接拉取和真实部署传输耗时仍是后续独立授权项。
