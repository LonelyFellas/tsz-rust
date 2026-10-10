---
name: ship
description: 审查并按授权提交、推送 tsz-rust 后端改动，创建或更新从任务分支面向 main 的 PR。包含 Rust、SQLx、API/迁移验证和精确提交的独立 pre-push 审查；不用于仅实现或部署。
---

# 后端交付

通用授权、任务分支与证据复用遵循 [AGENTS.md](../../../AGENTS.md)；沿用主记录，不重新评估或逐步确认。

## 1. 基线与授权

- 核对原请求、完整 diff、remotes、已有提交/PR 和用户改动，fetch 后固定 origin/main 基线；只交付本任务差异。仅提交不含 push，仅审查不改文件或生成物；修复与写入须在原请求范围内。
- 已有本任务 worktree/PR 复用；旧 PR 已合并则为剩余差异建新任务分支。detached HEAD 或其他任务 checkout 中的改动须保护，在独立任务 worktree 中只迁移本次范围。
- 缺少交付授权时先完成可审查结果、验证与提交说明，再一次性说明缺项；合并与部署按已有对应授权判断。
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
若命中，先同步前端契约并验证兼容，在主记录/PR 写清发布顺序；只有跨任务仍有效的对接约束才更新 `docs/frontend-integration.md`。
不能把某个 DTO 的历史问题泛化成所有接口一律前端先部署。
需要跨仓同步时读取本仓 [contract-sync](../contract-sync/SKILL.md)；配套交付再读 [配套发布清单](../contract-sync/references/paired-release.md)，把兼容性、目标版本和顺序纳入已有 PR；此步骤不自动授权前端修改、交付或部署。

纯文档只做相关一致性、格式和引用检查；不套用前端的 coverage、UI 或 SEO 规则。

## 3. 统一安排质量门

读取当前 CI、Cargo 配置与 hooks，复用实施阶段的验收核对与有效检查，只补缺项。原生 hooks 承接 clippy 与单元测试，独立审查只安排第 4 节，不在自检中再加一轮。
代码变化补 fmt 及 hooks 未覆盖的必要定向检查；数据库集成测试按本次风险选择，跨模块风险或用户明确要求时再全量运行，不能将未运行的集成检查写成通过。
API 修改按原生生成链更新 OpenAPI；SQL/schema 修改的 `.sqlx` 刷新由正常 pre-commit 承接，审查其生成差异。仅开发过程中确需缓存才能验证时提前生成，不重复手工刷新。
纯规则文档提交由 hook 的保守白名单判定；配置、脚本、源码、生成输入等仍执行原生门禁。hooks 始终正常运行。
耗时命令保留句柄与真实退出码；检查尚未结束时不报告通过。

## 4. 提交与一次独立审查

1. 检查 `git diff --check` 和 staged diff，只提交本任务文件，使用 conventional commit；署名不写死历史模型。
2. pre-commit 对查询或构建输入变化刷新并暂存 .sqlx；非纯规则文档提交仍运行 clippy。核对生成 diff 确属本任务；保留无关用户改动，需要时用隔离 checkout。
3. 提交后固定完整 `review_base_sha`（本次 origin/main）与 `review_sha`（HEAD）。使用可用独立 review 工具；不可用时本技能要求一个新的只读 reviewer 子代理。
4. 给 reviewer 原目标、精确 SHA、项目规则和实际测试结果。只读提交对象及直接受影响调用链，检查完整 `base_sha...review_sha`；不传自审结论，不重跑已提供的全量检查。脏工作区使用隔离 checkout。
5. P0–P2 或其他实质正确性/安全问题阻断 push，误报用证据排除；P3 风格/清理不扇出额外 verifier。
6. 修复后记录旧/新 SHA，重跑受影响检查，请同一 reviewer 检查 `old_sha..new_sha`、原问题与相关调用链。不变部分沿用初审证据，最终结论覆盖当前 SHA。范围扩大或原审查假设失效才全量重审。
7. 不设「只准修一轮」的限额，也不靠无限复审拖延；在安全点报告范围/耗时偏差并收敛下一步。reviewer 不可用或存在阻断时不能推送。

窄修复沿用已给授权；新增范围或破坏性取舍才重新确认。提交组织按可审查、可回退主题决定，不强制 amend 已发布历史。

## 5. 推送与 PR

正常 `git push -u origin <branch>`，让 pre-push 执行原生门禁。绝不使用 `--no-verify`、禁用 hooks、跳过 CI 或普通 force。
失败读实际日志，区分 hooks、网络、认证；不要猜代理问题、擅自修改全局网络配置或引用不可访问的记忆。
修复产生新 SHA 时补齐增量审查再推送。

开 PR 前再次 `git fetch origin` 并检查与 origin/main 的合并冲突；发现冲突先处理、验证并补齐增量审查。
创建 `--base main --head <本任务分支>` 的 PR，已有同任务 open PR 就更新；正文写清结果、契约/迁移、发布顺序、验证、审查与回退限制。多行正文用结构化参数或 `--body-file`。
报告 SHA、reviewer、检查状态、PR 链接和未验证事项。GitHub CI 全绿后才具备合并条件，合并与部署仍按各自授权执行。
