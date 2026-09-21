# AGENTS.md

## 工作流

- 前后端统一使用任务分支 → main：新任务先 `git fetch origin`，从最新 `origin/main` 创建独立任务分支和 worktree；已有本任务 worktree 则复用。不在 main 或 dev 直接开发、提交、推送。旧 Skill 中与此冲突的分支约定不再适用。

技能链路 `assess → build → ship`，Bug 走 `bugfix → ship`；部署见下节。
默认在当前会话连续推进已授权阶段，复用现有证据；用户要求或实际上下文限制出现时再交接。

- **评估分档**：一句话能说清 diff 就直接做；单模块内且不动 DTO/迁移/OpenAPI/权限
  写一份 ≤80 行 `design.md`；契约变更、迁移、鉴权、数据约束或需前端配套才写双文档。
- spec 写明可执行的验收命令与预期；只能人工验收时给出步骤与判据。免 spec 的小改动直接使用任务描述与最小验证。
- 涉及配套发布时按实际 schema 与消费者验证新旧组合，再确定发布顺序；兼容性未知时补验证或兼容方案，不以「同批发布」代替中间版本验证。
- 小范围调查由当前 agent 完成；存在可独立并行的调查且收益明确时，使用只读子代理（Claude 可用 `rust-scout` / `contract-auditor`）。

## 部署

- 用户明确要求把 tsz-rust 后端部署到 tshb-test 时，主 Agent 必须把部署执行委派给项目级
  `backend_deploy_runner` 自定义 Agent（`.codex/agents/backend-deploy-runner.toml`）。如果当前 Agent
  已经是 `backend_deploy_runner`，则直接执行，不得再次递归委派。
- 仅讨论、询问、评估或准备部署不构成执行授权；提交、推送、合并与部署是独立授权边界。
- `backend_deploy_runner` 必须完整读取并严格遵守 `.agents/skills/deploy/SKILL.md`。该 skill 是后端
  部署目标、CI 门禁、服务器操作、备份、回退、冒烟验证与 manifest 验收的唯一事实来源。
