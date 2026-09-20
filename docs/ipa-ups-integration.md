# IPA / UPS 接入

本任务最终产品决定为 IPA/UPS，不新增 SAPI。
统一需求与设计位于前端仓库 `docs/features/ipa-ups/`；本地账号、进程与音频验收记录保留在开发环境，不进入发布提交。

本仓变更仅涉及：
- PronunciationSynthesisV3 可选 use_spelling、ups_words 和 UpsWordV3；候选各自新增可选 ipa_locale/ups_locale（en-GB|en-US）。旧字段 alphabet/ipa/ups 不变。
- 新元数据结构上限校验；显式 UPS 来源的多词完成校验要求正文和 UPS 字符串与逐词数据一致。
- 旧记录未提供新字段时不输出新字段，保留原有来源、原 UPS SSML 行为。complete 校验新增固定方言与候选口音匹配；新 common phoneme 来源需明确口音，存草稿仍允许未确认。
- OpenAPI 原生导出，不执行 schema migration。

验证：在本任务隔离 PG/Redis 下 all-features --lib 297 项通过，fmt/clippy 通过；匹配前端的真实登录、单词/短语保存回读、正式 speech/previews 与 OSS 音频播放已验证。

新字段会被旧前端严格 schema 拒绝。需新 reader 先就绪，后端匹配后再开 writer；本次按用户明确要求从最新 main 的独立任务 worktree 提交和推送，不修改 dev、不合并或部署。
