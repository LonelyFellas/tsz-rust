# Task #61：教学标注不再切断朗读

## 目标与基线
- 后端基线：84cbd92e7cfcfd8f831fbf8f1d252b56e7daf54f。
- 前端验收基线：04ed6d3a27673050672755e92e02ead01a6aaed3，无前端代码修改。
- Core(job) + Function(s) 原先输出两个 emphasis 标签，真实 Azure 返回 job、s 两个词边界。
- 修复要求：仅改变教学分类，不得增加 SSML 边界；保留显式音标、停顿和词性提示符静音。

## 实现
- `src/speech/ssml.rs`：仅 Phoneme 进入开闭标签范围；Core、Function、历史 Strong 不生成标签。
- 历史 Strong 在 DTO 中定义为核心词兼容值，按 Core 处理，不恢复旧的语音重读语义。
- 不删教学标注、不改正文、不插入或裁剪空白、不重分词。
- Grammar 继续静音，保留原空白，并过滤静音区内的停顿与全静音音标。
- 保留 IPA/UPS、显式 Pause、语速、音高、风格、XML 转义和 NothingToSpeak。
- 音标在 SynthesisRequest 中已校验互不重叠，移除仅服务于 emphasis 嵌套的排序。
- `src/speech/model.rs`：SSML_BUILDER_VERSION 升为 rich-text-v2-ssml-v3。
- `src/speech/tests.rs`：教学标注与纯文本 SSML 等价、显式音标/停顿保留及缓存版本回归。

## 缓存、兼容与非目标
- fingerprint 已包含生成器版本，新试听不命中旧版 preview_cache；无需清库。
- 不改 normalized_content 哈希规则，视觉差异仍可占用不同缓存项；缓存去重优化不属本次范围。
- 前端每次试听都调用后端；已有播放中的旧 URL 或下载文件不会自动改写。
- 无 DTO/OpenAPI/数据库迁移，无需配套前端发布。后端回退会恢复旧行为与旧缓存键。
- 混合版本后端期间仍可能请求到旧实例，发布验收需固定实际运行版本。
- 不保证零停顿：自然语流、标点、显式音标与用户 Pause 仍可能影响朗读。
- 保留前后端既有 crossing_speech_marks 校验，不顺便开放“整词音标跨多种颜色范围”。

## 验证记录
- 红：旧实现运行 `speech::tests::teaching_marks_do_not_split_words_or_change_spoken_text` 失败。
- 绿：`SQLX_OFFLINE=true cargo test --locked --lib speech::tests`，18 项全部通过。
- 库测试首次运行：282 通过、14 因 DATABASE_URL 缺失失败；为本任务建立隔离 PG 后，重跑包含这 14 项的模块全部通过。
- 原有 speech preview 锁单测硬编码 localhost:6379；首次库测试包含这些单测，后续未重复，不声称全库测试均使用隔离 Redis。
- `cargo fmt --all -- --check` 通过。
- `SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。
- LSP 未运行成功：当前 1.98.0 工具链缺 rust-analyzer；可安装该组件或调整 pi-lsp.json。
- 修复后原 SSML 生成器直连真实 Azure：英美音 × 单词/短语 × 无标注/整体/拆分，共 12 次成功。
- 修复后三种教学标注形式生成同组相同 SSML；12 次 WordBoundary 均以完整 jobs 结束。
- WordBoundary 是服务返回的分词事件，不是人工音素听辨。

## 真实后台验收
- 独立后端：127.0.0.1:8461，来自本 worktree 修复后构建，复制独立 binary 避免共享 target 被覆盖。
- 独立前端 Vite：127.0.0.1:3161，代理 /api/v1 到上述后端；词库、词性与 TTS mock 显式关闭。
- 独立容器 tsz-task61-pg（55461）/ tsz-task61-redis（56461），仅绑定 loopback。
- 对象存储沿用本地已授权 speech 配置，使用新增 task61-continuous-speech 子目录隔离测试对象。
- 创建本任务管理员与草稿 01a0bf58-2699-7c43-993a-a6bf79e18543；未发布词条。
- 浏览器真实登录 → 创建 job 草稿 → 语法结构 jobs → 连续标注 job/core、s/function。
- Sonia / Aria 各试听两次：POST /api/v1/admin/speech/previews 均 200，分别 generated、hit。
- 四次下载音频均 200，Content-Type audio/mpeg，各 22464 bytes；页面显示“试听完毕（缓存）”。
- 保存 forms、meanings 返回 200；刷新独立 GET 仍为 jobs + core(0..3)/function(3..4)，字母样式恢复正确。
- 上述验收阶段没有修改前端代码，也没有推送或服务端部署。

## 本地证据与继续验收
- 旧/新合成对照：`/tmp/tsz-task61-investigation/`、其 `fixed/` 子目录。
- 修复后试听页：`http://127.0.0.1:3261/fixed/`（当前本地诊断服务）。
- 脱敏浏览器请求：`/tmp/task61-runtime/browser-preview.json`、`save.json`、`reloaded.json`；同目录截图与 MP3。
- 运行时私密配置与浏览器会话仅放本地受限目录，不进入仓库；PID 文件供核实进程身份后停止。
- 本地服务、隔离数据库和两份测试对象保留供继续试听；清理只针对本任务资源，不删共享数据或存储目录。

## 可执行回归
```sh
SQLX_OFFLINE=true cargo test --locked --lib speech::tests
cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
```
- 数据库模块测试需先配置本任务隔离 DATABASE_URL；不要直接使用未知 .env 启动迁移。
- 真实验收按上述后台路径操作；新增账号、草稿与音频只在隔离环境创建。
