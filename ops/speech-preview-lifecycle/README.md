# 试听音频的生命周期规则

试听缓存产生两类垃圾，由两套机制分头回收，**职责不重叠**：

| 垃圾 | 谁回收 |
|---|---|
| 过期的 `speech.preview_cache` 行 | 应用进程内的定时任务（`src/speech/preview/cleanup.rs`） |
| OSS 里的对象（过期的、以及没有 DB 行的孤儿） | bucket 生命周期规则（本文件） |

应用**不做**按年龄的批量对象删除。`ObjectStore` 没有 `list`（见
`docs/object-storage-design.md` §4，且明确禁止为业务接口新增这类高风险能力），
因此扫表方案结构性地发现不了孤儿——一个在 `storage.put` 之后、`save_cache` 之前中断的生成
会留下任何 DB 行都不引用的对象。只有按对象年龄工作的生命周期规则能覆盖它。

（客户端 abort 曾是这类中断的主要来源，现已消除：生成过程 detach 到独立任务，调用方消失也会
跑完。剩下的来源是补偿删除失败，以及进程在生成中途被杀——detach 的任务不在优雅停机的等待
范围内。两者都是低频且不可预防的，正是生命周期规则存在的意义。）

应用侧仅保留既有的**补偿删除**：针对刚写入失败、或刚被同 `request_hash` 替换掉的
单个 key。它与生命周期规则不冲突——时间域不重叠（补偿在创建后数秒，规则在数十天后），
且两边的 delete 都是幂等的，撞上也只是一次 no-op。

注意行清理任务对补偿删除的影响：`generate_locked` 是从数据库读 stale key 才知道该删哪个旧对象，
而行清理每小时跑一次，所以过期超过一轮之后的再次请求读不到 stale key，旧对象不再被及时删除，
一并落到规则回收。这是刻意的：对象的删除职责集中在规则一侧，应用不与它重复。
其后果是 bucket 里的稳态垃圾比「每次替换都及时删」要多一些，按当前量级（约 30 KB/条、3 条/天）
可忽略。

## 当前配置

| 项 | 值 |
|---|---|
| Bucket | `tshb-test-assets`（华南2 / 河源） |
| `OBJECT_STORAGE_SPEECH_OSS_ROOT` | `/speech` |
| 规则前缀 | `speech/previews/` |
| 当前版本文件 | 最后一次**修改时间** 30 天后 → 删除 |
| 清理对象删除标记 | 启用 |
| 历史版本文件 | 最后一次修改时间 1 天后 → 删除 |
| 文件碎片 | 生成时间早于 7 天 → 删除 |
| 版本控制 | **已开启** |

规则的真相源是控制台。下面这份 `PutBucketLifecycle` XML 只是同一份意图的文字记录，
**照抄前先读这一段**：`<Days>` 与 `<ExpiredObjectDeleteMarker>` 互斥，不能写在同一个
`<Expiration>` 里，也不能在同一个 `<Rule>` 里放两个 `<Expiration>`，所以走 API 复现时
必须拆成两条同前缀的规则（OSS 允许前缀相同或重叠的规则）。具体接受形式以 OSS 官方文档为准。

```xml
<LifecycleConfiguration>
  <Rule>
    <ID>speech-previews</ID>
    <Prefix>speech/previews/</Prefix>
    <Status>Enabled</Status>
    <Expiration><Days>30</Days></Expiration>
    <NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays></NoncurrentVersionExpiration>
    <AbortMultipartUpload><Days>7</Days></AbortMultipartUpload>
  </Rule>
  <Rule>
    <ID>speech-previews-delete-marker</ID>
    <Prefix>speech/previews/</Prefix>
    <Status>Enabled</Status>
    <Expiration><ExpiredObjectDeleteMarker>true</ExpiredObjectDeleteMarker></Expiration>
  </Rule>
</LifecycleConfiguration>
```

控制台里这两件事在同一条规则的两个开关上（「当前版本」的天数 + 「清理对象删除标记」），
不需要建两条；只有走 API 才需要拆。

**版本控制开着，所以历史版本那条不是可选项**：`DeleteObject`（生命周期的、以及应用补偿删除的）
只会生成删除标记，对象变成历史版本继续计费，必须靠 `NoncurrentVersionExpiration` 才真正释放空间。
1 天足够——试听对象的 key 是 UUIDv7，永不原地覆盖，历史版本只可能由删除动作本身产生。

## 必须维持的不变量

```
lifecycle_days × 24h  >  CACHE_TTL_HOURS + presign_ttl + 时钟偏差 + 规则调度延迟
```

URL 只在 row 存活时签发，签出后再活 `presign_ttl`。所以对象可能被访问的最晚时刻是
「创建 + TTL + presign_ttl」，规则必须晚于它，否则会签出指向已删对象的 URL。

`src/speech/preview/mod.rs` 里的 `const _: () = assert!(...)` 把这条不等式编译期锁住了：
`LIFECYCLE_EXPIRE_DAYS` 是本文件规则天数的镜像，TTL 调过界直接编译失败。
当前 30 天规则下 `CACHE_TTL_HOURS` 最多可到 528 小时（22 天）。

**改 TTL 的顺序**：

- 放大 TTL → **先改控制台规则**，再改代码常量；
- 缩小 TTL → **先改代码常量**，再改控制台规则。

任何中间时刻不等式都必须成立。

## 禁止项

**不得实现「命中即续期」的滑动过期。** 生命周期规则按对象的 `LastModified` 计龄，
而当前 row 的 `expires_at` 恒等于对象创建时间 + TTL（`save_cache` 的 upsert 换的是新 key + 新 row，
从不原地续期）。滑动过期会打破这个恒等式，让规则删掉正在被引用的热对象。
这在「为了提高命中率而拉长 TTL」的语境下格外危险——顺手加个 LRU 续期是很自然的念头。

## 新环境上线检查项

代码里的编译期断言只能保证 TTL 数值不越界，**保证不了规则真的存在**。
新环境漏配就是把「对象永久堆积」这个 bug 原样复刻一遍，所以它必须躺在这张单子里：

- [ ] 新建独立 bucket（环境边界按 bucket 划，不靠前缀区分）
- [ ] `.env` 里 `OBJECT_STORAGE_SPEECH_OSS_ROOT=/speech` —— **必须与本文件一致**，
      否则规则前缀就得按环境维护对照表，而对照表就是下一次写错前缀的温床
- [ ] 按「当前配置」建生命周期规则，前缀 `speech/previews/`
- [ ] 确认版本控制状态；开启则必须带历史版本与删除标记两项
- [ ] RAM 用户只给该 bucket 的**对象读写**权限，不给 bucket 管理权限
      （配规则用主账号在控制台做，服务器那把 AK 不需要任何批量删除能力）

## 踩过的坑

- 前缀写成 `/speech/previews/`（带前导斜杠）或掉字母，规则匹配 0 个对象且**不报任何错**，
  状态一直显示「启用中」。建规则后务必回列表逐字核对生效范围。
- 前缀写少一层（如 `speech/`），将来同一 space 下的正式发布音频会被一起删掉，不可恢复。
- 判断依据选「距最后访问时间」需要额外开访问跟踪，且热对象永不过期，行为不可预测。必须用修改时间。
- 到期动作选成「转为低频/归档」不但不删，还因最小存储时长计费更贵。
- 规则创建后 24 小时内加载、48 小时内生效，之后每天定时执行。不要期待立刻看到效果。
