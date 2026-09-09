# 音色目录与选择语义

## 依赖与实现
- 已有：完整 Azure API 客户端、speech.voices、单次试听、voice_profile 保存/发布回传。
- catalog-snapshot.json 固化已核对的 73 个目录条目，product.json 只维护 8 个常用项的名称和顺序。
- 启动时 ensure_catalog_voices 在事务内仅补缺，保留现有运维数据；页面 GET 只执行一次查询。
- GET 返回全部启用的英美 Azure 记录；display_name 为友好名，is_common 区分常用项，常用优先排序。
- sync_speech_voices 显式调用 Azure 后同步全部合格记录，核齐常用项后才写库；不随页面访问触发。
- 前端 VoiceOption.isCommon 传递分类；旧后端缺分类时按常用兼容。常用区直接显示，非常用区由“更多音色”折叠。
- 勾选仅从 voice_profile.voices 中 enabled=true 的条目恢复，缺省为空；不再有“没配过就全选”的派生规则。
- 点击 checkbox 才改变音色选择，调语速保留当前选择；试听按钮不修改选择。
- 折叠状态独立于音色选择，标题提示折叠区已选数量。
- voice_profile.maxItems 继续保持 2000，历史配置不被本次修正清空。

## 音频边界
- 选择代表未来给 C 端提供哪些音色音频的持久化意图。
- 当前发布仅保留配置，不创建合成任务；实际合成仍只有管理员试听入口。
- 后续 C 端音频生成/分发必须另行实现，不能把试听缓存或 checkbox 当作音频已就绪。

## 契约与发布
- GET 增加 display_name、is_common，内部 alias 不变；无数据库迁移、无查询宏变化。
- 两端原生生成链同步 OpenAPI 与 runtime schema；前端先、后端后。
- 未提交或部署；预览使用本任务的隔离库与 Redis。

## 验证
- 未配置不勾选、试听不改变选择、明确选择回填、折叠后选择保留。
- 单次只读查询在 Azure 不可用时仍正常；预置目录补缺幂等，历史记录与人工设置保留。
- 真浏览器：初始 8 可见/73 总数/0 勾选，展开 73，选择与折叠不触发合成。

`SQLX_OFFLINE=true cargo test --locked --all-features --test speech_voice_catalog_seed --test speech_preview_handler --test speech_preview_service`
