# 发音人目录种子

`seed.sql` 是 `speech.voices` 的可重复执行种子，把试听发音人目录固化进版本控制。
在此之前目录是纯手工数据：重建库、恢复到旧备份或换环境都会静默丢空，
表现为前端「获取语音」按钮全禁用（`<locale> 暂无可用发音人`）。

## 执行

```bash
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f ops/speech-voice-catalog/seed.sql
```

`ON_ERROR_STOP=1` 不能省：默认 psql 报错后照样以退出码 0 结束，这个步骤就当不了门禁。

先看当前状态再执行：

```bash
psql "$DATABASE_URL" -c "SELECT alias, locale, gender, enabled, provider_version FROM speech.voices ORDER BY alias;"
```

## 幂等语义

- 按 `alias` 冲突合并；重跑不会重复建行，也不会换掉已有行的 `id`。
- 身份与能力（`provider`、`provider_voice_id`、`locale`、`gender`、`styles`、`provider_version`）
  以本文件为准，漂移时收敛回来；完全一致时整条 `UPDATE` 不触发，`updated_at` 保持不动。
- **不写 `enabled`**：运维停用某个发音人后，重跑种子不会把它悄悄启用回去。
- **不写 rate/pitch 上下限**：沿用建表默认（-50/100、-12/12）。手工调过的行不会被种子覆盖。
- 文件里不删任何行。下线发音人是运维动作（`UPDATE ... SET enabled = false`），不走种子。

## 何时重跑

- 新建库、恢复备份、换测试/预发环境之后；
- 目录改动（增删发音人、Azure 更新了某个发音人的 styles）之后；
- 怀疑目录漂移时——重跑是安全的，一致就什么都不写。

`tests/speech_voice_catalog_seed.rs` 在 CI 里对空库跑两遍本文件，锁死上面的幂等语义。

## 改目录前必须做的两件事

### 1. `styles` 逐个发音人向 Azure 核实，不能照抄

同一个 locale 下不同发音人的 style 列表差别很大
（`en-US-AriaNeural` 16 个、`en-GB-SoniaNeural` 只有 `cheerful`/`sad`）。
目录是 style allowlist 的唯一来源：写进去的 style 前端会当可选项、后端会放行，
而 Azure 对该发音人不支持的 style **不报错，直接忽略**（实测 HTTP 200，照常返回音频）。
所以抄错 styles 不会炸在任何一层，只会表现为「用户选了某个风格但听不出区别」——
静默错误，比报错难发现得多。逐个发音人核实，别照抄同 locale 的另一个发音人。

```bash
curl -sS -H "Ocp-Apim-Subscription-Key: $AZURE_SPEECH_KEY" \
  "https://$AZURE_SPEECH_REGION.tts.speech.microsoft.com/cognitiveservices/voices/list" |
  python3 -c 'import json,sys
for v in json.load(sys.stdin):
    if v["Locale"].startswith("en-"):
        print(v["ShortName"], v["Gender"], v.get("Status"), sorted(v.get("StyleList") or []))'
```

顺手确认 `Status` 不是 `Deprecated`，并把核实日期写进该行的 `provider_version`
（`azure-voices-list-YYYY-MM-DD`）。⚠️ `provider_version` 参与试听缓存键
（`src/speech/preview/service.rs` 的 `versioned_hash`）：只在这一行的目录事实确实
重新核对过时才动它，无谓地改会白白作废该发音人的全部试听缓存。

### 2. 同一 locale 加第二个发音人前，先想清楚谁会被自动选中

`GET /api/v1/admin/speech/voices` 的契约是「只列 enabled，稳定按 alias 排序」
（`src/speech/preview/repository.rs` 的 `list_voices`），前端在这个列表里
**按 locale 取第一个匹配项**，目录里没有任何显式的偏好字段。
也就是说：**当前实际用哪个发音人由 alias 字母序决定。**

现在每个 locale 各一个自动选中项，字母序恰好落在预期上（`en-gb-sonia`、`en-us-aria`，都是女声）。
往同一个 locale 里加发音人会改变这个结果——例如加 `en-gb-ryan`（男声）后
`r < s`，英式会自动翻成男声，与美式的女声对不齐。

真要支持一个 locale 多发音人，正确做法是给 `speech.voices` 加显式的
`sort_order` / `is_default`，让 `list_voices` 按它排序，而不是靠 alias 拼字母
——那会同时改动上面那条已写进 `docs/tts-preview-api-design.md` 的排序契约，
属于独立改动，别在加数据时顺手做。
