# TTS 试听 API 与缓存编排设计

## 1. 范围

本 PR 增加管理员 voice 目录与同步试听 API：

- `GET /api/v1/admin/speech/voices`；
- `POST /api/v1/admin/speech/previews`；
- `speech.voices` 与 `speech.preview_cache` 的 additive、可回退 migration；
- PostgreSQL 缓存索引、Redis 单 key 锁、`speech` ObjectStore 与短期签名 URL 编排。

不包含前端、正式发布音频、`synthesis_jobs`/`audio_assets` Worker、CDN、头像、生产 Azure 配置或部署。

## 2. 权威契约与决策

PR 2 的 `Voice`、`SpeechOptions`、`SynthesisRequest`、`SynthesisFingerprint` 与
`SpeechProvider` 是合成边界。API 只接受 canonicalizable `RichTextV2`、稳定 voice alias、
`style`、`rate_percent` 与 `pitch_semitones`，DTO 使用 `deny_unknown_fields`。客户端不能提交
SSML、provider voice id、fingerprint、object key、音频、URL 或输出格式。

`docs/word-data-model.md` 的总体表设计与当前边界有两点收敛：

1. `preview_cache` 不保存 SSML。SSML 是可由 canonical input 确定性重建的敏感 provider 请求正文，
   不应进入数据库或日志；
2. 本 PR 不提前建立 `synthesis_jobs`/`audio_assets`。同步试听由 cache row 表达生命周期，正式任务与
   发布资产仍由后续 Worker PR 一次性建立完整约束。

voice 目录由数据库维护，migration 仅建立 schema/table，不硬编码生产 voice seed；目录内容的真相源是
`ops/speech-voice-catalog/seed.sql`（幂等，重建库或恢复备份后重跑）。空目录是合法状态；
未配置 Azure 或 `speech` storage 不影响进程启动与 voice 查询，试听在运行时返回稳定 503。

## 3. 数据模型与回退

`speech.voices`：UUID v7 主键、唯一 alias、provider/provider voice id/locale/gender、styles JSONB、
rate/pitch 上下限、provider version、enabled 与时间戳。约束限制 token、范围、JSON 形状；API 只暴露
alias、locale、gender 和能力，不暴露 provider 字段。

`speech.preview_cache`：32-byte `request_hash` 主键、voice FK、object key、content hash、mime、size、
创建与过期时间。object key 唯一；不保存 URL、SSML 或 provider 响应。过期 row 可被同 fingerprint
原子替换。down 先删 cache，再删 voices/schema；旧二进制从不引用这些表，因此 up 后兼容旧版本，
down 前必须停止新二进制，且只丢弃可再生试听缓存。

## 4. API 契约

voice 列表要求 active admin，返回 `{"items":[...]}`，只列 enabled voice，稳定按 alias 排序。

试听请求：

```json
{
  "content": {"schema_version": 2, "paragraphs": []},
  "voice_alias": "en-us-female-1",
  "style": null,
  "rate_percent": 0,
  "pitch_semitones": 0
}
```

成功统一返回 200：`cache_status` 为 `hit` 或 `generated`，另含 `audio_url`、`expires_at`、
`url_expires_in_seconds`。URL TTL 来自 `speech` storage policy；cache row 保留 `CACHE_TTL_HOURS`
（当前 24 小时，上限受 OSS 生命周期规则约束，见 `ops/speech-preview-lifecycle/README.md`），URL
不写数据库。相同 canonical fingerprint 的非 owner 请求不会调用 provider：短轮询数据库，若 owner
仍生成则返回 409 `speech_preview_in_progress`，调用方可重试。

稳定错误：非法 RichText/options 为 400；未知/禁用 voice 为 404；provider 未配置、storage 未配置、
Redis/存储故障、provider 鉴权/不可用/超时为 503；provider 限流为 429；同 key 生成中为 409。
鉴权沿用 active admin（401/403）。所有错误为 RFC 9457 Problem Details。

试听以 canonical fingerprint 天然幂等，不接受 `Idempotency-Key`，也不创建正式 synthesis job。
voice 列表是只读操作；试听是高频、无业务真相变更的派生读取，因此不写 `audit.admin_actions`，
仅记录 request id、稳定错误分类、cache hit/generated/in-progress 与耗时，且不记录正文、SSML、音频或 URL。

## 5. 缓存、锁与补偿

流程为：校验 admin → 查询 voice → canonical request/fingerprint → 查未过期 PG cache → Redis
`SET key token NX PX` → 锁后双检 → provider → ObjectStore put → PG upsert → presign → token 校验
Lua unlock。

- 拿到锁之后的生成过程 detach 到独立 tokio 任务，调用方 future 被丢弃也照常跑完。前端在
  「生成中改文本 / 换发音人 / 关页面」时会 abort，若生成就地跑，axum 丢弃 handler future 会在
  put 与 save_cache 之间留下无 DB 行的孤儿对象，并且 `lock.release()` 永不执行——同一
  fingerprint 要等租约到期（provider timeout + 30s）才能再试。detach 后已付费的合成结果落缓存，
  锁也一定释放；代价是客户端消失后仍会跑完这一次生成，这是有意接受的；
- 锁 TTL 必须覆盖 provider request timeout 与存储/DB 尾延迟；只有 token owner 能释放；
- 锁丢失不影响正确性：PG 主键是最终 winner，竞争方若写入失败会删除自己生成的对象；
- put 后 DB 失败会 best-effort delete；delete 失败只记录稳定分类与 object key，不记录内容/URL；
- DB 成功后 presign 失败保留有效 cache，下次请求可再次签名；
- 过期 row 的旧对象在成功替换后 best-effort 删除；补偿删除失败、以及进程在生成中途被杀
  （detach 的任务不在 axum 优雅停机的等待范围内）形成的孤儿，由 bucket 生命周期规则按对象年龄
  兜底，应用不做按年龄的批量对象删除（`ops/speech-preview-lifecycle/README.md`）；
- 过期 row 本身由进程内定时任务每小时清理，只删 row 不碰对象，因此没有跨系统删除顺序问题；
- 该任务删掉过期 row 之后，同 fingerprint 的再次请求读不到 stale key，旧对象不再被及时删除，
  同样落到生命周期规则回收。因此清理任务上线后，「替换时删旧对象」只覆盖过期未满一轮的窗口，
  其余旧对象一律走规则——这是刻意的取舍：对象的删除职责集中在规则一侧，不与应用重复；
- provider 可重试错误不在单个 HTTP 请求内自动重试，避免放大流量；释放锁后由客户端重试。

## 6. 测试矩阵

| ID | 层 | 场景 | 预期 | 优先级 |
|---|---|---|---|---|
| V1 | repository/API | enabled voice 排序与能力 | 不泄露 provider id，disabled 不返回 | P0 |
| V2 | API | 未认证/禁用 admin | 401/403 Problem Details | P0 |
| R1 | model/API | canonicalizable RichTextV2 | 合成 canonical 内容 | P0 |
| R2 | API | 非法/越界 RichText、SSML/未知字段 | 400 或 422，provider 不调用 | P0 |
| O1 | model/API | style/rate/pitch 边界 | allowlist 与范围严格执行 | P0 |
| C1 | service | hash cache hit | 不调用 provider/put，只重新 presign | P0 |
| C2 | service | miss | provider、put、row、presign 各一次 | P0 |
| C3 | Redis integration | 并发同 key | 单 owner；其余命中或 in-progress | P0 |
| C4 | Redis integration | 锁超时/错误/错误 token unlock | 稳定失败且不误删他人锁 | P0 |
| P1 | service | provider 400/401/429/5xx/timeout | 映射 400/503/429，不缓存失败 | P0 |
| S1 | service | put/presign/delete/DB 失败 | 按边界补偿，不返回敏感信息 | P0 |
| H1 | API | cache hit/generated/in-progress | 200 hit/generated 或 409 稳定 code | P0 |
| M1 | migration | up/down、约束、历史兼容 | schema 可升级回退 | P0 |
| A1 | OpenAPI | 路径、DTO、Problem Details | snapshot 与实现一致 | P0 |

普通测试使用 fake provider 与 memory/fake store；Redis 锁集成测试使用本地 Redis，数据库契约测试
使用本地 PostgreSQL，不读取真实 Azure key。

## 7. 实施计划

1. 增加 migration、voice/cache repository 与 schema tests；
2. 增加严格 DTO、service、Redis lock、错误映射和 fake 单测；
3. 接入 admin router、AppState 与 OpenAPI，生成权威 snapshot；
4. 运行 focused tests，再运行 fmt/check/clippy/full test、SQLx prepare、migration/schema/OpenAPI 门禁；
5. 汇报审查与风险，停在 `ship` skill 的用户确认门，不自行提交推送。
