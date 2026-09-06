# 首次创建确认误报匹配变化

## 前置与复现

- 后端 worktree `tsz-rust-dev-worktree`，HEAD `4a9a3ecf`；前端只读 HEAD `a030f75`。
- 原后端任务明确释放 writer 后接手；保留全部未提交标注功能。
- 临时 PostgreSQL16 容器 `tsz-surface-confirm-bugfix-20260906`，127.0.0.1:55433，master `surface_confirm_bugfix`；Redis DB2。
- 每条 Cargo 命令显式 DATABASE_URL 指向上述临时实例，不读取 .env 旧业务库连接。
- 新增 SQLx 测试 `surface_confirm_bugfix_first_explicit_confirmation_http`：创建已有 harbour fixture，启动随机 TCP 端口真实 Axum 服务；HTTP detect → 使用检测 token、未变显式统一主词 → POST entries。
- 预期：第一次 409 annotation_conflict；填标注后一次 POST 为201。
- 修复前确定性失败：实际409 surface_matches_changed；原始日志 `/tmp/surface-confirm-bugfix-before.log`。与用户 center 首击红错是同一绑定路径。
- 既有标注测试辅助函数会替换匹配 token 重试，新回归首击不调用该辅助函数。

## 根因与修复

检测 token 的 owner/content digest 是 v3_detection；显式最终创建采用 v3_create_final_headwords，因此没有实际变化也绑定不匹配。
仅最终主词精确等于缓存检测建议时，尝试以最终完整匹配和上下文重新构建 detection binding 验证。匹配集合或标注上下文变化仍摘要不等，回到最终创建重新确认；actor、command、检测ID、TTL、policy 校验保留。
未改公开请求响应契约、前端、迁移或原有重试辅助函数。

## 验证与交付

新增3个定向回归通过：首次确认与一次保存、主词/旧标注修订/新增匹配/策略变化、词典额外词形 harbor 引入未展示命中。真实 HTTP 与额外风险回归在 tests/lexicon_handler.rs 的 surface_confirm_bugfix_*。
独立只读 reviewer 已检查运行时绑定、材料排序与策略映射，无阻断发现。
未暂存、提交、推送、开PR、合并或部署；未触碰现场 center 组。最终 UI 由主协调任务验收。

- fmt、全目标全features Clippy -D warnings通过。
- lib 301通过/1已有忽略，bins 7通过，doc 0用例通过；修复版构建成功。
- OpenAPI SHA256仍为 `c4fe3bcf0f5a594282385900ab6d41eb6ed0705ced03f262ed6cb05e095c2369`。

## 日志完整性

- `/tmp/surface-confirm-bugfix-before.log` SHA256 `8d97ca547b3a71bc033a1c8fb6b9171bddbfa1852719146692f18725defdb9c7`
- `/tmp/surface-confirm-bugfix-after.log` SHA256 `d1e040ac5cfa4ac4d035760c4225a35a4f61fb4171d4a0dbd55b8413d5c4f8ae`
- `/tmp/surface-confirm-bugfix-fmt.log` SHA256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`
- `/tmp/surface-confirm-bugfix-clippy.log` SHA256 `57aa3b8f54de0b2527b9aa7de957f2461e45246ccd70a854eca4330cceda216d`
- `/tmp/surface-confirm-bugfix-unit.log` SHA256 `632f46e7d58275e189a76ba1b91096da22754a11805560073f6d1e4fdde11306`
- `/tmp/surface-confirm-bugfix-doc.log` SHA256 `3152b17c71039071e8e0ebcabf2002ae04648667919a97c8fed29c0aedf3f168`
- `/tmp/surface-confirm-bugfix-build.log` SHA256 `a41051563a4cb5fc1b1bcdd9aa0ba28ca80f7736d629d90cc138218756e8e3a9`
