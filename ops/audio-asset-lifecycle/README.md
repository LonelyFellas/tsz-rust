# 上传音频资产的空间配置与生命周期规则

管理员直传的真人录音走 `audio` 空间。本文件是该空间的部署事实来源：环境变量、bucket 生命周期
规则、CORS，以及新环境上线检查项。

## 空间配置

`audio` 空间未配置时三个端点一律返回 `501 audio_storage_not_configured`，前端据此把音频面板
置灰——这是刻意的降级路径，不配置不会让服务启动失败。要开通就得把下面这组变量**一次配全**，
缺任何一项都会在启动期整体失败（`docs/object-storage-design.md` §5）。

```text
OBJECT_STORAGE_SPACES=speech,audio
OBJECT_STORAGE_AUDIO_BACKEND=oss
OBJECT_STORAGE_AUDIO_OSS_ENDPOINT=https://oss-cn-shenzhen.aliyuncs.com
OBJECT_STORAGE_AUDIO_OSS_REGION=cn-shenzhen
OBJECT_STORAGE_AUDIO_OSS_BUCKET=tshb-test-assets
OBJECT_STORAGE_AUDIO_OSS_ROOT=/audio
OBJECT_STORAGE_AUDIO_OSS_ACCESS_KEY_ID=...
OBJECT_STORAGE_AUDIO_OSS_ACCESS_KEY_SECRET=...
OBJECT_STORAGE_AUDIO_PRIVACY=private
OBJECT_STORAGE_AUDIO_MAX_OBJECT_SIZE_BYTES=10485760
OBJECT_STORAGE_AUDIO_PRESIGN_TTL_SECONDS=300
OBJECT_STORAGE_AUDIO_CACHE_CONTROL=private, max-age=86400
```

`OBJECT_STORAGE_AUDIO_OSS_ROOT` **必须是 `/audio`**：下面的规则前缀是按这个 root 写死的，
按环境改 root 就得维护一张前缀对照表，而对照表就是下一次写错前缀的温床（speech 空间同款约定）。
root 与 `/speech` 相邻不重叠，启动期的 root 重叠检查会挡住写反的情况。

## 两个前缀，两种命运

| 对象 | 前缀 | 谁回收 |
|---|---|---|
| 已签发许可、但从未 confirm 的上传 | `audio/uploads/` | bucket 生命周期规则（本文件） |
| 已 confirm 的资产 | `audio/assets/` | 本期不回收（引用关系与回收随后续 PR 落地） |

confirm 成功时服务端把对象 `copy` 到 `assets/` 再删掉 `uploads/` 里的那份。**这次搬运不是多余的**：
`ObjectStore` 没有 `list`（`docs/object-storage-design.md` §4 明确禁止为业务接口新增这类能力），
所以「对象存在但没有数据库行」这种孤儿只能靠按对象年龄工作的生命周期规则发现，而规则只认前缀。
不搬前缀，就只能在同一个前缀里既放孤儿又放正式资产，规则一开就会连正式资产一起删。

搬运失败（copy 报错）时资产不落库，客户端拿到 5xx 可重试。注意底座的 `copy` 是「read 源 + put 目标」，
**OSS 已提交 PUT 但响应丢失同样会报错**，那时目标对象其实已经写成了——所以这条路径也会尝试删除目标对象
（`delete` 是幂等的，没写成只是一次 no-op），删不掉时按日志里的 `object_key` 人工清理。
落库失败同样删掉刚复制出的正式对象，暂存那份留给规则。删暂存对象失败只记 `warn`，
由规则兜底——删除职责不与规则重复。

搬运与落库跑在 detach 出去的任务里，客户端中途关页面不会把它打断在「对象已搬、行没落」的中间态。

## 生命周期规则

真相源是控制台，下面这份 `PutBucketLifecycle` XML 只是同一份意图的文字记录。
`<Days>` 与 `<ExpiredObjectDeleteMarker>` 互斥，走 API 时必须拆成两条同前缀的规则
（控制台里是同一条规则的两个开关）。

```xml
<LifecycleConfiguration>
  <Rule>
    <ID>audio-uploads</ID>
    <Prefix>audio/uploads/</Prefix>
    <Status>Enabled</Status>
    <Expiration><Days>7</Days></Expiration>
    <NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays></NoncurrentVersionExpiration>
    <AbortMultipartUpload><Days>7</Days></AbortMultipartUpload>
  </Rule>
  <Rule>
    <ID>audio-uploads-delete-marker</ID>
    <Prefix>audio/uploads/</Prefix>
    <Status>Enabled</Status>
    <Expiration><ExpiredObjectDeleteMarker>true</ExpiredObjectDeleteMarker></Expiration>
  </Rule>
</LifecycleConfiguration>
```

7 天远大于一次上传的生命周期（许可 5 分钟过期，confirm 紧随其后），留的是「管理员选完文件去开会、
回来再 confirm」这类长尾的余量。**规则前缀绝不能写成 `audio/`**：那会把 `audio/assets/` 下
的正式资产一起删掉，且不可恢复。

## CORS

直传是浏览器直接 PUT 到 OSS 域名，bucket 必须放行 admin 来源，否则前端只会看到一个没有细节的
网络错误：

- 来源：`http://localhost:3001`、测试服 admin、生产 admin
- 方法：`PUT`
- 允许 header：`Content-Type`、`Cache-Control`

**`Cache-Control` 必须放行，漏了就是功能不可用。** 预签名把 `Content-Type`、`Content-Length`
和空间固定的 `Cache-Control` 一并纳入 V4 签名，客户端必须原样回发；而 `Cache-Control` 不在 CORS
安全列表里，浏览器会为它发预检。只放行 `Content-Type` 的话，预检直接被拒；反过来为了绕开预检
而不发这个头，OSS 就回 `SignatureDoesNotMatch`——两条路都走不通，而前端看到的都只是一个没有
细节的网络错误。（`Content-Length` 与 `Host` 是浏览器禁止脚本设置的头，由浏览器自己补，
不需要也不能在 CORS 里列。）

规则创建后生效有延迟，提前配。

## 新环境上线检查项

- [ ] `.env` 配全上面 12 个变量，`OBJECT_STORAGE_SPACES` 里带上 `audio`
- [ ] `OBJECT_STORAGE_AUDIO_OSS_ROOT=/audio`，与本文件一致
- [ ] 建生命周期规则，前缀逐字核对为 `audio/uploads/`（建完回列表看生效范围，写错不报错）
- [ ] 确认版本控制状态；开启则必须带历史版本与删除标记两项
- [ ] 配置 CORS 并确认 OPTIONS 预检通过——**预检请求头里带上 `cache-control` 再验一次**，
      只用 `content-type` 验会漏掉真正会拒的那个头
- [ ] RAM 用户只给该 bucket 的对象读写权限，不给 bucket 管理权限
