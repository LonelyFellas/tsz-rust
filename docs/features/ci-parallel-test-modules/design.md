# GitHub CI 模块化并行测试技术设计

## 方案概述

把当前 `ci` 串行 Job 拆成三个层次：无服务依赖的 `quality`、带服务的 `unit-doc`、以及
`integration` 模块矩阵。矩阵按 Lexicon、Admin、Identity、Platform 四个领域并行运行，最后由
一个稳定的 `ci-summary` Job 汇总所有必需结果。

模块选择由仓库内的轻量 Python 标准库脚本负责。脚本扫描 `tests/*.rs`，按冻结规则分配模块，
校验每个测试 target 恰好出现一次，再用一次 Cargo 调用执行该模块的所有 `--test` target。相比在
YAML 中手写长命令，这能让遗漏、重复和空模块成为可测试的明确失败。

不采用 `cargo-nextest` 或 hash shard：前者会新增工具安装和版本维护，后者虽能均衡耗时，却不能
让失败结果直接对应业务模块。当前首要诉求是按模块拆分，因此使用 Cargo 原生命令即可。

## 现状与耗时证据

当前 `.github/workflows/ci.yml` 的 PR 路径只有一个 `Format, lint and test` Job：

1. 初始化 PostgreSQL/Redis 与 Runner；
2. checkout、Rust toolchain、Cargo cache；
3. `cargo fmt --all -- --check`；
4. `python3 -m unittest ops/test_deployment_manifest.py`；
5. `cargo clippy --locked --all-targets --all-features -- -D warnings`；
6. `cargo test --locked --all-features`。

PR #64 / run `32796171801` 的实际步骤耗时：

| 部分 | 耗时 |
| --- | ---: |
| Job + service 初始化 | 约 22 秒 |
| checkout + toolchain + cache | 约 22 秒 |
| format + Python test | 约 1 秒 |
| Clippy | 约 31 秒 |
| 全量 Rust tests | 约 663 秒 |
| 完整 Job | 约 744 秒 |

集成测试二进制目前由 Cargo 逐个执行。按目标模块累计的测试运行时间为：

| 模块 | 当前测试 target 数 | 累计执行时间 |
| --- | ---: | ---: |
| Lexicon | 9 | 约 208 秒 |
| Admin | 19 | 约 179 秒 |
| Identity | 22 | 约 163 秒 |
| Platform | 其余 | 约 40 秒 |

并行后的理论测试关键路径由 Lexicon 的约 208 秒决定，而不是四组之和。考虑 Runner 初始化、缓存
恢复和编译，warm-cache 目标为 4–6 分钟；冷缓存或 GitHub 排队时可能更长。

## 代码影响范围

- 修改 `.github/workflows/ci.yml`
  - 拆分 `quality`、`unit-doc`、`integration`、`ci-summary`；
  - 为 integration 增加四模块 matrix 和 `fail-fast: false`；
  - 只给需要的 Job 配置 PostgreSQL/Redis services；
  - 保持 release artifact Job 的构建与上传步骤不变。
- 新增 `ops/ci_test_modules.py`
  - 发现、分类、校验并执行集成测试 targets；
  - 仅使用 Python 标准库；
  - 支持只列出映射，便于日志与单测。
- 新增 `ops/test_ci_test_modules.py`
  - 锁定全部现有测试文件的模块归属；
  - 覆盖未知文件、重复规则、空模块和 Cargo 参数生成。
- 新增本评估目录：
  - `docs/features/ci-parallel-test-modules/requirements.md`
  - `docs/features/ci-parallel-test-modules/design.md`

不修改 Rust 源码、Cargo 依赖、数据库迁移、OpenAPI、部署脚本或 release artifact 内容。

## 模块分类规则

分类使用测试 target 名（文件名去掉 `.rs`），按以下规则匹配：

| 模块 | 规则 |
| --- | --- |
| Admin | `admin_*` |
| Lexicon | `lexicon_*`、`content_completion_handler`、`dictionary_schema` |
| Identity | `account_deletion_handler`、`auth_*`、`otp_*`、`refresh_tokens_schema`、`register_session_transaction`、`session_*`、`student_profiles_schema`、`teacher_profiles_schema`、`user_*`、`users_schema` |
| Platform | 明确列入的 Catalog、Health、Object Storage、Redis readiness、Speech targets |

规则不是“未匹配就默认 Platform”。任何未知 target 都失败，迫使新增测试时显式决定模块所有权。
脚本在执行前校验：

1. 每个发现的 target 恰好匹配一个模块；
2. 每个模块至少一个 target；
3. 请求执行的模块名合法；
4. 传给 Cargo 的 target 名只来自仓库扫描结果。

## Job 设计

### `quality`

- 不启动 PostgreSQL/Redis；
- checkout、解析 pinned Rust、安装 rustfmt/clippy、恢复 Cargo cache；
- 运行格式检查；
- 运行 deployment manifest 与 CI 模块分类器的 Python 单测；
- 运行全 target、全 feature Clippy。

### `unit-doc`

- 启动 PostgreSQL 16 和 Redis 7；C2 后库内已有 `#[sqlx::test]`，不能假设 unit 不需要数据库；
- 运行 `cargo test --locked --all-features --lib --bins`；
- 单独运行 `cargo test --locked --all-features --doc`，避免拆分后丢失 doc tests。

### `integration`

- `strategy.fail-fast: false`；
- matrix：`lexicon`、`admin`、`identity`、`platform`；
- 每个 matrix Runner 拥有独立 PostgreSQL/Redis service container；
- 运行 `python3 ops/ci_test_modules.py run <module>`；
- 脚本先打印 target 清单，再执行：

  ```text
  cargo test --locked --all-features \
    --test <target-1> --test <target-2> ...
  ```

同一 Cargo 进程负责该模块的所有 target，避免为每个文件重新启动 Cargo。

### `ci-summary`

- `needs: [quality, unit-doc, integration]`；
- `if: always()`，即使前置 Job 失败也执行；
- 任一前置结果不是 `success` 时失败；
- 对外显示名继续使用稳定的 `Format, lint and test`，兼容 PR 阅读习惯和未来 branch protection。

### `release-artifact`

保持现状：只在 `main` push 或 workflow_dispatch 执行，继续使用 Ubuntu 22.04 容器构建与服务器
glibc 兼容的默认-feature release 二进制。

## 缓存策略

- 继续使用锁定 SHA 的 `Swatinem/rust-cache`；
- 保留 `save-if: github.ref == 'refs/heads/main'`，PR 只恢复、不写入长期缓存；
- matrix Job 使用同一 Job id，因此共享 integration cache 命名空间；Quality 与 Unit/Doc 分别维护适合
  自身 profile/target 的缓存；
- 缓存 miss 只影响耗时，不能影响命令和结果；
- 首个改造 PR 可能因为新 Job cache key 冷启动而偏慢，收益以合并后两次 warm-cache 运行评估。

不在第一阶段上传/下载整个 `target` artifact。它会增加大文件传输、序列化和清理成本，还会引入一个
所有测试都必须等待的 build Job，可能重新制造关键路径。

## 失败与取消语义

- integration matrix 关闭 fail-fast，让所有模块给出完整结果；
- workflow 继续使用现有 concurrency，新提交会取消同一 PR 的旧运行；
- 被 concurrency 取消的必需 Job 会让对应旧运行的 summary 非成功，不会被误认成绿灯；
- 分类器失败发生在 Cargo 前，日志明确列出未知或冲突 target；
- 任一模块测试失败只标红该模块和汇总 Job，其他模块结果仍保留。

## 测试策略

### 自动化用例矩阵

| # | 层 | 场景 | 输入/前置 | 预期 | 优先级 |
| --- | --- | --- | --- | --- | --- |
| CI-01 | 单元 | 当前测试 inventory 正常分类 | 扫描仓库现有 `tests/*.rs` | 每个 target 恰好属于一个批准模块，四个模块均非空 | P0 |
| CI-02 | 单元 | 未知测试文件 | 加入不匹配任何规则的 target | 分类失败并列出未知 target，不回退到 Platform | P0 |
| CI-03 | 单元 | 规则重叠 | 同一 target 同时命中两条模块规则 | 分类失败并列出全部冲突模块 | P0 |
| CI-04 | 单元 | 非法或空模块 | 请求未知模块，或 inventory 使批准模块为空 | 执行前失败，不启动 Cargo | P0 |
| CI-05 | 单元 | Cargo 命令生成 | 任一合法模块的 targets | 含 `test --locked --all-features`，每个 target 恰好一个 `--test` | P0 |
| CI-06 | 单元 | list 模式 | 合法 inventory | 稳定输出四模块及有序 targets，不调用 subprocess | P0 |
| CI-07 | 静态集成 | Workflow 矩阵 | 解析 `.github/workflows/ci.yml` | 四模块、`fail-fast: false`、正确 runner 脚本入口 | P0 |
| CI-08 | 静态集成 | 服务隔离 | 解析各 Job | Quality 无 services；Unit/Doc 与 Integration 均有 PostgreSQL/Redis | P0 |
| CI-09 | 静态集成 | 汇总门 | 解析 `ci-summary` | `always()` 且依赖 Quality、Unit/Doc、Integration，任一非 success 时失败 | P0 |
| CI-10 | 静态集成 | 发布回归 | 对比改造前后 release Job | 触发条件、Ubuntu 22.04、默认-feature build、digest 与 artifact 上传不变 | P0 |
| CI-11 | 本地集成 | 四模块真实执行 | 本地 PostgreSQL/Redis 可用 | 四个模块命令全部通过，并集等于全部 integration targets | P0 |
| CI-12 | 本地集成 | Unit/Doc 与 Quality | 执行批准命令 | lib/bins/doc、format、Python tests、Clippy 全绿 | P0 |
| CI-13 | GitHub 验收 | 并行启动与完整汇总 | 推送独立 PR | 四模块时间区间重叠；所有必需 Job 与 summary 结果一致 | P1 |
| CI-14 | GitHub 验收 | warm-cache 性能 | main 缓存预热后的后续 CI | 记录关键路径，目标约 4–6 分钟；超限时保留数据再评估细分 Lexicon | P1 |

### 分类器单元测试

- 当前 `tests/*.rs` 全量 inventory 与预期模块一一对应；
- 每个 target 只匹配一个模块；
- 未知 target fail closed；
- 非法模块名、空模块和重复归属报稳定错误；
- Cargo 参数包含 `--locked --all-features` 且每个 target 只出现一次；
- list 模式不启动 Cargo。

### Workflow 静态验证

- YAML 可解析；
- matrix 恰好包含四个批准模块；
- `fail-fast: false`；
- quality 无 services，unit/integration 有 PostgreSQL/Redis；
- summary 使用 `always()` 并依赖全部必需 Job；
- release-artifact 关键字段与基线一致。

### 本地命令

- 运行分类器单元测试；
- 分别运行四个模块命令；
- 运行 lib/bins 和 doc tests；
- 运行 format、Clippy 与 `git diff --check`；
- 对比模块 target 并集与 `tests/*.rs`，证明无漏跑和重复。

### 真实 GitHub Actions 验收

- 推送独立分支并创建 PR 后，核对四模块实际并发；
- 记录每个 Job 的 startedAt/completedAt、结论和实际测试清单；
- 证明总汇只在全部必需 Job 成功时成功；
- 缓存预热后至少再记录一次耗时，避免用 cold-cache 单次数据误判。

## 风险与回滚

- **Runner 用量增加**：四个数据库集成 Runner 会重复初始化与部分编译。接受以总 runner-minutes 换墙钟
  时间，并在两次 warm-cache 运行后复盘。
- **模块失衡**：Lexicon 当前最慢。第一阶段保持领域清晰；若关键路径仍超过 6 分钟，再拆为
  Lexicon HTTP 与 Lexicon storage/migration，而不是提前引入 hash shard。
- **缓存竞争**：matrix 在 main 上可能同时尝试保存同一 integration cache；Actions cache 不可变，先完成
  者成为该 key 的内容，其余 Job 仍不影响正确性。若后续命中率差，再引入明确 shared-key/save-owner。
- **新增测试漏跑**：分类器对未知 target fail closed，避免静默遗漏。
- **Workflow 配置失误**：改造只涉及 CI 文件与辅助脚本；回滚时恢复旧 `.github/workflows/ci.yml` 并删除
  分类脚本即可，业务产物和数据库无须回滚。

## 工作分解与估算

1. 分类器与单元测试：约 0.5 天；
2. Workflow 拆分、summary 和缓存接入：约 0.5 天；
3. 本地全量验证与真实 PR cold/warm-cache 验收：约 0.5–1 天。

总计约 1.5–2 天，主要不确定性来自 GitHub Runner 排队和 warm-cache 数据采集，不来自代码量。
