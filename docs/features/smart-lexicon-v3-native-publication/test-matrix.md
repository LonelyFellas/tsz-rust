# Smart Lexicon V3 原生发布测试用例矩阵

| # | 层 | 场景 | 输入 / 前置 | 预期 | 优先级 |
| --- | --- | --- | --- | --- | --- |
| B1 | 后端契约 | native capability wire | `origin=native` 的 V3 entry | `publication.mode=native`；严格反序列化与 OpenAPI discriminator 接受 | P0 |
| B2 | 后端契约 | 未知 capability mode | `mode=future_mode` | strict schema 拒绝，不能回退到 native/canary | P0 |
| B3 | 后端服务 | 合法 native 资格 | native 且 migration provenance 全空、canary=false | 发布和历史激活进入既有 schema 3 事务 | P0 |
| B4 | 后端服务 | native 混合 migration 状态 | native 但 batch/source/canary 任一不符合不变量 | fail closed，无 publication/current pointer/surface/outbox 写入 | P0 |
| B5 | 后端服务 | migrated canary 资格 | verified batch/entry、source publication 一致、canary=true | 保持现有发布成功行为 | P0 |
| B6 | 后端服务 | migrated 未授权 | canary=false、未 verified 或 source 不一致 | 409 migration-canary 错误，无写入 | P0 |
| B7 | HTTP + DB | 原生 V3 首次发布 | 完成 forms/meanings 的 native draft | 201；schema 3 immutable publication、nodes/catalog refs/sense refs、current surface、pointer、audit/outbox 同事务写入 | P0 |
| B8 | HTTP + DB | 原生发布幂等 | 同 key+同 body 重放；同 key+异 body | 前者复用响应且不增行；后者稳定冲突 | P0 |
| B9 | HTTP + DB | 同 revision 与新 revision | 同 revision 再发；编辑后再发 | 前者复用 publication；后者 publication number +1 | P0 |
| B10 | HTTP + DB | surface / visibility 确认 | 匹配 surface、过期 token、policy/revision 漂移 | 按现有 409/410 流程重新确认；失败无半写入 | P0 |
| B11 | HTTP + DB | 历史激活 | native V3 有两条 publication，执行 B→A→B | current pointer/surface 同步切换，lifecycle revision 单调增加 | P0 |
| B12 | 消费者集成 | native current publication | 由真实 native publish 生成，不手工 seed publication | list/stats/history/related-search/关系与例句关联能读取 schema 3 | P0 |
| B13 | 生命周期集成 | published native V3 archive/restore | 已发布 native V3 | 既有引用、surface、确认 token 和原子性规则保持 | P0 |
| B14 | 运行门禁 | publish/projection/read 开关 | 分别关闭运行开关 | 写入或读取 fail closed；V2 行为不受影响 | P0 |
| B15 | 数据纯度 | native 发布不写 V2 bridge | 发布前 V2 count=0 | 发布后 V2 count 不增加、无 `entry_headwords`、响应无 `compatibility` | P0 |
| F1 | 前端类型/契约 | types/runtime guard 接受 native | backend OpenAPI 新分支 | TS union 和 runtime schema 接受 native、拒绝未知 mode | P0 |
| F2 | admin 组件 | native 发布入口 | 完整、未归档 native word | 发布按钮可用并调用既有 publish API | P0 |
| F3 | admin 组件 | blocked capability | shadow、未白名单 canary、archived | 发布按钮不可用并显示对应阻塞原因 | P0 |
| F4 | admin 组件 | native 历史激活 | published native word + 非 current history | 激活按钮可用；current/archived/未授权状态不可用 | P0 |
| F5 | admin 集成 | surface confirmation | native 首次发布返回 confirmation conflict | 展示确认页；确认后复用同一命令上下文成功发布 | P0 |
| F6 | api-client 契约 | OpenAPI 对账 | 从 tsz-rust 权威 spec 同步 | snapshot/runtime schema 无手改、契约测试通过 | P0 |
| E1 | 真实环境 | `serendipity` 首次发布 | tshb-test revision 6 原生草稿，V2=0 | UI 发布成功；API/DB 确认 current schema 3 publication | 手测 |
| E2 | 真实环境 | 发布后消费者烟测 | E1 成功 | 列表、历史、搜索、编辑入口均可用，服务 health/ready 正常 | 手测 |
| E3 | 真实环境 | 第二版与历史激活 | 修改草稿并发布第二版 | 两条 immutable history 可切换，surface 与 pointer 一致 | 手测 |

## 手测清单

- [ ] 从 `serendipity` 核对页发布；若出现 surface confirmation，确认后成功。
- [ ] 返回词条列表并重新进入，状态为 published，revision/published_revision 正确。
- [ ] 发布历史存在 schema 3 快照，预览中的词形、发音、释义、例句不变。
- [ ] API 与数据库确认 `current_publication_id`、schema 3 snapshot、surface、outbox、audit 一致。
- [ ] 确认 schema 2 entry 总数仍为 0，原生 entry 无 legacy headword/compatibility。
- [ ] 发布第二版并激活第一版，再激活第二版，确认 current surface 随之切换。
- [ ] `/healthz`、`/readyz` 和 admin API 反代保持正常。
