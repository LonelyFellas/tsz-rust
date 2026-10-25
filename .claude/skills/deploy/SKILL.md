---
name: deploy
description: 将 tsz-rust 当前 GitHub main 的成功 CI 制品部署到 tshb-test，执行精确来源校验、部署锁、备份、原子替换、完整冒烟与回退。仅用于明确的后端部署请求；讨论或准备部署不触发服务器写入。 兼容入口，规则复用本仓库 .agents 技能。
---

# deploy 项目入口

执行前读取 [项目 deploy 技能](../../../.agents/skills/deploy/SKILL.md)，并按其中要求读取相关引用文件。
该文件是流程的唯一维护位置；本入口不维护独立的授权、分支、测试、审查或部署规则。

- 从当前 checkout 解析链接，不能读取另一 worktree 的同名文件。
- 当前会话已加载对应项目技能时沿用其流程，不重复执行。
- 发布和恢复统一调用精确候选的 `ops/deploy_test.py`，不维护手工 SSH/rsync/manifest 步骤。
- 执行者按项目 AGENTS.md 的宿主约定处理：Claude 直接执行同一控制器流程，不强制寻找 runner；不冒充不可用工具或以兼容为由跳过质量门。
