# TTS 领域模型与 Azure 适配器设计

## 1. 范围与决策

本阶段只交付 `speech` 领域的纯模型、canonical SSML builder、稳定缓存键、`SpeechProvider`
抽象和 Azure Speech HTTP adapter。它不增加 HTTP API、Redis 锁、对象存储缓存、正式音频 Worker
或生产开关，也不接受客户端 SSML。

本阶段不增加 migration。`speech.voices`、`synthesis_jobs`、`audio_assets` 和 `preview_cache` 的
写入生命周期依赖下一阶段的试听 API、缓存和任务边界；提前建一部分表会形成没有 owner 的持久化契约。
PR 3 应以本文的 `SynthesisRequest`、`SynthesisFingerprint` 和 `SpeechProvider` 为边界编排这些表。

## 2. 领域模型

- `Voice`: 服务端配置/目录中的稳定 voice，包含受校验的 provider identity、provider voice id、locale 和允许的 styles；调用方不能
  直接把任意字符串拼进 SSML。
- `SpeechOptions`: `style`、`rate_percent`、`pitch_semitones` 和固定 `output_format`。
- `SynthesisRequest`: canonical `RichTextV2` + 已校验 voice/options。
- `SynthesizedAudio`: 统一 MP3 bytes、`audio/mpeg` 和可选 provider request id。
- `SpeechProvider`: 异步、可替换的外部边界；普通测试使用 fake provider。

首个输出契约固定为 Azure `audio-24khz-96kbitrate-mono-mp3`。领域层只暴露该枚举值，避免调用方
制造 adapter 不支持或缓存键未覆盖的格式。

## 3. RichTextV2 到 canonical SSML

输入必须先通过现有 `lexicon::rich_text::canonicalize`；只接受 `RichTextV2`，不接受 V1 或 SSML
字符串。索引继续采用 Unicode codepoint 的 `[start, end)`，与 voice-rich-text-editor wire 一致。

builder 生成固定结构：`speak(version=1.0, xml:lang)` → `voice(name)` → 可选
`mstts:express-as(style)` → `prosody(rate, pitch)` → 内容。正文、IPA、voice、locale 和 style 均作
XML 属性/文本转义。`emphasis`、`phoneme`、`pause` 分别映射为 `emphasis`、`phoneme`、`break`；
`highlight` 与 `liaison` 仅是编辑展示信息，不影响发音。换行作为普通文本保留。

现有 RichText 校验已经拒绝越界、跨段落标注、重叠 phoneme、phoneme 内 pause，以及与 phoneme
局部交叉的 emphasis。builder 只处理 canonical 输入，并按码点事件生成确定性嵌套；任何不能形成
合法树的组合都会返回领域错误，而不是猜测语义。

SSML builder 版本为独立常量。任何标签结构、忽略规则、转义或格式化规则变化都必须提升版本。

## 4. 参数校验

- voice id、locale、style 使用保守字符集并限制长度；style 必须属于该 voice 的 allowlist；
- `rate_percent` 范围 `-50..=100`，`pitch_semitones` 范围 `-12..=12`；
- 没有 style 时不生成 `mstts:express-as`；不支持 style 的 voice 拒绝 style；
- 输出格式固定为 `audio-24khz-96kbitrate-mono-mp3`。

这些范围是本服务的稳定产品边界，不直接透传 Azure 更宽或未来可能变化的取值空间。

## 5. 缓存 fingerprint

缓存输入使用长度前缀的二进制字段编码后做 SHA-256，避免字符串拼接歧义。字段顺序固定为：

1. `schema_version`；
2. `ssml_builder_version`；
3. `provider`；
4. `voice_id` 与 `locale`；
5. `style`（显式区分 `None` 与字符串）；
6. `rate_percent`；
7. `pitch_semitones`；
8. `output_format`；
9. `normalized_content`（canonical `RichTextV2` 的确定性 JSON）。

hash 不含 Azure key、endpoint、随机 request id 或生成后的 SSML。PR 3 可将其作为 preview cache key
和 synthesis job 幂等键。

## 6. Azure adapter 与错误映射

`AzureSpeechProvider` 构造时创建一个全局复用的 `reqwest::Client`，配置 connect timeout 和整体
request timeout。endpoint 只由已校验 region 构造：
`https://{region}.tts.speech.microsoft.com/cognitiveservices/v1`；测试可注入 loopback endpoint，
生产配置不能覆盖 endpoint。

请求固定设置 subscription key、SSML content type、Azure output format 和客户端标识。响应采用
受限流式读取，超过上限立即失败，不先完整缓冲。成功响应还必须是 `audio/mpeg`（允许参数）。

稳定错误映射：

- 400 → `InvalidRequest`（不重试）；
- 401/403 → `Authentication`（配置/权限错误）；
- 429 → `RateLimited`（可重试）；
- 5xx → `Unavailable`（可重试）；
- connect/request timeout → `Timeout`（可重试）；
- 200 但响应超限 → `ResponseTooLarge`；
- 200 但 Content-Type 错误 → `InvalidResponse`；
- 其他网络/状态/协议错误 → `Unavailable` 或 `InvalidResponse`。

错误类型和日志不得携带 key、SSML、音频、响应正文或完整敏感响应；只允许记录稳定分类、HTTP
状态和经过校验的 provider request id。

## 7. 配置与安全边界

默认不设置 `AZURE_SPEECH_*` 时关闭且不构造 HTTP client。显式设置
`AZURE_SPEECH_ENABLED=true` 时，region/key 必须完整提供，timeout 和响应大小必须在安全范围内；
缺字段、孤儿字段、未知字段或非法值均启动失败。`ENABLED=false` 只允许单独出现，避免看似关闭却
遗留半套敏感配置。

生产环境变量：

```text
AZURE_SPEECH_ENABLED=true
AZURE_SPEECH_REGION=eastasia
AZURE_SPEECH_KEY=...
AZURE_SPEECH_CONNECT_TIMEOUT_MS=3000
AZURE_SPEECH_REQUEST_TIMEOUT_MS=15000
AZURE_SPEECH_MAX_RESPONSE_BYTES=5242880
```

timeout/size 有安全默认值，但显式值仍严格校验。普通测试不读取真实 key；可选真实 smoke 必须是
`#[ignore]` 且只读取专用环境变量。

## 8. PR 3 接口

PR 3 负责 voice repository/API、试听 request DTO、缓存/并发控制和对象存储。其流程应为：先把
API 的 `RichTextV2` 走现有 canonicalizer，再从 voice 目录构造 `Voice`，调用本模块构造请求和
fingerprint，查询/写入缓存，最后调用注入的 `Arc<dyn SpeechProvider>`。前端始终只提交 RichText
和稳定 voice alias/options，永远不提交 SSML、provider voice id、hash 或对象键。
