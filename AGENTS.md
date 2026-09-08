# AGENTS.md

## 工作流（阶段 = 会话）

技能链路 `assess → build → ship`，Bug 走 `bugfix → ship`；部署见下节。
**每个阶段跑完就切新会话**，交接靠 spec / PR 正文——上下文会一路涨到 800k+，
成本按会话长度平方增长，且模型表现随上下文填满而下降。

- **评估分档**：一句话能说清 diff 就直接做；单模块内且不动 DTO/迁移/OpenAPI/权限
  写一份 ≤80 行 `design.md`；契约变更、迁移、鉴权、数据约束或需前端配套才写双文档。
- **spec 结尾必须是一条能跑的验收命令**（通常是 `cargo test --test <target> <case>`）。
- **发布顺序是 spec 的必填项**：响应新增字段 → **前端先部署**（前端 V3 runtime validator
  拒绝未声明字段）；改必填/移除字段/收窄枚举 → **后端先部署**；拿不准写「同批发布」。
- **调研派只读子代理**（Claude 用 `rust-scout` / `contract-auditor`），主上下文只收结论。

## 部署

- 用户明确要求把 tsz-rust 后端部署到 tshb-test 时，主 Agent 必须把部署执行委派给项目级
  `backend_deploy_runner` 自定义 Agent（`.codex/agents/backend-deploy-runner.toml`）。如果当前 Agent
  已经是 `backend_deploy_runner`，则直接执行，不得再次递归委派。
- 仅讨论、询问、评估或准备部署不构成执行授权；提交、推送、合并与部署是独立授权边界。
- `backend_deploy_runner` 必须完整读取并严格遵守 `.agents/skills/deploy/SKILL.md`。该 skill 是后端
  部署目标、CI 门禁、服务器操作、备份、回退、冒烟验证与 manifest 验收的唯一事实来源。
