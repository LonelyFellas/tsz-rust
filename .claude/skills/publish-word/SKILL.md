---
name: publish-word
description: 用 ops/lexicon-publish/publish_words.py 直接经 admin API 创建并发布 V3 词条，替代在管理后台一步步点击。用户说「发布单词」「加个词」「批量造词条」「往词库里塞几个词」「publish word」，或给出一批 单词+释义 要求入库时使用。
---

# 发布词条（不走后台页面）

脚本 `ops/lexicon-publish/publish_words.py` 一次跑完
`detect → create → forms(complete) → meanings(complete) → validate → publish`，
详细字段说明在同目录 `README.md`。本技能只负责：把用户口述的词变成正确的脚本调用，并如实回报结果。

## 红线

- **默认只打本地** `http://127.0.0.1:8383`。用户没有明确说「测试环境 / tshb-test / 47.121.142.19」时，
  绝不加 `--base-url`。要打远程，先复述一遍「这会往 tshb-test 写真实词条」并拿到确认。
- 凭据只从环境变量 `TSZ_ADMIN_PHONE` / `TSZ_ADMIN_PASSWORD` 读。不把密码写进仓库文件、
  不写进 JSON、不打印到回复里。用户没配就让他自己配，别替他猜。
- 发布是写操作且**没有去重**：同一个词跑两次会得到两条独立词条。批量前先确认词表没重复、
  也没在库里发过；用户说「重发一遍」才重发。
- 不为了让脚本跑通去改后端代码或放宽校验。校验拦下来的内容问题，回去改词条描述。

## 1. 备好输入

- 简单词（一个词性、一个义项）直接用位置参数，一次可以给多个：

  ```bash
  ops/lexicon-publish/publish_words.py harbour:港口 apple:苹果 nurse:护士
  ```

- 需要多词性、多义项、英美拼写差异、真实音标、复数/过去式等派生词形时，把词条描述写成 JSON，
  放到 scratchpad（不要污染仓库），再 `--file` 传进去。字段见 README，样例见
  `ops/lexicon-publish/example-words.json`。
- 用户只报了单词没给释义时，先问释义，别自己编。词性、CEFR 等级、词频这些可以按默认值走
  （`noun` / `A1` / `100`），但要在回报里说明用了默认值。
- 细分词性 `sub_pos` 必须属于对应基本词性（`noun` → `N-COUNT` 等）。不确定就交给脚本取默认值，
  或先跑一次读报错里列出的可用值。

## 2. 跑脚本

```bash
./publish_words.py <参数>
```

跑之前确认目标后端活着（`curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8383/healthz`），
不活着就告诉用户先起服务，别自作主张启动或改配置。

## 3. 回报

- 逐条列出成功的词和它的 entry id，写清楚哪些字段用了默认值（音标占位、A1、词频 100 等）。
- 有失败就把后端返回的 `code` 和字段问题原样带出来，并给出下一步：
  - `401` —— 验证码一次性，60 秒冷却、每天 10 次上限；等一分钟重试，或用 `--token`。
  - `unknown_part_of_speech` / 细分词性不匹配 —— 词性目录里没有，换一个或先去后台配。
  - `surface_matches_changed` / `surface_match_acknowledgement_required` —— 脚本已自动确认重放，
    仍失败说明库里同词面情况复杂，去后台人工确认。
  - 校验类 `field_issues` —— 是词条内容缺项，改描述重跑。
- 部分成功时明确说清哪些进去了、哪些没有；已发布的不会自动回滚。
