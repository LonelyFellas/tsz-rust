---
name: contract-sync
description: 协调 tsz-rust 与实际前端/Agent 消费者的契约版本、同步顺序和兼容验证；维护 Rust OpenAPI 导出，消费者生成由各自仓库负责。
---

# 契约同步与跨仓顺序

通用授权与主记录遵循 [AGENTS.md](../../../AGENTS.md)。本文件维护跨仓顺序与 Rust 导出；前端快照/wire 操作读取选定 tsz 的同名技能，Agent schema/协议读取选定 tsz-agent 的 test 技能及 `docs/integration.md`。
只要求核对时保持只读，不运行生成器；同步/修复请求完成必要本地改动与验证，不自动提交或部署。

## 1. 固定实际参与者

从用户目标、Git 根目录和 remotes 核实各任务 checkout，必要时用 `git worktree list` 定位；主仓、相邻目录、端口及历史文档都只是候选。
记录本次涉及的仓库路径、分支、HEAD/脏差异、端点及实际消费者。未提交源码生成的结果归为「SHA + 工作区差异」，不冒充纯提交产物。
只加载受影响消费者；没有前端变化的服务间协议不强行加入 UI 流程。缺少配套版本时完成本仓检查，标记兼容性未验证，不默读别的主仓或用旧快照补证。

沿 handler/DTO → `docs/openapi.json` → 消费者运行时模型/快照 → 实际调用点核对。`docs/frontend-integration.md` 和 Agent 对接文档帮助定位，历史 SHA/端口不是当前值。

## 2. 按所属仓库生成

在选定 Rust checkout 中导出：

```bash
SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi
```

该二进制只生成 `docs/openapi.json`，不启动服务；编译依赖当前 `.sqlx`。缓存缺失先查 SQL/schema 变化，并按 [dev-env](../dev-env/SKILL.md) 核实隔离库，不改用裸服务启动或随意连接共享库。
已有相同实现、配置、依赖输入的导出证据就复用；生成目标有用户改动时先核实归属，不能覆盖未知差异。

实际涉及前端时，在所选 tsz 读取 `.agents/skills/contract-sync/SKILL.md` 的前端生成步骤，传入本次 Rust spec 的绝对路径；该步骤不重新启动本节流程。
涉及 Agent 时按其导出检查与实际校验模型对账，不能让 Agent 改写 Rust OpenAPI。发现文档/实现不一致先查清正确行为，不用生成结果为错误实现背书。

## 3. 证明行为和兼容性

选择本次 method/path、状态码、Problem Details、字段必填/可选/nullable、枚举和权限分支。
严格 runtime validator 可能拒绝新增字段；不能默认响应新增字段安全，也不能一律前端先发。核实「已部署消费者 + 候选服务」「候选消费者 + 已部署服务」「目标组合」，迁移另核实回退二进制与新数据。
前端 `PENDING` 只用于后端确实未实现且有原因的端点，不能掩盖已实现接口不一致。

运行受影响 Rust handler/序列化测试及各消费者自己的契约检查；Rust 测试与隔离遵循 [test](../test/SKILL.md)。各仓最终门在主任务统一安排，同状态结果复用，不重复全量验证。
需要真实业务链路时组合读取参与服务的 dev-env/test；页面验收使用所选 tsz 的 `.agents/skills/test/references/real-backend-acceptance.md`。mock 和各仓测试通过不等于真实联调。

## 4. 交接和发布

报告端点变化、各仓版本/脏状态、spec 来源与哈希、消费者、验证及剩余兼容风险。配套发布按 [配套发布清单](references/paired-release.md) 交给各仓 ship/deploy，不扩大原授权。
