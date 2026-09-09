# 音色选择与折叠验证

## 当前结果
- 未配置时默认 0 个选择；只有明确保存过的启用选择才回填（后续逐音色结构见 per-voice-rate）。
- 常用 8 个直接显示，其他 65 个默认折叠；后端返回完整 73 项，单次本地只读查询。
- 选择保存为未来 C 端音频配置；试听不改变选择。当前尚无 C 端自动合成任务。
- 本任务隔离预览已更新；未推送或部署测试环境。

## 复现与回归
- 新增默认不选回归，在旧逻辑上因 checkbox 初始为 true 而失败，修正后通过。
- 试听不改变 checkbox 或 voice_profile，明确选择回填保持正确。
- 折叠组件回归覆盖展开、选择、收起、再展开后仍选中；采用直接组件测试避开 jsdom 中 antd 弹层动画的可见性限制。
- 真浏览器：初始 8 可见 / 73 总数 / 0 选中；展开显示 73；选择 Jenny 后折叠标题“已选 1”，重新展开仍选中；全程未触发合成请求。
- 用户预览恢复为 0 选中并折叠更多项；未保存测试操作。
- 浏览器记录：/tmp/tsz-voice-selection-preview-results.json；图：/tmp/tsz-voice-selection-panel.png。

## 本轮检查
- Rust lib：312 passed / 1 ignored。
- speech_preview_handler、speech_preview_service、speech_voice_catalog_seed：共 11 项通过。
- cargo clippy --locked --all-targets --all-features -- -D warnings：通过。
- cargo fmt --all -- --check：通过。
- voice-editor：232 项通过；API client：423 项通过；admin 语音适配器/数据源：27 项通过。
- admin、voice-editor、api-client、types 类型检查及改动文件 ESLint/Prettier 通过。
- OpenAPI 原生导出、前端同步完成，source hash 一致；本轮无新增 SQL 查询宏或迁移。
- 此次语义修正前的全量 Rust 基线为 1027 passed / 2 ignored，本轮运行受影响回归与全部 lib。

## 环境与交付
- 后端 dac7ffc814cde9690f9d3324423c9236a8711ae2 + 工作区差异，codex/azure-voice-catalog。
- 前端 eb8bece2faa4103f0e14b6def91a9af2db142090 + 工作区差异，codex/azure-voice-catalog-ui。
- 本地前端 :15440 → 真实后端 :18540，隔离库 tsz_azure_preview_20260909、Redis :56391/0，mock 关闭。
- 服务句柄：/tmp/tsz-azure-preview-handles.json。
- 配套发布前端先、后端后；历史配置不清空。

验收：`SQLX_OFFLINE=true cargo test --locked --all-features --test speech_voice_catalog_seed --test speech_preview_handler --test speech_preview_service`
