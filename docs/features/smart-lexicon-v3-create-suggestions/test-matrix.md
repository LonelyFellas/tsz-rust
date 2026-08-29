# Smart Lexicon V3 创建建议测试矩阵

## 目标

修复 `create_v3` 无条件创建空 `forms` 的回归，并把合法同表面已有词条的词性与内置词典建议合并。已有词条只贡献词性代码，不复制任何内容或节点身份。

## 用例矩阵

| # | 层级 | 场景 | 前置/输入 | 预期 | 自动化落点 | 优先级 |
| --- | --- | --- | --- | --- | --- | --- |
| B1 | DTO/契约 | 顶层合并建议必填 | V3 detect 响应 | 必填 `suggested_pos`；builtin evidence 字段与 provenance 不变；未知/缺字段按严格 wire 处理 | dto/openapi tests | P0 |
| B2 | service | builtin matched、无 surface match | 词典返回多个 POS/base suggestions | 顶层建议等于 builtin 建议、稳定去重；create 物化每个 POS 的 group/base/membership | V3 service/handler tests | P0 |
| B3 | service | builtin matched、重复确认后创建 | surface matches 非空且 token 已确认 | 确认只解除创建门禁，不丢失 builtin suggested forms | handler integration | P0 |
| B4 | service | existing-only 词性 | builtin 不含该 POS；合法 draft/current-publication match 含 POS | 顶层建议追加 POS；create 为该 POS 创建空 base 结构与默认空 pronunciation | surface + create integration | P0 |
| B5 | service | builtin/existing 重叠去重 | 两来源均含相同 POS，且 draft/publication 重复投影 | 顶层与物化 POS 只出现一次；builtin 顺序优先，existing-only 按稳定顺序追加 | service tests | P0 |
| B6 | service | 生命周期过滤 | 同表面包含 published、draft、archived entries | published/draft 贡献 POS；archived 不贡献；无效/无 POS source 不产生建议 | surface tests | P0 |
| B7 | service | not_found 空白路径 | 无 builtin、无合法 existing POS | `suggested_pos=[]`，create 保持空 forms；不得伪造默认词性 | service/handler tests | P0 |
| B8 | identity | 不复制旧内容/身份 | existing entry 含 forms/pronunciations/meanings/examples/relations | 新词条仅复用 POS code；拼写/音标/实际发音为空；所有 POS/group/form/variant/membership/pronunciation UUID 为新值 | DB/API assertions | P0 |
| B9 | persistence | create 后原生 V3 read-back | 非空建议完成 create | editor projection、`entry_pos`、`v3_*` 关系表和 GET 响应一致且非空 | handler + SQL assertions | P0 |
| B10 | catalog | 建议 POS 目录不一致 | 建议包含当前 catalog 不存在的 code | create fail closed 为 catalog mismatch，不创建半成品 entry | service/handler tests | P0 |
| B11 | idempotency | 相同 key 重放 | 首次 create 已成功物化建议 | 返回完全相同 entry/UUID；不同 body 仍冲突 | existing idempotency tests extended | P0 |
| B12 | 真实验收 | 重复创建 `center` | 已有已发布 `center`，内置词典 matched，确认 surface warning | detect 顶层建议含 builtin + existing POS；create/GET/DB 均有预填结构，旧词条内容未复制 | 本地 HTTP + PostgreSQL | 必验收 |

## 实施约束

- `builtin_dictionary.suggested_pos/suggested_forms/coverage/provenance` 只表达内置词典事实。
- 顶层 `suggested_pos` 是服务端合并后的权威建议集合。
- surface 建议只取非归档的合法 draft/current-publication entry；同 entry/同 POS 去重。
- existing-only POS 创建空 base；不复制旧 surface、发音、释义、例句、关系、publication 或 UUID。
- OpenAPI 只通过仓库权威生成流程更新。
