# GitHub CI 模块化并行测试需求评估

## 背景与目标

`tsz-rust` 当前在一个 GitHub Actions Job 中依次执行格式检查、部署工具测试、Clippy 和
`cargo test --locked --all-features`。随着 Smart Lexicon V3 等数据库集成测试增长，单个
PR 的质量门已明显受全量测试串行执行限制。

2026-08-25 的 PR #64 CI 是本次评估基线：

- 完整 Job 耗时约 12 分 24 秒；
- 环境与容器初始化约 44 秒；
- Clippy 约 31 秒；
- 全量 Rust 测试约 11 分 03 秒，是明确的关键路径；
- 集成测试按拟议模块统计的测试执行时间约为：Lexicon 208 秒、Admin 179 秒、Identity
  163 秒、Platform 40 秒。

目标是在不减少测试、不降低失败门槛、不改变发布产物的前提下，把 PR 测试按业务模块放到
多个 GitHub Actions Runner 并行执行，使失败定位直接落到模块，并显著缩短开发者等待时间。

## 使用场景

- 开发者提交 PR 后，希望 Lexicon、Admin、Identity、Platform 测试同时启动，而不是等待前一
  模块全部结束。
- 某个模块失败时，开发者希望从 Job 名直接识别失败归属，并只重跑对应 Job。
- 新增集成测试文件时，维护者需要明确选择所属模块，避免测试因分片规则遗漏而没有执行。
- 合并到 `main` 时，仍必须完成与当前等价的格式、Clippy、单元、文档和全部集成测试。

## 功能范围

本次范围内：

- 将当前单 Job 质量门拆分为并行的质量检查、单元/文档测试和模块化集成测试；
- 集成测试至少按 Lexicon、Admin、Identity、Platform 四个模块并行；
- 保持 `--locked --all-features` 语义；
- 对测试文件做完整性校验，保证 `tests/*.rs` 每个文件恰好属于一个模块；
- 模块矩阵关闭 fail-fast，使一个模块失败后其他模块仍能跑完并提供完整证据；
- 保留一个稳定的汇总检查，便于 PR 页面、未来分支保护和部署前置检查消费；
- 继续使用 PostgreSQL 16、Redis 7 和现有 Rust/Cargo 缓存策略；
- 保持 `release-artifact` 的触发条件、构建环境和产物不变。

明确不在范围：

- 不修改业务代码、数据库迁移、OpenAPI、运行时配置或部署脚本；
- 不删除、跳过或标记忽略现有测试；
- 不引入需要额外维护的第三方测试运行器；
- 不把同一个测试重复放入多个模块来制造表面覆盖；
- 不在本次改造中调整测试本身的断言、超时或并发语义；
- 不修改 GitHub 仓库权限、Ruleset、Secrets 或计费设置。

## 模块边界

- **Lexicon**：`lexicon_*`、词典 schema、词库内容补全等词库领域集成测试。
- **Admin**：`admin_*` 管理员账户、会话、后台用户与偏好测试。
- **Identity**：用户认证、注册、OTP、会话、角色和学生/教师资料测试。
- **Platform**：健康检查、Catalog、Speech、Object Storage、Redis readiness 等平台能力测试。
- **Unit/Doc**：库、二进制目标和 Rust doc tests，不与 `tests/*.rs` 集成测试重复。
- **Quality**：格式、部署 manifest 工具测试和 Clippy，不依赖数据库服务。

具体文件归属在技术设计中冻结；分类器遇到无法归类的新测试文件时必须失败并提示维护者分配模块。

## 约束与边界

- GitHub Actions 权限保持 `contents: read`；不得为并行化增加写权限或 Secrets。
- 需要数据库或 Redis 的 Job 使用相互隔离的 service container；不能跨 Runner 共享可变数据库。
- 每个模块必须使用同一 Rust toolchain、Cargo.lock 和 feature 集合，避免不同 Job 验证不同代码面。
- 缓存只能优化编译，不能成为测试正确性的前置条件；冷缓存也必须能成功完成。
- 当前 GitHub `main` 没有启用 branch protection，且 Ruleset 为 disabled；仍需提供稳定汇总 Job，避免将来启用保护时再次改名。
- 并行 Runner 会增加总 runner-minutes。优先优化开发等待时间，但需要在上线后记录实际用量。

## 可观测性

- PR 页面必须显示 Quality、Unit/Doc、Lexicon、Admin、Identity、Platform 和汇总结果。
- 每个模块 Job 在日志开头输出本次实际选择的测试 target 清单。
- 未归类、重复归类、空模块都应给出明确错误，不允许静默成功。
- 保留现有 workflow concurrency，新的提交到来时取消同一 PR 的旧运行。

## 验收标准

- [ ] 四个集成测试模块在 GitHub Actions 中作为独立矩阵 Job 并行运行。
- [ ] `strategy.fail-fast` 为 `false`，单模块失败不取消其余模块。
- [ ] 所有当前 `tests/*.rs` 被恰好分配一次；新增未归类文件会让质量门失败。
- [ ] 库、所有二进制目标和 doc tests 仍被执行。
- [ ] 格式检查、部署 manifest 工具测试和全目标全 feature Clippy 仍被执行。
- [ ] 所有 Rust 测试命令继续带 `--locked --all-features`。
- [ ] 需要的测试 Job 各自拥有 PostgreSQL 16 和 Redis 7，不依赖其他 Job 的可变状态。
- [ ] `release-artifact` 与改造前的行为和产物保持一致。
- [ ] 存在稳定的总汇检查；任一必需 Job 失败或取消时，总汇检查失败。
- [ ] 本地分类器测试、workflow 格式检查、项目原生格式/Clippy/测试门全部通过。
- [ ] 在一次真实 PR CI 中证明模块并行启动且没有测试遗漏。
- [ ] 缓存预热后，目标 PR CI 关键路径约 4–6 分钟；若未达到，保留各 Job 数据再决定是否细分 Lexicon。

## 开放问题

1. 是否接受为缩短墙钟时间而增加总 GitHub Actions runner-minutes。推荐接受，并在两次 warm-cache CI 后复盘。
2. Lexicon 模块单独约 208 秒，第一阶段建议先保持领域完整；若真实关键路径仍超过 6 分钟，再把它拆成 HTTP 与 storage/migration 两个子模块。
