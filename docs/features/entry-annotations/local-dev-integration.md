# 本地dev整合记录

用户明确授权仅本地提交/整合/合回dev，不执行ship、push、PR、部署或业务数据操作。

## 基线与保全

- 原仓库 `/Users/darwish/Dev/tsz-core/tsz-rust`：干净dev，`66ee4293ba53308eae510d830f985aef19dcb033`。
- 功能worktree原HEAD：`4a9a3ecf2a9eec6bc2ad2cb4b3985f8c817d4028`，分支 `codex/dev-worktree-20260906`。
- 先以正常原生SQLx/Clippy pre-commit保存已验收全部工作，保护提交 `9af8e979b6ece8faf427159a4eea3872eceb2732`，再合入本地dev。
- 原仓库AGENTS与.githooks已核对；没有reset --hard、清理用户改动或绕过hooks。

## 整合取舍

- dev已包含 `7f93bec` 标注功能/首次检测确认修复，以及 `ffabefb` 关系预绑定下线；复用已合入能力。
- 后续净增量只含可见空草稿续编提示、列表annotation_visible与回归/验收记录；不重复加入标注存储迁移。
- publishing、v3_publication、config、dictionary和全部迁移与整合前dev一致；删除旧prebinding测试，保留dev的显式关系绑定测试。
- tests/lexicon_handler.rs相对dev只追加5项后续测试与一个列表辅助函数；既有token回归保留。
- 合并文档保留dev补充的持久化发布快照回退风险，追加历史UI实证及后续列表规则。原库测试管理记录疏漏审计和原日志哈希完整保留，无连接凭据。
- 重新导出OpenAPI，相对dev仅增加V2/V3列表annotation_visible及detect existing_draft_id。SHA256 `c3f6a18ac3dba0dfb790cdf68c29d8638e83cfc037a582d1d8273d366c55006f`，已交前端显式同步。

## 环境与范围

所有Cargo、hooks、lib/bin/doc及集成测试显式使用独立PG16实例 `tsz-local-dev-integration-20260906`、localhost55433/master `local_dev_integration`，Redis DB2。未依赖.env旧业务库。临时库先应用原工作树迁移，再应用dev的20260906140000预绑定下线迁移；不对业务库执行这些迁移。

现有8583 PID49628未重启，继续运行先前验收版本；此次代码整合验证使用隔离服务/数据库，不声称现有本地常驻服务已切到新dev基线。批量恢复3/4是另外事项，本次只记录、不扩展修复。

## 整合后验证

- lib301通过、1项已有忽略；bin7通过；doc通过（0用例）。
- 单轮143个lexicon_handler、1个surface_snapshot、22个v3_migration、11个v3_storage_schema全部通过，共177个集成测试，无定向补跑拼接。
- 测试包含dev显式关系绑定/拒绝旧预绑定形态、原标注事务与token回归、空草稿续编/权限/归档竞态，以及标注显示/分页/恢复删除/普通变体边界。
- .sqlx缓存与dev无净差异；原生提交hook继续执行SQLx prepare和全目标Clippy。
- 代码整合净差异为后续功能，迁移与dev逐字一致；没有执行远程操作、部署或现场数据修改。

日志完整性：
- `/tmp/local-dev-integration-preserve-commit.log` SHA256 `730d03d16b79897cbf33327a3de7cf0a010207f0065b1c1c26a1542f59866268`
- `/tmp/local-dev-integration-migrate-base.log` SHA256 `8c54ed01d54b9ec7873d64b183c89f6768b28f078acc2341d3a120a457341fed`
- `/tmp/local-dev-integration-migrate-merged.log` SHA256 `4c8ada3b6ea126a86ec9e0ff8e30b197bbd9a671f23d3dfbac00e1daccf07ac8`
- `/tmp/local-dev-integration-openapi.log` SHA256 `ac9dcf887dfe0011cdb0f0294828c0c7b05aff375a15b200f4e6e6e5b13e4368`
- `/tmp/local-dev-integration-unit.log` SHA256 `eeca45adb70a585e4eefbd01d1fd3c01fcbf0e6739f1ede42e6bebae5ec56182`
- `/tmp/local-dev-integration-doc.log` SHA256 `c20e85677f6dba6912d9506da32bd416ac328253ff30fe6694e2279130b64baa`
- `/tmp/local-dev-integration-regression.log` SHA256 `9c65375d779c84b7c876004060242d048e0871d002778d6f4c85ad1ba778f358`
