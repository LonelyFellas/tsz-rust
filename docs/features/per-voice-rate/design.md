# 逐音色语速实现

## 依赖
- 复用目录、试听 API、voice_profile JSONB 存储与发布回填。
- 修改 DTO、前端镜像、运行时 schema、编辑器状态和目标音色试听参数。
- 新增每行倍率按钮与设置浮层；移除全局语速栏。

## 数据和交互
- 新结构：voice_profile = { voices: [{ voice_id, enabled, rate_percent }] }。
- 未出现的音色默认 enabled=false、rate_percent=0；只保存用户编辑过的音色。
- enabled 与 rate_percent 独立，取消勾选只改 enabled，重新勾选恢复原有速度。
- 每行独立的 0.50×/0.75×/1.00×/1.25× 与自定义入口，校验通用范围及目标音色能力。
- 保存整型相对百分比 -50..100，不保存展示用倍数字符串。
- 试听按目标音色计算指纹；只改变其他音色的设置不停止当前音色，当前音色或正文变化才失效。
- 工具栏仅显示“已选 N 个音色”。

## 契约与数据影响
- 旧 voice_ids / 全局 rate_percent 字段严格拒绝；无双结构读取、无迁移。
- 每项 voice_id 唯一、非空、最多64字符，无 NUL；最多2000项；enabled必须为布尔。
- 历史下线 alias 继续允许保存；不通过目录外键校验阻断旧音色的修改。
- 原生导出 OpenAPI 并同步严格前端 schema，旧测试夹具改用新结构。
- JSONB与发布克隆保留整个profile，无数据库 schema migration。
- 破坏性契约需要两端同批发布及数据处理协调；本次不改线上旧数据，发布暂停。
- C 端自动生成/分发任务不在本次范围，勾选只保存后续音频配置。

## 验证
- 两个音色设不同速率、试听请求分别携带对应值；改另一音色不打断当前试听。
- 保存/读回/发布保留每项enabled与rate，取消勾选后重新打开保留速度。
- 非法速率、重复ID、过长列表和旧结构被拒绝。
- 实际本地浏览器验证默认1.00×、每行调速、独立勾选和布局。

`SQLX_OFFLINE=true cargo test --locked --all-features --test lexicon_handler v3_voice_profile`
