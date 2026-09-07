---
name: ship
description: 审查并按授权提交、推送 tsz-rust 后端改动，创建或更新面向 dev 的 PR。包含 Rust、SQLx、API/迁移验证和精确提交的独立 pre-push 审查；不用于仅实现或部署。
---

# 后端交付

分支模型：feature → dev → main。PR 以 dev 为 base；合并 main 和部署使用各自授权及流程。

## 1. 基线与授权

- 检查完整 diff、分支、remotes、已有 PR 和用户改动。fetch 后固定本次 origin/dev 基线，保留任务已有分支。
- 在 main/dev 上时为本次工作建立 `codex/<slug>` 分支；不向 main/dev 直接提交或推送。分支已有已合并 PR 时先核对剩余差异，必要时建立干净的新任务分支。
- 「提交并推送」「开 PR」「ship」已经授权相应交付步骤；沿用本会话授权，不逐步重复确认。「仅提交」不含 push，「仅审查」不含 commit，实施批准不自动包含交付。
- 仅审查时交付发现与验证局限，不自动改文件、刷新 .sqlx 或进入提交阶段；修复与写入需要原请求包含相应范围。
- 缺少授权时，先完成可审查的结果、验证与提交说明，再一次性请求缺少的决定。需要停下时链接本文件并引用触发规则。
- 核对 `core.hooksPath` 和 [.githooks](../../../.githooks)；按仓库约定启用 hooks，绝不通过改配置绕过。GitHub 是 PR/CI 来源，Gitee 镜像仅在用户要求时同步。

## 2. 根据 diff 审查风险

| 变化      | 必要检查                                                      |
| --------- | ------------------------------------------------------------- |
| API/DTO   | 状态码、Problem Details、字段/枚举、OpenAPI、幂等、调用方兼容 |
| 数据/并发 | 事务边界、锁、唯一/外键、历史数据、失败原子性与并发冲突       |
| SQLx/迁移 | 缓存、up/down、迁移顺序、新旧 schema 与二进制兼容性           |
| 鉴权/IO   | 权限、cookie/token、输入、SQL/命令构造、敏感日志与配置        |
| 性能/测试 | N+1、阻塞调用、无界结果、关键回归与错误路径的证据             |

API 响应新增字段也要核对实际消费者。V3 admin 等严格 runtime contract 可能拒绝未知字段；
若命中，先同步前端契约并验证兼容，在 `docs/frontend-integration.md` 与 PR 写清发布顺序。
不能把某个 DTO 的历史问题泛化成所有接口一律前端先部署。
需要跨仓同步时定位已安装的 `contract-sync`；配套交付再读其 `references/paired-release.md`，把兼容性、目标版本和顺序纳入已有 PR。若个人技能不可用，仍按原生导出流程核实实际前端消费者；此步骤不自动授权前端修改、交付或部署。

纯文档只做相关一致性、格式和引用检查；不套用前端的 coverage、UI 或 SEO 规则。

## 3. 统一安排质量门

读取当前 CI、Cargo 配置和 hooks，复用同一代码/配置/环境状态下已有结果。
迭代跑受影响的 `--lib`、`--test <target>` 或契约检查；完整代码变化交付前安排一次：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

clippy 已包含编译检查，不为同一状态额外重复 `cargo check`。正常 hooks 仍必须执行。
完整测试需要本地 Postgres/Redis；先检查仓库配置的测试依赖与隔离数据，不能拿快速单测冒充集成通过。
API 修改按原生导出流程更新 OpenAPI；SQL/schema 修改使用 `cargo sqlx prepare -- --all-targets --all-features` 并审查 .sqlx 差异。
代码/测试/配置变化后旧结果只适用于未受影响范围；测试恢复文件后须确保构建缓存没有沿用错误产物。

耗时命令使用可继续读取的会话句柄，保存真实退出码。若与只读审查并行，固定相同输入，审查结果不能将仍运行的检查写成通过。
不要在每个技能、提交或审查回合重复全量门；发现失败时先做定向复验。

## 4. 提交与一次独立审查

1. 检查 `git diff --check` 和 staged diff，只提交本任务文件，使用 conventional commit；署名不写死历史模型。
2. 正常 pre-commit 会刷新并暂存 .sqlx、运行 clippy。核对生成 diff 确属本任务；保留无关用户改动，需要时用隔离 checkout。
3. 提交后固定完整 `review_base_sha`（本次 origin/dev）与 `review_sha`（HEAD）。使用可用独立 review 工具；不可用时本技能要求一个新的只读 reviewer 子代理。
4. 给 reviewer 原目标、精确 SHA、项目规则和实际测试结果。只读提交对象及直接受影响调用链，检查完整 `base_sha...review_sha`；不传自审结论，不重跑已提供的全量检查。脏工作区使用隔离 checkout。
5. P0–P2 或其他实质正确性/安全问题阻断 push，误报用证据排除；P3 风格/清理不扇出额外 verifier。
6. 修复后记录旧/新 SHA，重跑受影响检查，请同一 reviewer 检查 `old_sha..new_sha`、原问题与相关调用链。不变部分沿用初审证据，最终结论覆盖当前 SHA。范围扩大或原审查假设失效才全量重审。
7. 不设「只准修一轮」的限额，也不靠无限复审拖延；在安全点报告范围/耗时偏差并收敛下一步。reviewer 不可用或存在阻断时不能推送。

窄修复沿用已给授权；新增范围或破坏性取舍才重新确认。提交组织按可审查、可回退主题决定，不强制 amend 已发布历史。

## 5. 推送与 PR

正常 `git push -u origin <branch>`，让 pre-push 执行原生门禁。绝不使用 `--no-verify`、禁用 hooks、跳过 CI 或普通 force。
失败读实际日志，区分 hooks、网络、认证；不要猜代理问题、擅自修改全局网络配置或引用不可访问的记忆。
修复产生新 SHA 时补齐增量审查再推送。

创建 `--base dev` 的 PR，已有同任务 PR 就更新；正文写清结果、契约/迁移、发布顺序、验证、审查与回退限制。多行正文用结构化参数或 `--body-file`。
报告 SHA、reviewer、检查状态、PR 链接和未验证事项。GitHub CI 全绿后才具备合并条件，合并与部署仍按各自授权执行。
