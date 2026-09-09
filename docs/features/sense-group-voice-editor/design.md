# 实现方案

依赖路径：V3 DTO → 保存校验与 JSON 持久化 → OpenAPI → 前端运行时契约 → 英文编辑组件。
复用 RichTextV3、VoiceProfileV3、现有富文本校验和试听组件。
SenseGroupV3 增加可选 name_en_rich、voice_profile，缺省不序列化，保持旧记录形状。
纯文本仍保存在 name_en，禁止富文本与纯文本不一致。
前端 toWritableMeanings 必须保留两个新增字段；字段绑定仍为 group.id/name_en。
前端沿用旧的纯文本降级读取，不增加命名转换层。
本次不开放该字段的真人录音上传，以免上传无持久化归属的音频。
发布前需先交付单独的前端兼容契约补丁，再更新后端，最后开放编辑入口；当前完整候选不可直接前端先发。旧前端拒绝新字段，新前端向旧 API 写新字段也会失败。
验收：`cargo test --lib sense_group_voice`
