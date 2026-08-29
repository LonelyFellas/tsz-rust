# AGENTS.md

## 部署

- 用户明确要求把 tsz-rust 后端部署到 tshb-test 时，主 Agent 必须把部署执行委派给项目级
  `backend_deploy_runner` 自定义 Agent（`.codex/agents/backend-deploy-runner.toml`）。如果当前 Agent
  已经是 `backend_deploy_runner`，则直接执行，不得再次递归委派。
- 仅讨论、询问、评估或准备部署不构成执行授权；提交、推送、合并与部署是独立授权边界。
- `backend_deploy_runner` 必须完整读取并严格遵守 `.agents/skills/deploy/SKILL.md`。该 skill 是后端
  部署目标、CI 门禁、服务器操作、备份、回退、冒烟验证与 manifest 验收的唯一事实来源。
