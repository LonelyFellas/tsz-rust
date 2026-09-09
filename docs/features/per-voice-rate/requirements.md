# 每个音色独立语速

- 每个音色独立设置语速；未设置时为自身原速 1.00×，没有统一语速入口或继承规则。
- 一行包含勾选、音色名称、倍速按钮、试听；倍速支持预设与自定义。
- 勾选代表未来给 C 端提供该音色音频，默认 false；调速、试听不改变勾选。
- 取消勾选保留该音色速度；更多音色继续折叠，收起不丢设置。
- 试听、保存、读回、发布使用相同的逐音色配置。
- 不兼容旧数据结构，不做迁移，不保留全局语速字段。
- 发布继续暂停；下次测试环境发布需开启 VITE_VOICE_AUDIO_UPLOAD=true。

验收：`SQLX_OFFLINE=true cargo test --locked --all-features --test lexicon_handler v3_voice_profile`
