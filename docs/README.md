# 后端文档

## 开发与对接

[开发规范](../AGENTS.md) · [项目结构](project-structure.md) · [测试指南](testing-guide.md) · [OpenAPI](openapi.json) · [前后端对接](frontend-integration.md) · [错误码](api-errors.md) · [SQLx 速查](sqlx-type-mapping-cheatsheet.md)

## 业务参考

| 主题 | 文档 |
| --- | --- |
| 用户与账号 | [用户域](user-domain-reference.md)、[注销](account-deletion-design.md) |
| 登录与会话 | [登录](login-design.md)、[Token](auth-token-design.md)、[会话刷新](session-refresh-design.md)、[验证码](otp-design.md) |
| 管理端 | [管理域](admin-design.md)、[账号管理架构](admin-account-management-architecture.md) |
| 词库 | [V3 模型](word-data-model.md)、[词性配置](part-of-speech-config-design.md) |
| 参考词典 | [轻量词头导入](dictionary-import.md)、[完整正文导入](dictionary-content-import.md) |
| 语音与发音 | [Azure TTS](azure-tts-provider-design.md)、[TTS 预览](tts-preview-api-design.md)、[IPA/UPS](ipa-ups-integration.md) |
| 基础设施 | [Redis](redis-design.md)、[对象存储](object-storage-design.md) |

具体字段以 OpenAPI 为准，数据库以迁移为准；设计中的阶段状态不是当前服务器验收结论。

## 运维

[部署入口](deployment.md) · [数据库备份与恢复](postgresql-backup-restore.md) · [上传音频资产](../ops/audio-asset-lifecycle/README.md) · [预览生命周期](../ops/speech-preview-lifecycle/README.md) · [语音目录](../ops/speech-voice-catalog/README.md) · [词条发布工具](../ops/lexicon-publish/README.md)

## 未完成或待确认

- [词库领域重构](features/lexicon-domain-refactor/)：保留尚未关闭的批次范围与验收边界。
- [词形类型与方言解锁](features/form-type-dialect-unlock/)：待实施方案。
- 定级测试[产品方案](placement-product-plan.md)与[前端方案](placement-frontend-guide.md)：待评审，当前没有对应接口。

完成的一次性任务文档和过时方案直接删除，历史通过 Git 查询，不再新增归档副本。产品范围与品牌规范统一维护在配套总文档仓库；该仓库暂无远端，需取得本地 checkout。
