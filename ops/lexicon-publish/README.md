# 词条发布脚本

`publish_words.py` 用 admin API 走完一条原生 V3 词条的完整链路，替代在管理后台一步步点：

```
detect → create → forms(impact + complete) → meanings(complete) → validate → publish
```

纯标准库 Python 3.9+，无第三方依赖。

## 前置条件

- 目标后端在跑，且 `SMART_LEXICON_V3_CREATE/EDIT/PUBLISH/PROJECTION` 都是 `true`。
- 有一个 active 的管理员账号（本地可用 `cargo run --bin seed` 造）。
- 登录需要短信验证码；本地与 tshb-test 的 `OtpSender` 都是 Mock，固定码 `000000`。

```bash
export TSZ_ADMIN_PHONE=13900009527
export TSZ_ADMIN_PASSWORD='your-password'
```

## 用法

最小模板，一个词一条 `单词:中文释义`，可以一次给多个：

```bash
./publish_words.py harbour:港口 apple:苹果 nurse:护士
```

完整描述走 JSON（对象、数组，或 `{"words": [...]}`）：

```bash
./publish_words.py --file example-words.json
```

打远程环境必须显式给 `--base-url`（默认只打本地 `http://127.0.0.1:8383`）：

```bash
./publish_words.py harbour:港口 --base-url http://47.121.142.19:8383
```

常用开关：`--pos`（默认基本词性，默认 `noun`）、`--sub-pos`、`--level`（默认 `A1`）、
`--frequency`、`--grammar`、`--token`、`--no-cache`。

## JSON 字段

顶层：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `surface` | 是 | 词面，用于 detect 和默认拼写 |
| `kind` | 否 | `word`（默认）或 `phrase` |
| `gloss` | 二选一 | 只给中文释义时，脚本按默认词性生成一个义项 |
| `pos` | 二选一 | 词性数组，见下 |

`pos` 元素：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `pos` | 否 | 基本词性 code，必须在后端词性目录里（默认 `noun`） |
| `spelling` | 否 | `"harbour"` / `{"common": ...}` / `{"uk": ..., "us": ...}`，默认取 `surface` |
| `pronunciation` | 否 | 同上形状的音标；不给就按拼写兜一个 `/拼写/` 占位 |
| `extra_forms` | 否 | 派生词形：`{"form_type": "plural", "spelling": ..., "pronunciation": ...}` |
| `senses` | 二选一 | 义项数组，元素可以是字符串（当作 `gloss`）或对象 |
| `gloss` | 二选一 | 没有 `senses` 时的单义项简写 |

`senses` 元素：`gloss`（必填）、`sub_pos`（默认取该词性目录里的第一个）、`level`（默认 `A1`）、
`frequency`（默认 `100`）、`grammar`（语法结构文本）、`group`（语义区间，写 `"引申义"` 则中英同名，
写 `{"zh": "引申义", "en": "figurative"}` 可分开给；默认「核心义 / core」）。

同一个词性里只要有一处区分英美，整个词性会升成 `uk_us` 形状，其余词形自动把 common 拼写复制到两侧——
这是后端 `dialect_rules` 的硬约束（`distinguish` 拼写必须配 `distinguish` 音标）。

## 会话缓存

后端每个手机号一天只发 10 次验证码，两次之间还有 60 秒冷却。脚本因此把会话缓存在
`~/.cache/tsz-lexicon-publish/<base-url>.json`（权限 0600）：

1. 缓存里的 access token 没过期就直接用；
2. 过期了先拿 refresh token 续（不消耗验证码）；
3. 都不行才用手机号+密码+验证码登录。

`--no-cache` 关掉缓存，`--token` 直接传现成 access token。碰到 401 多半是验证码在冷却期内
被重复消费，等一分钟再跑即可。

## 已知边界

- V3 词条没有跨词条唯一键，同一个词跑两次会得到两条独立词条，脚本不做去重。
- 库里同一词面的候选**超过一页（20 条）**时，写操作要求的确认令牌只在末页签发，脚本会自动翻
  `GET /surface-match-snapshots/{id}?cursor=…` 到末页再重放。翻页在 2026-09-02 之前会 503
  （Redis Lua 回写快照时把空数组编成了空对象），后端已修；跑在旧版本上会看到这个 503。
- 音标占位（`/拼写/`）只是为了满足发布校验，不是真实音标；要准的就在 JSON 里写 `pronunciation`。
- 例句、词条关联（同/反义词等）不在脚本范围内，需要的话仍在后台补。
