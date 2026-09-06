# 词条标注后端交接与验证

更新时间：2026-09-06T11:35:24.618658+00:00。基线/HEAD均为 `4a9a3ecf2a9eec6bc2ad2cb4b3985f8c817d4028`。
工作树：`/Users/darwish/Dev/tsz-core/tsz-rust-dev-worktree`；分支 `codex/dev-worktree-20260906`。
未暂存、提交、推送、开PR、合并或部署。

## 实现与契约

- V3创建同事务保存旧标注和新词条，失败无部分保存，成功请求可幂等重放。
- 按最终原型识别组，保留V2 headword/V3 base、草稿可见性、当前发布版本和空骨架防重。
- 独立annotation_revision，不改变内容revision、投影或待发布状态。
- 管理V2/V3列表、详情、匹配context返回标注；PATCH独立编辑，冲突使用Problem Details。
- 修改旧条会检查其其他有效原型，duplicate不一定发生在当前弹窗内。
- OpenAPI SHA256：`c4fe3bcf0f5a594282385900ab6d41eb6ed0705ced03f262ed6cb05e095c2369`。共享契约见 [design.md](design.md)。

## 验证

- 全目标cargo check、最终cargo fmt、全目标all-features Clippy -D warnings通过。
- 显式临时库lib/bin：301个库测试通过、1个已有忽略；7个二进制测试通过。
- doc测试通过（0个doc test）。
- 词库模块全部228个集成测试均取得通过证据，**包含定向复跑，不是单次全绿**：
  content_completion 4、dictionary 3、lexicon_handler 148、其他9模块73。
- 首轮12个失败来自旧重名fixture缺标注，已显式opt-in适配，未改底层call或吞普通409。
- 后续147项handler中144通过，两个嵌套旧fixture和一次SQLx SSLRequest握手波动失败；
  修复后3项单线程定向复跑通过。新增不同key并发/普通变体测试也通过。
- 四个entry_annotations专项覆盖第二/第三条、历史null、多原型去重、直接组关系、
  trim/大小写/20 Unicode scalar、旧修订/token漂移、晚期INSERT失败回滚、同key并发、
  不同key创建和旧条编辑交错、V2读取、普通变体不强制、旧条其他原型和当前发布原型。
- 独立只读审查的2个P1已修复并复核；无剩余P1/P2。当前发布回归用隔离SQL fixture
  删除draft source，证明查询边界，不声称走了完整save_forms发布切换流程。

## 服务与数据台账

- 核对旧PID79461来自指定工作树，只停止该8583进程后启动新构建PID2726。
- 服务显式使用localhost:5433的 `tsz_dev_worktree_20260906`、Redis localhost:6379 DB2、
  PORT8583、COOKIE_SECURE=false。不依赖.env中的数据库连接设置。
- /readyz返回ready；迁移20260906180000 success=true。启动时entries=0；此后center组
  由前端唯一写入，后端未触碰。
- 初始专项测试在授权master新建唯一entry_annotations子库；最终核对无相关管理记录。
- 全量门用临时容器 `tsz-entry-annotations-gate-20260906`，PostgreSQL16，localhost:55433，
  master `entry_annotations_gate`。无活跃连接/遗留记录后已停止，--rm清理临时实例和数据。
- Redis2未执行广泛清理以保护前端会话；检测/快照沿用原TTL，独立测试策略前缀可能保留。
  未声称所有Redis测试key均已删除。
- 两次lib/bin漏设DATABASE_URL的疏漏和证据边界见
  [test-environment-incident.md](test-environment-incident.md)。原库未作补偿/清理写入。
- 8583业务二进制包含标注实现。启动后仅调整OpenAPI 409媒体类型声明和测试/文档；
  真实HTTP错误媒体类型一直正确。保存项目目录、8383/8483未修改。

## 交付边界

迁移down会删除标注列，回退前需备份。旧程序严格反序列化可能不接受新增context字段，
应用回退前应处理短期检测/快照缓存。目前是未提交工作树，未执行外部发布。
真实浏览器验收由前端任务负责，本记录后端证据不替代UI验收。

## 原始日志

- `/tmp/entry_annotations_unit_gate.log` — SHA256 `c1120449aa0a86e1ac26c90124762a1f8515a002d5419e41930f13b0450d74bc`
- `/tmp/entry_annotations_unit_gate_final.log` — SHA256 `16c5d5f64c12b65b55f5d3c27c707e1ad6b7fcf7540c17359c26282d073deb4c`
- `/tmp/entry_annotations_unit_gate_isolated.log` — SHA256 `45e727452124b5868630df3a1d7e18368a2053e43c40ae94191d8d156cac9273`
- `/tmp/entry_annotations_lexicon_gate.log` — SHA256 `6192cdf75cd082e96ac846b98b2c46f8b8868f3b1d49bcf8f41066fb34ee2933`
- `/tmp/entry_annotations_lexicon_gate_final.log` — SHA256 `a10d0e2493e56c0ca9c05bc1c2dcf5841eb079d2ddb039f30a210441988a8386`
- `/tmp/entry_annotations_remaining_tests.log` — SHA256 `95a0e64d53e26953935b426e99d0610ce14624b42d8e5ddca3df7ef8a90b3efd`
- `/tmp/entry_annotations_race_test.log` — SHA256 `f9368310200b4fe1d8a054bea34e3fd77296e0537fb0f944f7627584e9852b43`
- `/tmp/entry_annotations_remaining_gate.log` — SHA256 `b21dab6d82ea49a76372f4b1322e110389481dd0485db973a63a12667dbb8e5c`
- `/tmp/entry_annotations_clippy_final.log` — SHA256 `58f7149ec4935a01a0cf5f603ceabecbdb6d21fc393909866fe21e970042790d`
- `/tmp/entry_annotations_doc.log` — SHA256 `5bd897f808d72ea8c0b9be532b82327158c52f9ea2efad7c22e6f3296c0559f2`

最终漂移核对：HEAD未变化，git diff --check通过。23个改动/新增代码、测试及迁移文件按
路径排序、以 path + NUL + 文件内容 + NUL 聚合的SHA256为
`84be379de0e21f061d19df4cec13c7ef20554ba1b2cd8819b6ada2dd49bb6fcb`。
最终再次核对PID2726 cwd和8583监听一致，readyz仍ready，临时测试容器已不存在。

## 列表单条隐藏标注补充验证（2026-09-06）

已批准新增V2/V3列表必填annotation_visible，annotation/annotation_revision/PATCH不变。后端以列表IDs单次批量SELECT判断同原型的另一可见有效entry，不依赖分页/筛选，不加写锁或迁移。

- 3项annotation_visibility_*通过：自身多source不算重复、V2/V3一致、任意原型直接重复、分页/筛选外peer、归档隐藏/恢复显示/硬删除隐藏、隐藏标注仍可编辑且原数据不清、draft actor/currentpub、普通plural命中不显示。
- 4项entry_annotations原专项通过；lib301通过/1已有忽略、bins7通过；全目标全features check、Clippy -D warnings及格式通过。新增最后一条负向测试后仅复跑3项visibility，运行时代码与独立已审版本一致。
- 独立只读review无P0–P2；NOT MATERIALIZED允许条件下推，现有entry/lookup索引可用。没有实测EXPLAIN或大组压测，不声称具体查询耗时；单次JOIN+DISTINCT极大同名组中间行量是性能边界。
- 权威OpenAPI SHA256：f8352c91a0165506a80034273645a339e0e0cbb25ec1ee7e77d95aa0a24c172c。前端已显式同步，报告11文件275测试、3包typecheck、admin/API lint通过。
- 原8583 PID37278核对cwd及监听后仅重启为PID49628，readyz200。显式localhost5433/tsz_dev_worktree_20260906、Redis DB2；现场3条center未操作。主任务负责最终浏览器验收。
- 本轮独立PG16容器tsz-annotation-visibility-20260906，localhost55433/master annotation_visibility；全部cargo显式隔离URL。活跃测试连接0后已停止，--rm清理测试子库/数据。Redis不广泛清理，快照沿TTL。
- ship保持暂停，未stage/commit/push/PR/rebase/整合dev或部署，原保存目录与前端由本任务保持只读。

日志完整性：
- `/tmp/annotation-visibility-check.log` SHA256 `5cf661f9d46751e162004e6c2788dc6d986cbe7685c4af1e8d13f751095fdec1`
- `/tmp/annotation-visibility-tests-final.log` SHA256 `648dee8311fb47d6f54e4b0422251cd9c1a5f012c890317e28554e104da06390`
- `/tmp/annotation-visibility-openapi.log` SHA256 `24e280bb0a8ea5992e93549ab0e6102c89789c6e4056511876707f6ccf8db723`
- `/tmp/annotation-visibility-clippy.log` SHA256 `2bab22d201d48d241fe854cef3707ac458274525c6463152428b4d01dfdef7a8`
- `/tmp/annotation-visibility-unit.log` SHA256 `11f2457385de6750e5657c1bc30cb0fe5117149dced35fa9620d8657c094d924`
- `/tmp/annotation-visibility-annotations.log` SHA256 `c3ec76774be3cfc16d10eb9efabdec808423e37e3726ad54023d66aff91de955`
- `/tmp/annotation-visibility-build.log` SHA256 `6ee867f41747b2810b88ee16f24783b7ce71881232220f7b05c6bf568bc0a25e`

当前代码/测试/迁移聚合（path+NUL+内容+NUL）SHA256：`686596a6dc4e03ca38f2277643cda7193ac7dd622d5c985ec7d9764656f0ca40`。
