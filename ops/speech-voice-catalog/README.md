# 音色目录

- catalog-snapshot.json：保留原 73 个 GA 英美 Neural 音色及其能力、资料；2026-10-25T14:23:35Z 只读查询当前 Azure 资源目录，扩展为 14 个原生英语地区、123 个 GA Neural／Multilingual 音色。新增 50 项的官方 Gallery 资料检查时间为 2026-10-25T14:31:32.921856Z；每条 metadata 保留 source_url 和 checked_at。2026-10-25T15:33:53Z 再核资源后，仅加入3个GA HD Flash（Jimmie／Tiana／Tyler），当前快照126项；Neural HD／HD Omni在当前资源均未返回。排除38个MAI Preview、4个其他Preview及1个Deprecated，不以静态候选作为可用证明。Flash官方资料检查时间2026-10-25T15:34:42.683261Z；仅开放原速、原调、原量自然朗读，停顿／音素和其他参数明确拒绝。
- product.json：8 个常用项的中英文名称和展示顺序。

两份配置编译进后端。启动时仅补缺，不改已有 ID、alias、enabled、能力与限幅。
GET /api/v1/admin/speech/voices 只读全部已启用英语目录，返回原生 locale、display_name 和 is_common；分组分类为 GB/AU/NZ→uk、US/CA→us，其余英语地区→other，不改写合成地区。
非常用音色仍可选择，前端默认折叠；没有选择配置时全部不勾选。

显式设置目标 DATABASE_URL 与 Azure Speech 配置后，可执行能力更新：

```bash
cargo run --locked --bin sync_speech_voices
```

先获取 Azure 目录并核齐常用项，再事务同步所有合格音色；失败不改库。
版本按目录事实 hash 生成，保留人工停用和限幅；不删除历史记录。
新增/更新预置目录时同步维护 snapshot；中文名称属于产品配置。

seed.sql 保留旧版 Aria、Davis、Sonia 初始化数据，新服务不依赖手动执行它。
勾选只保存未来 C 端音色配置，当前没有自动为 C 端生成音频的任务。

## 描述元数据快照

GET /api/v1/admin/speech/voices 按数据库 provider_voice_id 匹配编译内置 metadata；未知 ID 省略该字段。语言地区、性别和可选风格继续来自原目录字段；metadata 不参与合成版本 hash，不改变能力、参数或管理员维护的 description_zh。

官方 Voice Gallery 页面：https://speech.microsoft.com/portal/voicegallery 。其目录查询使用 POST https://eastus.api.speech.microsoft.com/texttospeech/acc/v3.0-beta1/vcg/voices，请求体为 `{"queryCondition":{"items":[]}}`，携带页面 Origin/Referer 和浏览器 User-Agent（缺少时可能返回空体）；这是官方页面使用的 beta 接口，不能假设为稳定公共 API。只读查询并保存 JSON 到独立空目录，不执行响应内容，不请求合成或音频。

将查询响应保存为 `gallery.json`，记录请求实际完成的 UTC 时间，然后在仓库根执行离线合入：

```bash
python3 -I ops/speech-voice-catalog/update_metadata.py --input /absolute/path/to/gallery.json --checked-at '<实际查询时间，含时区>'
```

工具不联网、不访问数据库，只更新当前快照已有英语 ID 的 metadata；缺项、重复项、非 GA/Neural 或身份不匹配时在写文件前失败，不新增音色、不改 styles。AgeGroups 保留官方 Adult/YoungAdult/Child/Senior，model 按官方 modelSeries 的 Monolingual/Multilingual 映射为 neural/multilingual；仅已核实的 DragonHDFlashLatest 映射为 hd_flash，不按 persona 名字推断。description 保存英文原文，personalities/scenarios 是描述标签，缺失为 null；roles 缺失为 null，明确为空为 []。

2026-10-25 查询覆盖全部 73 个音色：年龄与介绍 73、个性 63、场景 62、角色全部为空。Gallery 的 Ryan 比资源目录多列 sad/whispering；可选风格仍以资源目录同步的 capabilities.styles 为准。source_url/checked_at 只证明资料来源及查询时间，不证明合成成功。更新后审查 diff，运行 `python3 -I ops/speech-voice-catalog/test_update_metadata.py` 和 Rust 目录/序列化测试，再重新导出 OpenAPI；此流程不替代上面的资源能力同步。
