# 逐音色语速验证

## 最终结构与交互
- voice_profile={voices:[{voice_id,enabled,rate_percent}]}；无全局字段、不读旧结构。
- 默认1.00×；每行独立倍率按钮。调速和试听不改变勾选，取消勾选保留速率。
- 工具栏仅显示已选数量，常用8项/其他65项折叠保持。
- 外侧管理员试听在未勾选时仍可回退默认音色，并使用该音色自己的速率。

## 自动验证
- Voice editor：233项通过，包括两音色独立设置/试听、改其他音色不中止当前试听、取消勾选及回填。
- API client：423项通过，原生OpenAPI和runtime bundle的source hash一致。
- Admin全量首轮为1951通过、3失败、3跳过；旧全局语速断言及语音目录fixture调整后，3个相关文件复测115通过/1跳过；新增未勾选仍可试听回归所在文件28项通过。
- Rust lib：312通过/1跳过。
- lexicon_handler v3_voice_profile：2项通过，覆盖不同速率保存/读回/发布和非法配置。
- lexicon_audio_asset_references pronunciation_editor：2项通过，词形发音的逐音色配置经过保存、读回、发布快照保留。
- Rust clippy全目标/全特性、fmt检查通过；admin/voice-editor/api-client/types类型检查和所有修改TS文件ESLint通过。
- 独立审查发现未勾选导致外侧试听禁用，已修复并补回归。

## 本地浏览器
- 隔离预览：前端15440→后端18540，mock关闭，未部署服务器。
- 真实操作设置Sonia 0.75×、Ryan 1.10×，经“完成编辑→保存草稿→确认影响”返回200。
- 保存响应和刷新读回均保留两项不同速率，enabled均false，未设置音色1.00×。
- 记录：/tmp/tsz-per-voice-rate-preview-results.json；截图：/tmp/tsz-per-voice-rate-panel.png。
- 未执行付费合成，实际试听请求参数由自动测试验证。

## 发布状态
- 未合并或部署，继续等待用户确认其他事项。
- 本次无旧结构兼容或数据迁移；后续两端同批发布与旧数据处理需协调。
- 下次测试环境发布记得设置VITE_VOICE_AUDIO_UPLOAD=true。

## 提交前全量门
- 最终 `cargo test --locked --all-features` 退出0：1027通过、2跳过。
- 前端提交8cf2d1f已通过原生pre-commit/commit-msg和精确提交独立审查；发布仍暂停。
