---
name: feature
description: 评估并实现 tsz-rust 新能力，按风险处理迁移、SQLx、契约与验收；仅评估时交付方案，已有方案直接实施。不用于普通 Bug 或小改动。
---

# 后端新功能

通用任务、授权、分支与证据复用遵循 [AGENTS.md](../../../AGENTS.md)。先按用户请求确定终点：仅评估不写实现、不启动服务或迁移；已授权实施就连续推进，不因设计完成再次确认。

## 查清依赖与必要设计

沿 handler → DTO → service → repository → 数据约束检查已有能力和本次消费者。复用主任务设计与验收，只补剩余依赖、方案、接口/文件影响和未决取舍；小改动无需新建 spec。

- 迁移说明 up/down、执行顺序、历史数据影响、数据损失与回退限制。
- 契约/权限说明 method/path、状态码、错误、字段可选性/枚举与资源授权；按 [contract-sync](../contract-sync/SKILL.md) 核对实际消费者。
- 配套发布按 [配套发布清单](../contract-sync/references/paired-release.md) 验证过渡组合后决定顺序，不预设所有 API 都先发前端。

验收写可执行命令及判据，数据库相关步骤写明隔离条件。仅重大未决产品/范围/不可逆取舍交给用户；仅评估到此交付方案与缺项。

## 实施并同步生成物

按已有依赖与验收实施最小改动；需要启动服务或数据库验证时先读 [dev-env](../dev-env/SKILL.md)。启动主服务会执行迁移，不能用启动探测未知数据库。

- 迁移 up/down 成对，在隔离库验证两个方向及历史数据影响。
- SQL 变化按需要用 `cargo sqlx prepare -- --all-targets --all-features` 刷新并审查 `.sqlx`。紧接 ship 时可由正常 pre-commit 承接；确需缓存才能开发验证时提前生成，不重复同输入生成。
- API/DTO 变化按 [contract-sync](../contract-sync/SKILL.md) 导出 OpenAPI 并同步必要消费者；已有同输入导出结果直接复用。
- 新增测试 target 登记当前 CI 分区清单，确认被实际执行。

## 验证与交付

按 [test](../test/SKILL.md) 选择定向验证和统一最终门；当前 Agent 对照验收核对 OpenAPI、SQLx、迁移与 CI 登记，不另加独立审查。
已授权交付就继续 [ship](../ship/SKILL.md)，精确提交独立审查在该阶段执行；实施请求不自动包含提交、推送、合并或部署。
报告已实现、失败、未验证项与各仓状态；跨仓真实链路未验证时不能以两仓测试通过宣称可用。
