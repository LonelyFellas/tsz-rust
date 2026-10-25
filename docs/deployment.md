# 后端运行与部署入口

## 部署流程只维护一处

部署到 `tshb-test` 必须遵循[部署技能](../.agents/skills/deploy/SKILL.md)及其[操作手册](../.agents/skills/deploy/references/runbook.md)，执行精确候选携带的 `ops/deploy_test.py plan/apply/status/resume`。执行者按 AGENTS.md 的宿主约定处理；本文不另行维护安装、覆盖二进制、回退或 manifest 命令。真实正常发布已验收，真实回退尚未演练，自动 CD 未启用。

部署使用精确 `origin/main` 的成功 CI 制品；服务器没有 Git checkout，不同步源码或现场编译。提交、推送、合并、前端部署与后端部署分别需要对应授权，阅读本文不构成执行授权。

## 运行配置与依赖

- 环境变量的名称、必填项与默认值以 [config.rs](../src/config.rs) 和 [.env.example](../.env.example) 为准；示例中的开发凭据不能用于生产。
- `JWT_SECRET` 与 `ADMIN_JWT_SECRET` 各自使用 `openssl rand -base64 32` 生成，至少 32 字节且不同；所有构建均在连接数据库之前拒绝短值、空白、单字符重复值和 `replace-with-` 占位符。升级到强度校验版本前先检查现有配置，不输出密钥。轮换会使旧 access token 失效，但不会撤销数据库中的 refresh token；若需强制退出，须另行撤销会话。
- 服务依赖 PostgreSQL 与 Redis。连接创建在 [main.rs](../src/main.rs)，启动过程见 [lib.rs](../src/lib.rs)。
- `run` 在监听之前应用内嵌迁移并初始化所需数据；升级不是简单替换二进制，必须遵守部署手册的数据库预检、恢复点与迁移兼容检查。
- `/healthz` 与 `/readyz` 是运行探针；服务 active 或探针成功不能替代登录、刷新、退出等完整 smoke，也不能证明部署来源正确。
- 生产环境使用 HTTPS 与 Secure cookie；`COOKIE_SECURE=false` 只适用于明确的临时 HTTP 测试环境，不能作为生产部署方案。
- 密码登录使用 Redis 固定窗口预算：每个来源 IP 每 15 分钟最多 100 次请求；规范化登录标识每 15 分钟最多 10 次失败/在途校验，已绑定同一账号的手机与邮箱共享预算。登录凭据校验成功且账号可用时仅清该账号失败计数，IP 预算保留；超过预算返回 `429 login_rate_limited` 和 `Retry-After` 秒数，Redis 故障返回 `503 login_unavailable`。
- 登录来源 IP 来自真实 TCP peer。仅 loopback peer 被视为同机可信反代：优先使用它覆盖的 `X-Real-IP`，缺省时取 `X-Forwarded-For` 最右地址，头无效则回退 peer；其他 peer 忽略所有来源头。部署须保持 nginx 同机并覆盖 `X-Real-IP $remote_addr`、追加 XFF，后端应绑定 loopback 或由防火墙限制入口，不能把非同机反代默认视为可信。没有连接信息时所有请求共用保守的 `unknown` 桶。
- 当前默认启动仍使用 `OtpSender::Mock`，详见 [lib.rs](../src/lib.rs) 和 [sender.rs](../src/otp/sender.rs)。不能把测试验证码通道当作真实短信发送能力。

## 运维专题

- [PostgreSQL 备份与恢复](postgresql-backup-restore.md)：恢复点、客户端版本与验证要求。
- [语音目录](../ops/speech-voice-catalog/README.md)：目录初始化与更新。
- [音频资产生命周期](../ops/audio-asset-lifecycle/README.md)、[预览对象生命周期](../ops/speech-preview-lifecycle/README.md)。
- [CI 配置](../.github/workflows/ci.yml)：构建与制品门禁以当前流水线和部署技能为准，不再维护已完成的优化实施计划。
