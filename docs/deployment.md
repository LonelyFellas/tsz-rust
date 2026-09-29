# 后端运行与部署入口

## 部署流程只维护一处

部署到 `tshb-test` 必须遵循[部署技能](../.agents/skills/deploy/SKILL.md)及其[操作手册](../.agents/skills/deploy/references/runbook.md)，由项目指定的部署执行代理处理。本文不另行维护安装、覆盖二进制、回退或 manifest 命令，避免与实际门禁分叉。

部署使用精确 `origin/main` 的成功 CI 制品；服务器没有 Git checkout，不同步源码或现场编译。提交、推送、合并、前端部署与后端部署分别需要对应授权，阅读本文不构成执行授权。

## 运行配置与依赖

- 环境变量的名称、必填项与默认值以 [config.rs](../src/config.rs) 和 [.env.example](../.env.example) 为准；示例中的开发凭据不能用于生产。
- 服务依赖 PostgreSQL 与 Redis。连接创建在 [main.rs](../src/main.rs)，启动过程见 [lib.rs](../src/lib.rs)。
- `run` 在监听之前应用内嵌迁移并初始化所需数据；升级不是简单替换二进制，必须遵守部署手册的数据库预检、恢复点与迁移兼容检查。
- `/healthz` 与 `/readyz` 是运行探针；服务 active 或探针成功不能替代登录、刷新、退出等完整 smoke，也不能证明部署来源正确。
- 生产环境使用 HTTPS 与 Secure cookie；`COOKIE_SECURE=false` 只适用于明确的临时 HTTP 测试环境，不能作为生产部署方案。
- 当前默认启动仍使用 `OtpSender::Mock`，详见 [lib.rs](../src/lib.rs) 和 [sender.rs](../src/otp/sender.rs)。不能把测试验证码通道当作真实短信发送能力。

## 运维专题

- [PostgreSQL 备份与恢复](postgresql-backup-restore.md)：恢复点、客户端版本与验证要求。
- [语音目录](../ops/speech-voice-catalog/README.md)：目录初始化与更新。
- [音频资产生命周期](../ops/audio-asset-lifecycle/README.md)、[预览对象生命周期](../ops/speech-preview-lifecycle/README.md)。
- [CI 配置](../.github/workflows/ci.yml)：构建与制品门禁以当前流水线和部署技能为准，不再维护已完成的优化实施计划。
