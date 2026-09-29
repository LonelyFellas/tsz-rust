# 词库 V3 模型与实现入口

本文说明当前 V3 模型的职责与源码位置，不复制 OpenAPI 的完整字段定义。

## 当前模型

[OpenAPI](openapi.json) 中的 `AdminWordV3` 是管理端词条聚合，包含 `forms`、`meanings`、修订与生命周期信息、发布状态及 `capabilities`。当前内容 API 只接受 V3，不恢复旧内容格式。

- **具体词形**：独立 `forms[]` 承载词形与地区变体；词形组通过 `WordFormGroupV3.members` 引用具体词形，不再使用 V2 的 `base_form + slots`。
- **词义与语义区间**：`WordSenseV3` 承载释义、关联词、例句及成分用词；绑定字段与必填约束以当前 schema 和校验器为准，不沿用某一阶段的单组限制。
- **富文本与发音**：语法、音色、连读、音频资产等遵守各自 V3 wire 结构；字典音标、实际发音与 Azure IPA/UPS 的转换边界见[IPA/UPS](ipa-ups-integration.md)。
- **修订号**：内容、标注、生命周期等有各自的并发边界，使用端点要求的修订号，不把某个 `revision` 用于所有写入。
- **发布版本**：`AdminWordPublicationV3` 包含发布编号、来源修订、发布时间及版本内容；发布、历史版本回退与依赖校验由后端服务处理，不能靠前端切换状态字段代替。
- **共享例句**：正文、译文、音频与句内关联由独立共享例句模块管理；引用、发布、撤回与可见性不是一回事，不按早期“词条内嵌例句”方案实现新功能。
- **关联候选**：搜索只消费当前发布内容；旧的草稿候选开关已移除。已有草稿引用的兼容边界见[对接指南](frontend-integration.md)。

## 代码与数据归属

| 职责 | 当前入口 |
| --- | --- |
| 端点与请求分派 | [router.rs](../src/lexicon/router.rs)、[handler/](../src/lexicon/handler/) |
| V3 契约与 DTO | [v3_contract.rs](../src/lexicon/v3_contract.rs)、[dto/](../src/lexicon/dto/)、[OpenAPI](openapi.json) |
| 业务写入与发布规则 | [service/](../src/lexicon/service/) |
| 草稿、发布、引用持久化 | [repository/](../src/lexicon/repository/) |
| 校验与节点身份 | [validation/](../src/lexicon/validation/)、[node_identity.rs](../src/lexicon/node_identity.rs) |
| 共享例句 | [shared_sentences/](../src/lexicon/shared_sentences/) |
| 音频资产 | [audio_assets/](../src/lexicon/audio_assets/) |
| 数据库结构与约束 | [migrations/](../migrations/)；按迁移顺序理解当前结构，不只看最初建表 SQL |
| 行为验证 | [lexicon_handler.rs](../tests/lexicon_handler.rs)、[shared_sentences.rs](../tests/shared_sentences.rs) |

## 不同资料的边界

- 内置参考词典不是可编辑词条库：[轻量词头导入](dictionary-import.md)与[完整正文导入](dictionary-content-import.md)有不同数据集和消费者，不能合并成同一个导入步骤。
- 尚未关闭的[词库领域重构](features/lexicon-domain-refactor/)保留批次取舍与验收边界；发布行为以当前 `service/`、`repository/` 与相应测试为准。
- 运行能力还受[配置](../src/config.rs)、权限和实体状态影响；存在 DTO 或端点不意味着每个环境都已开启。
- 新增或变更字段时同步后端 OpenAPI、前端生成契约与消费者测试，不再在此手写第二份接口表或建表脚本。
