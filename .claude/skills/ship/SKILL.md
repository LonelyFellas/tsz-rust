---
name: ship
description: 审查并按授权提交、推送 tsz-rust 后端改动，创建或更新面向 dev 的 PR。审查用分级 effort 的 /code-review 打在已提交 SHA 范围上，并行跑受影响测试，全量门交给原生 hooks；不用于仅实现或部署。
---

# 后端交付（Claude Code 版）

本文件是 Claude Code 下后端 ship 的**唯一**流程来源，自带完整规则，不读 `.agents/skills/ship/`
（那份是给 Codex 的宿主中立版，两边独立维护）。项目约定读 `AGENTS.md`。

分支模型：feature → dev → main。PR 以 **dev** 为 base；合并 main 和部署使用各自授权及流程。

## 1. 基线与授权

- 检查完整 diff、分支、remotes、已有 PR 和用户改动。fetch 后固定本次 `origin/dev` 基线。
- 在 main/dev 上时为本次工作建立分支；**不向 main/dev 直接提交或推送**。
- 「提交并推送」「开 PR」「ship」已授权相应交付步骤，**不逐步重复确认**；
  「仅提交」不含 push，「仅审查」不含 commit，实施批准不自动包含交付。
- 仅审查时交付发现与验证局限，不自动改文件、不刷新 `.sqlx`、不进入提交阶段。
- 缺少授权时先完成可审查的结果、验证与提交说明，再一次性请求缺少的决定。
- 核对 `core.hooksPath` 与 [.githooks](../../../.githooks)；按仓库约定启用 hooks，**绝不通过改配置绕过**。
  GitHub 是 PR/CI 来源，Gitee 镜像仅在用户要求时同步。
- **工作区常有用户的未提交改动——只暂存本次文件，保留其余，不清空工作区。**

## 2. 定向自检（只看本次 diff 实际触及的面）

| 变化      | 必要检查                                                     |
| --------- | ------------------------------------------------------------ |
| API/DTO   | 状态码、Problem Details、字段/枚举、OpenAPI、幂等、调用方兼容 |
| 数据/并发 | 事务边界、锁、唯一/外键、历史数据、失败原子性与并发冲突      |
| SQLx/迁移 | `.sqlx` 缓存、up/down、迁移顺序、新旧 schema 与二进制兼容性  |
| 鉴权/IO   | 权限、cookie/token、输入、SQL/命令构造、敏感日志与配置       |
| 性能/测试 | N+1、阻塞调用、无界结果、关键回归与错误路径的证据            |

**响应新增字段也要核对实际消费者**——前端 V3 严格 runtime contract 会拒绝未知字段。
命中时先同步前端契约并验证兼容，在 `docs/frontend-integration.md` 与 PR 写清发布顺序。
不能把某个 DTO 的历史问题泛化成"所有接口一律前端先部署"。
跨仓同步读 [contract-sync](../../../.agents/skills/contract-sync/SKILL.md)，
配套交付读 [配套发布清单](../../../.agents/skills/contract-sync/references/paired-release.md)；
此步骤**不自动授权前端修改、交付或部署**。

纯文档只做一致性、格式和引用检查，不跑运行时套件。

## 3. 提交

`git diff --check` 通过，只暂存本任务文件并检查 staged diff，使用 conventional commit。
正常 pre-commit 会刷新并暂存 `.sqlx`、运行 clippy——**核对生成 diff 确属本任务**，
hook 改写的文件必须纳入第 4 步审查范围。有混合 hunk 时精确暂存或用隔离 checkout。

## 4. 审查与测试——并行起跑，审查只一轮

提交后固定 `review_base_sha`（本次 `origin/dev`）与 `review_sha`（HEAD）。
**先发起 `/code-review`（后台跑），随即用 Bash 跑受影响测试**，两边结果回来再一起汇总。

### 4a. `/code-review`：档位按 diff 右尺寸，每次显式带 effort

`/code-review` **必跑**，不用自查代替。它自带审查 agent、不继承自检结论，
已满足独立 pre-push 审查门禁的要求，因此**不再另派 reviewer 子代理，全流程只审一轮**。

**每次显式带 effort 参数**——不带参数会沿用上次用过的档位：

| 改动性质                                                             | 档位     | 调用                  |
| -------------------------------------------------------------------- | -------- | --------------------- |
| 纯文档/注释/日志文案/测试断言，且增删合计 ≲ 80 行                    | 低，单轮 | `/code-review low`    |
| 一般 handler / service 逻辑，不动契约与数据约束                      | 默认     | `/code-review medium` |
| 迁移与数据约束、鉴权/权限、API 契约变更、事务与并发、删端点/下线链路 | 最高     | `/code-review xhigh`  |

混合按**最重的部分**定档。P0–P2 或其他实质正确性/安全问题阻断推送；
误报用证据排除，风格建议不阻断。修复后记录旧/新 SHA，重跑受影响检查，
**只对 `old_sha..new_sha` 增量复审**；范围扩大或初审假设失效才重审全范围。

### 4b. 受影响测试

跑受影响的 `--lib` / `--test <target>` / 契约检查。完整门在交付前跑一次：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

clippy 已含编译检查，不重复 `cargo check`。完整测试需要本地 Postgres/Redis，
先核实测试依赖与隔离数据，**不能拿快速单测冒充集成通过**。
耗时命令用可继续读取的会话句柄，保存真实退出码。

## 5. 推送与 PR

正常 `git push -u origin <branch>`，让 pre-push 执行原生门禁。
**绝不**使用 `--no-verify`、禁用 hooks、跳过 CI 或普通 force push。
失败读实际日志区分 hooks、网络、认证；不猜代理问题、不擅自改全局网络配置。
修复产生新 SHA 时补齐增量审查再推送。

创建 `--base dev` 的 PR，已有同任务 PR 就更新；正文写清结果、契约/迁移、发布顺序、
验证、审查与回退限制。多行正文用 `--body-file`。
报告 SHA、审查结论、检查状态、PR 链接和未验证事项。
GitHub CI 全绿后才具备合并条件，**合并与部署仍按各自授权执行**。

ship 完成是阶段边界——交接信息落到 PR 正文后，建议开新会话继续下一件事。
