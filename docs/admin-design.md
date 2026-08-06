# admin 域设计(后台身份系统)

> 状态:已定稿(Q1–Q12 拍板见 §18,2026-07-19),一期实现中。
> 参照:tsz-go `internal/{admin,authz}` + `docs/admin-{account-management,auth-hardening,rbac,user-management}-design.md`(业务规则事实来源,**不是代码结构模板**);前端契约 `tsz/packages/api-client/src/admin.ts` + `packages/types/src/{admin,admin-role,admin-user}.ts`(wire 形状事实来源)。
> tsz-go 侧的编号决策(D 系列)在本文引用时标注出处,如「hardening-D8」。

## 0. 范围与非目标

**范围**(分三期,见 §17):

- 一期:`admins`/`admin_refresh_tokens` 表 + admin auth 五件套(login/refresh/logout/logout-all/change-password)+ `GET /admin/profile` + 账号锁定 + must_change_password 守卫。
- 二期:super_admin 管理 admins(创建/启禁用/重置密码)+ seed 机制。
- 三期:admin 管理 web 用户(4 端点)。(原三期的 RBAC 已整体取消,见 Q10。)

**非目标**(继承 go 侧已拍板的负向决策,重写时同样视为已定,勿返工):

- admin 自助找回密码(忘密码线下找超管;超管忘密码走 seed/DBA 带外)。
- 经 API 创建/启禁用/重置 super_admin(仅 seed 可造,治理顶点互不可管,account-D1/D2)。
- 管理员改级(promote/demote)、admins 侧昵称编辑。
- web 用户删除、level 字段、coin_balance 填充、手机/邮箱换绑(user-mgmt-D1/D3/D4)。
- 禁用 web 用户/admin 即时踢线(接受一个 access TTL 延迟,user-mgmt-D6)。
- **RBAC 整体**(角色/权限委派/roles 与 permissions 端点/派角色,Q10 产品定案):只有 admin/super_admin 两级身份,全体 admin 全功能;「管理员管理」按 `role == super_admin` 门禁。go 侧已落地的 RBAC 不移植。
- **email 字段**(Q9 产品定案):admin 只有手机号,无邮箱——连联系资料都不留,登录/找回/展示全线无 email。
- admin words 管理 10 端点(word 域,另立文档)。
- 审计表、per-IP 限流、失败登录告警(→ §15 偏离清单,生产上线前重估)。

## 1. 关键决策速览

| # | 决策 | 一句话理由 |
|---|------|-----------|
| 1 | admin 与 web 用户**完全隔离的身份存储**:独立表、独立登录、独立签名密钥,同手机号互不相通 | go 侧铁律;后台泄露不波及 C 端,反之亦然 |
| 2 | 第二把 JWT 密钥 `ADMIN_JWT_SECRET`(必填),`TokenManager::new(secret, Realm::Admin, ttl)` 第二实例 | 双防线 = per-realm secret + aud 校验;`Realm::Admin` 枚举早已就位 |
| 3 | 两级 `super_admin`/`admin`,列/枚举/wire 统一命名 **`role`**(单一身份,非 web 的多角色;RBAC 已取消无撞车) | super 是治理顶点:仅 seed 可造、不可被启禁用/重置;与 JWT claim 名天然一致 |
| 4 | refresh cookie:`admin_refresh_token`,`Path=/api/v1/admin`,SameSite=**Strict** | 名字+Path 与 web cookie 双向隔离;后台无跨站跳转需求,比 web 的 Lax 更严 |
| 5 | 会话策略:**严格单登录**——issue 前 `revoke_all`(Q1 已定) | go 侧语义;后台账号不该多处同时在线 |
| 6 | 轮换/重放语义**复刻 tsz-rust web 侧**(CAS 原子轮换 + 20s 宽限不铸币 + 窗口外连坐),不照搬 go 的「宽限内铸新币」 | 同一套已验证的模式;前端 refresh 已 single-flight,不需要铸币宽限 |
| 7 | 账号锁定:连续失败 5 次锁 15 分钟,锁定态 **423**(区别于 401),成功清零、自动解锁(hardening-D8) | 挡分布式低频撞库;423 的轻微枚举 oracle 已知且接受(admin 账号极少) |
| 8 | must_change_password = DB 列 + **逐请求查库的守卫**(非 token claim),白名单仅 change-password/logout/logout-all(hardening-D6) | 重置后即时生效,零 TTL 滞后 |
| 9 | 临时密码:后端生成 20 位(charset 去 `0O1lI`)、明文仅随响应返回一次、不落库不进日志(hardening-D2/D3) | 创建与重置共用同一内核;charset 无 `1` ⇒ 必不含手机号 |
| 10 | 密码策略:≥12 字符、≤72 字节、非纯数字、弱子串黑名单、不含本人手机号(hardening-D7) | 后台密码强度必做;违规统一 400 |
| 11 | **无 RBAC**(Q10):全体 admin 全功能;`profile.permissions` 恒返全量菜单 key(死数据,保前端菜单渲染零改动) | 产品定案只有两级身份;权限委派体系整体取消 |
| 12 | 错误不可区分与 web 侧同纪律:账号不存在≡密码错(401);refresh 无效态全笼统(401);错密码先于 disabled 检查 | 防探测;安全语义与 web 域一以贯之 |
| 13 | repository 仍是**具体 struct + `#[sqlx::test]`**,admin 会话仓库平行复刻(不抽 trait 泛化两张表) | 项目铁律;5 个方法的重复远便宜于泛型 SQL 的扭曲(query! 宏要静态 SQL) |
| 14 | **登录 2FA**:仅手机号标识,`{phone, password, code}` 三参数必填(Q7);发码端点恒 202 反枚举;码错≡密码错逐字节一致 | 后台账号是高价值目标,双因子缺一不可;go 侧无此设计,tsz-rust 主动升级 |
| 15 | **绝对会话上限 7 天**(Q8):refresh TTL=7 天且**轮换继承旧枚 expires_at 不重算**——登录后第 7 天必重走 2FA | web 滑动续期(活跃不掉线)适合 C 端;后台会话必须定期强制重认证 |

## 2. 身份模型:为什么是独立存储

go 迁移注释原文:admins 是「与 users 完全独立的后台身份库」——同一手机号可以既是学员又是管理员,两个身份毫无关联,各自唯一约束、各自登录、各自签名密钥。**不是** users 表加一个 `admin` 角色:

- 爆炸半径隔离:C 端任何漏洞(如 OTP 逻辑)不可能变成后台提权入口;
- 生命周期不同:web 用户自助注册、多角色可切换;admin 由超管 provision、单一 role、强制改密;
- 会话策略不同:web 多设备,admin 严格单登录。

`role` 语义(account-D1/D2;Q10 后无权限委派,两级即全部):

| | 怎么产生 | 可被启禁用? | 可被重置密码? | 权限 |
|---|---|---|---|---|
| `super_admin` | 仅 seed(带外) | ✗ 一律 403 | ✗ 一律 403(含自己) | 全功能 + 管理员管理 |
| `admin` | 超管经 API 创建 | ✓ | ✓(⇒ 强制改密+踢会话) | 全功能(除管理员管理) |

「一律 403」是毯式规则,好处是「最后一个活跃 super 被禁」状态**根本不可达**,无需 last-super-admin 保护护栏(account-D4:go 曾做过又删掉,我们直接不做)。

## 3. schema(迁移草案)

tsz-rust schema 从零重画的红利:go 分多次迁移补的列(must_change_password、lockout)我们**一步到位**。共两张表(RBAC 三表已随 Q10 取消):

```sql
-- 一期迁移 create_admins:
-- 后台身份库,与 users 完全独立(见 docs/admin-design.md §2)。无 email(Q9,仅手机号)。
CREATE TABLE admins (
    id                   UUID        PRIMARY KEY,          -- Rust 侧 Uuid::now_v7()
    phone                TEXT        NOT NULL,
    password_hash        TEXT        NOT NULL,
    display_name         TEXT        NOT NULL,
    role                 TEXT        NOT NULL DEFAULT 'admin'
                         CHECK (role IN ('admin', 'super_admin')),
    -- 状态:active 正常,disabled 已被禁用(超管管理动作,403)——区别于 locked_until 的自动锁定(423)。
    status               TEXT        NOT NULL DEFAULT 'active'
                         CHECK (status IN ('active', 'disabled')),
    -- 强制改密标志(hardening-D6):默认 true 是 fail-secure(漏赋值的路径最多逼人改密);
    -- seed 建超管必须显式写 false;change-password 与写 hash 同条 UPDATE 原子清除。
    must_change_password BOOLEAN     NOT NULL DEFAULT true,
    -- 账号锁定(hardening-D8):连续失败计数 + 锁定截止。过去的 locked_until 不是锁(自动解锁,无 cron)。
    failed_login_count   INT         NOT NULL DEFAULT 0,
    locked_until         TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX admins_phone_unique ON admins (phone);

-- 一期迁移 create_admin_refresh_tokens:形状 = refresh_tokens 平行复刻,仅 FK 指向不同。
CREATE TABLE admin_refresh_tokens (
    id          UUID        PRIMARY KEY,
    admin_id    UUID        NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    token_hash  TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,
    rotated_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX admin_refresh_tokens_admin ON admin_refresh_tokens (admin_id);
CREATE UNIQUE INDEX admin_refresh_tokens_hash ON admin_refresh_tokens (token_hash);
```

(原三期 RBAC 三表迁移草案已随 Q10 取消删除。)

## 4. JWT:第二把密钥

- config 新增**必填** `admin_jwt_secret`(⚠️ 部署面:上这版前生产 `.env` 必须先加此项,否则重启即失败——与 `redis_url` 同为「缺则启动失败」纪律,deployment.md/.env.example 同步)。
- `AppState` 加 `admin_token_manager: Arc<TokenManager>`,构造 `TokenManager::new(&cfg.admin_jwt_secret, Realm::Admin, Duration::minutes(admin_access_ttl))`。**零新代码**——`Realm::Admin` 的 aud 校验、防跨 realm、防算法混淆全在既有 `TokenManager` 单测覆盖内。
- claims:`sub` = admin id;`role` claim 承载 `"admin"`/`"super_admin"`——与 admins.role 列、wire 字段同名同值(Q11 统一命名),与 web realm 的 `role`=当前激活角色平行,同一个字段名、不同 realm 各自语义。
- 提取器:新写 `AdminAuth { subject: Uuid, role: AdminRole }`(`admin/extract.rs`),逻辑同 `AuthUser` 但走 `state.admin_token_manager`。**已知取舍**(go 同):role 从 token 读,15 分钟内可陈旧;must_change 逐请求查库补真,disabled 由 refresh 拒绝在一个 TTL 内兜底。

## 5. 会话:admin_refresh_tokens

**机制复刻 tsz-rust web 侧,不是复刻 go**——web 侧这套(CAS `consume_and_insert` 原子轮换、`RotatedRefresh` 无 Debug、peek→校验→签名→rotate 压轴、20s 宽限不铸币、窗口外 `revoke_all` 连坐、验尸 `revoked_at.is_none()` 防 DoS)是三轮评审+225 条测试钉出来的,admin 侧原样平移,只换表:

- `AdminRefreshTokenRepository`(具体 struct,方法集 = web 版:`insert/find_by_hash/consume_and_insert/revoke_by_hash/revoke_all_by_admin_id`),SQL 打 `admin_refresh_tokens`。**不抽 trait 泛化**:`query!` 宏要静态 SQL,表名进不了参数;5 个方法的复制是最地道的解。
- `AdminSessionService`:同 web `SessionService`(哈希私有、`peek_admin_id`、宽限窗口 20s 硬编码镜像测试常量)。
- **与 go 的刻意偏离**:go 宽限窗口(60s)内的诚实重放会**铸一枚新 token**(N 个 tab 竞速留 N 枚活 token);我们沿 web 侧语义——窗口内只回 401 不铸币、不连坐。代价:丢响应后重试会要求重登。admin 前端已做 single-flight 去重 + StrictMode 单飞,常规竞速根本到不了后端,此代价可接受。宽限秒数也随 web 用 20s(go 60s)。(Q2 已定)
- **严格单登录**(Q1 已定):`issue` 前先 `revoke_all_by_admin_id`——go 语义「Issue 先 RevokeAll 再发新」。web 侧多设备,admin 侧后台账号异地同时在线本身就是异常。
- cookie:名 `admin_refresh_token`,`HttpOnly; SameSite=Strict; Path=/api/v1/admin`(= `ADMIN_MOUNT` 常量,下发与清除同源,沿用 web 侧「挂载前缀单一事实来源」惯例),`Max-Age` = admin refresh TTL,`Secure` 共用 `cookie_secure` 配置。helper 平行复刻:`admin_session_service(state)` / `issue_admin_refresh_cookie(...)` / `admin_refresh_cookie(token, state)`。
- refresh 编排照搬 web 侧「轮换压轴」次序:peek → 查 admin → status 检查(disabled ⇒ 与无效 token 不可区分的 401)→ 签 access → **rotate 压轴** → 组装。服务端瞬时故障不烧客户端凭证。
- **绝对会话上限 7 天(Q8,与 web 滑动语义刻意相反)**:`issue`(仅 login 调)时 `expires_at = now + 7d`;`rotate` 的新枚 **继承被消费旧枚的 expires_at,不重算**——轮换只换凭证不续命,`expires_at` 事实上成为「本次登录的绝对死线」。到期后 refresh 走既有过期分支 401 ⇒ 前端跳登录,重走 2FA。实现落点:admin 版 `consume_and_insert` **不收 expires_at 参数**,CTE 单条 SQL 里 `UPDATE ... RETURNING admin_id, expires_at` 再把该 expires_at 直接喂给 INSERT(继承在 DB 层原子完成,service 无法传错)。连带两点:①响应的 `refresh_token_expires_at` 在整个会话期恒定不变(前端可据此做「即将到期」提示);②refresh 下发的新 cookie `Max-Age` 应设为**剩余寿命**(`expires_at - now`)而非满血 7 天,否则 cookie 活得比 token 久(无害但不洁——过期 cookie 只会换来 401)。

## 6. 登录流程(2FA:手机号 + 密码 + 验证码,三参数必填)

**Q7 已定(2026-07-19 追加拍板)**:login 请求 `{phone, password, code}`——admin **仅以手机号为登录标识**(email 只是联系资料,不能用来登录,`get_by_identifier` 的判 `@` 逻辑不进 admin 域);密码与短信验证码**双因子缺一不可**。这是四方案(§18-Q7 背景)里最强的一档,go 侧无此设计,属 tsz-rust 主动升级(§15 偏离 8)。

验证码复用 OTP 域全套:`Purpose` 枚举加 `AdminLogin` 变体(`as_key_segment` = `"admin_login"`,Redis key 隔离天然成立),TTL/冷却/日限/attempts 锁死照旧。

```
phone 归一化(trim)→ get_by_phone;NotFound ⇒ InvalidCredentials(不可区分,仍跑 dummy_hash 平衡时序)
  → ① 锁定检查(先于一切因子):locked_until 在未来 ⇒ 423
       —— 密码/验证码全对也拒,且**不累计失败**(攻击者不能把解锁时间无限后推)
  → ② bcrypt 比对:失败 ⇒ register_failed_login(threshold=5, lock=15min) 原子累计;
       恰好第 5 次 ⇒ 直接 423;否则 401
  → ③ status 检查(密码之后:错密码永不泄露禁用态):disabled ⇒ 403 "account disabled"
       —— 放在验证码之前:禁用账号不烧码
  → ④ 验证码校验 otp_service.verify(phone, AdminLogin, code)(CAS 单次消费):
       失败 ⇒ **同样 register_failed_login 累计**(密码对+码错 = 有密码没手机,更可疑)
              且响应与②逐字节一致 401——**码错绝不能与密码错可区分,否则验证码层
              反而成了密码爆破的确认 oracle**(拿垃圾码试密码,看报错就知道密码对没对)
       Store 错(Redis 挂)⇒ 503 fail-close
  → ⑤ clear_failed_logins(有残留才写)
  → ⑥ 签 access + issue refresh(单登录:先 revoke_all)——cookie 压轴,可失败步骤全在前
  → 200,body 含 must_change_password(登录本身不拦,由守卫拦其余端点)
```

已知可忽略的时序差:只有密码正确才触达 Redis(④),「密码对码错」比「密码错」多一次 Redis 往返(~1ms)——bcrypt(百 ms 级)占绝对主导,不构成可用 oracle;若反序(先验码)则攻击者拿密码爆破就能烧掉管理员的真码,是更糟的 DoS。

**发码端点 `POST /admin/auth/login-code`**(公开组):请求 `{phone}`,**恒返 202**——服务端只在 phone 是 active admin 时才真正 `otp_service.request`,其余(查无此号/已禁用/**冷却中/日限满**)一律静默压平成同一个 202。理由:admin 端点公网可达,发码若校验存在性并区分响应,= 「任意手机号收短信」的成本攻击面 + 「这个号是不是管理员」的名录枚举面(哪怕只有 429 与 202 之差也能探出来),双杀。前端发码按钮做本地 60s 倒计时即可,不依赖 429 反馈(**与 web `/otp/send` 的 429 语义刻意不同**,§15 偏离 9)。仅 Redis/sender 故障返 503(fail-close,基础设施故障无需也不应隐藏)。

repository 层两个原子方法(hardening-D8,单条 UPDATE 无 read-modify-write 竞态):

- `register_failed_login(id, threshold, lock_for)`:计数 +1;达阈值 ⇒ 置 `locked_until = now()+lock_for` 且**计数归零**(下个窗口重新有满额 5 次),返回是否触锁;顺手清过期的 stale locked_until。
- `clear_failed_logins(id)`:双列清零。

**423 需要 `AppError` 新变体**(如 `Locked(String)` → `StatusCode::LOCKED`)。423 与 401 区分构成轻微账号枚举 oracle——hardening 文档已明知并接受(admin 账号极少,这是任何锁定方案的固有属性)。

## 7. must_change_password 守卫

- 触发:二期的创建(`provision`)与重置(`reset_password`)置 true;`change_password` 成功时**与写 hash 同一条 UPDATE** 原子清除。
- 执法(hardening-D6):admin 子路由分三组挂载——
  - **公开组**:login/refresh/logout(仅 cookie,无 Bearer);
  - **逃生组**(有 Bearer、守卫外):logout-all、change-password——被强制改密者的唯一出口;
  - **守卫组**(有 Bearer、守卫内):profile 及其余全部。守卫 = 一层 axum middleware(或组合提取器),`AdminAuth` 之后查库 `must_change_password`,true ⇒ 403 `must_change_password` Problem。admin 行消失 ⇒ 401(视为过期会话,非 500)。
- 每请求一次索引查,后台 QPS 低,不缓存(rbac-D7 同理)。
- **前端契约**:靠 403 的 `code` 字段整页跳 `/change-password`(刷新后 refresh 响应不带 flag,靠这个 403 重新发现)——`code` 字段是硬契约,见 §12。

`change_password` 服务顺序:查账号 → bcrypt 验旧密(错 ⇒ 401 "current password is incorrect")→ **新==旧 ⇒ 400 "new password must differ from the current one"**(关键:阻止「临时密码改成临时密码」洗掉 flag)→ 策略校验(§8)→ `set_password(hash, must_change=false)` 原子落库 → 204。**不吊销当前会话**——go 注释明示这是记录在案的 follow-up,且前端改密后整页 reload 依赖存活 refresh cookie 重建会话;若要改成吊销必须同步改前端,本轮不动。

## 8. 密码策略与临时密码

**策略**(hardening-D7,作用于 change-password、seed、二期 provision 的回验):

- ≥12 字符(按 char 数)且 ≤72 字节(bcrypt 截断上限,web 侧同守);
- 非纯数字;
- 不含弱子串(小写匹配):`password qwerty 123456 letmein iloveyou welcome admin123`(**故意不含 `admin`**——后台用户名里全是它,含之则误伤面太大);
- 不含本人手机号子串。

落点:`admin/model.rs` 的校验函数(如 `validate_admin_password(pw, phone) -> Result<(), AdminPasswordError>`)。**不复用 web 的 `Password` newtype**——那是「≤72 字节」单一守卫,语义不同;但错误映射惯例(逐条规则→单一 400,message 说明具体规则)沿用。

**临时密码**(hardening-D2,创建与重置共用):20 位,charset = 字母数字去 `0 O 1 l I`(人工转录友好;无 `1` ⇒ 结构上不可能含手机号),`getrandom` + 拒绝采样(复用 OTP `generate_code` 的去偏手法),生成后回验策略、最多重试 8 次。明文仅随响应返回一次,永不落库/进日志/进审计。

## 9. super_admin 治理矩阵(二期)

全部 `/admins/*` 端点套 `RequireSuperAdmin`(从 `AdminAuth.role` 判,非 super ⇒ 403 "super admin required"):

| 操作 | 目标=admin | 目标=super_admin |
|---|---|---|
| `POST /admins`(provision) | 只能造 role=admin(**请求根本没有 role 字段**,account-D2 的「结构上杜绝」——同 web 注册无 role 字段的手法) | 不可达(造不出来) |
| `PATCH /admins/{id}/status` | ✓,返回更新后 Admin | 403 "cannot change a super admin's status" |
| `POST /admins/{id}/reset-password` | ✓,返回一次性临时密码 | 403 "cannot reset a super admin"(含 super 重置自己) |

**reset 副作用链的顺序有讲究**(hardening-D5):先 `revoke_all`(踢目标全部会话)再 `set_password(hash, must_change=true)`。两步不共事务:revoke 成功而 set 失败 = 目标被登出但旧密码仍可登录,自愈;**反序**则可能出现「会话没踢、临时密码没人知道」的死锁窗口。

**provision**:phone 必填(5–20)、display_name 必填(1–50,trim,拒 `<>`/控制符/Cf——约束与 web `DisplayName::parse` 高度重合,实现时评估直接复用);**无 email(Q9)**;判重**不先查**、直接 insert 靠唯一索引 + 23505 映射 409(user 域同哲学);置 must_change=true(列默认即 true,显式写更稳);201 返 `{admin, temporary_password}`。

**seed super_admin**(带外,Q5 已定:独立 bin `seed_admin`,服务器上跑,密码运行时传入不进 .env):幂等语义照 go——phone 不存在 ⇒ 建 active super(密码须过策略,must_change=false);已存在 ⇒ 自愈提升 level、重激活 status、**不动密码**。⚠️ 2FA 之后 seed 的 phone 必须是**真实可收码的手机号**(登录要过验证码因子;测试环境码在 journald,生产必须真短信)。

## 10. 权限模型(RBAC 已取消,Q10)

产品定案:**只有 admin/super_admin 两级身份,没有角色/权限委派概念**。go 侧已落地的 RBAC(admin_roles 三表、roles/permissions 端点、派角色)不移植;权限执法只剩一道闸——`RequireSuperAdmin`(管理员管理、users 写操作),从 `AdminAuth.role` 判,全体 admin 其余功能全开。

**`profile.permissions` 的处置**:前端菜单渲染依赖此数组(恒为数组不为 null 的契约),保留字段、**恒返全量菜单 key 死数据**(Rust 常量表,12 个 `*.access` key,顺序即侧栏顺序)——前端零改动,菜单全开;「管理员管理」菜单继续按 `role == super_admin` 前端门禁(rbac-D9 的残余语义)。若日后前端决定砍掉 permissions 渲染逻辑,该字段随 openapi sync 一起删,只动一处常量。

**profile 端点落地记录(2026-07-26,`GET /admin/profile` 已上线,全绿)**:

- **permissions 只在 profile 下发、login 概要不带**(用户拍板)。曾议 login-only 方案,被 F5 否决:
  会话恢复走 refresh→profile,login 一次性下发撑不住该链路;「任何 F5 后仍需要的数据必须
  可从查询端点获得」。login 侧有防回潮测试钉死(admin_login_handler)。
- **profile 刻意不查 locked_until**(有 `locked_admin_still_gets_200` 正向钉子):锁定语义
  只挡新登录/refresh 轮换(防爆破),不打断已认证的短命 access token——否则错码轰炸可把
  在线管理员打下线(DoS)。refresh 查锁(423)的拍板不变,两者语义不同勿混。
- must_change 守卫按 §7 内联在 handler(当前唯一守卫组端点);扩员时再抽 middleware。
- `AdminAuth` 提取器落 `admin/extract.rs`,role claim 认不出 fail-closed 401。
- `AdminProfileResponse` 平铺 5 字段(不用 serde flatten——utoipa 对 flatten 生成 allOf,
  平铺才有干净 properties);与 login 概要 `AdminProfile`(4 字段)是两个类型。
- 前端已同步:`AdminProfile.level→role`(Q11 收尾,store/守卫/顶栏/夹具全改),契约测试
  移出 PENDING;`Admin` 管理列表类型的 level 留待 admins 管理批次一并改。

## 11. 端点契约总表(前端对齐硬指标)

挂载 `ADMIN_MOUNT = /api/v1/admin`。wire 字段全 snake_case。`Admin` 对象序列化:`id, phone, display_name, role, status, created_at, updated_at`(**wire 字段名统一 `role`,偏离 go/存量前端类型的 `level`**,前端随 openapi sync 改)——**手挑字段,绝不序列化 password_hash/must_change_password/锁定列**(web 侧防泄惯例)。

| 端点 | 鉴权 | 请求 | 成功响应 |
|---|---|---|---|
| `POST /auth/login-code` | 公开 | `{phone}` | **恒 202** 空 body(反枚举,见 §6;仅基础设施故障 503) |
| `POST /auth/login` | 公开 | `{phone, password, code}`(2FA 三必填,Q7) | 200 `{admin, access_token, role, expires_in, refresh_token_expires_at, must_change_password}` + Set-Cookie |
| `POST /auth/refresh` | cookie | 无 body | 200 `{access_token, expires_in, refresh_token_expires_at}` + 新 cookie |
| `POST /auth/logout` | cookie | 无 body | 204,清 cookie(幂等,web 侧 T4 同语义) |
| `POST /auth/logout-all` | Bearer(逃生组) | 无 body | 204 |
| `POST /auth/change-password` | Bearer(逃生组) | `{current_password, new_password}` | 204 |
| `GET /profile` | Bearer(守卫组) | — | 200 `{id, phone, display_name, role, permissions:[key]}`(permissions 恒全量死数据,§10) |
| `GET /admins` | super | `?page&page_size&role&q` | 200 `{items:[Admin], page:{page,page_size,total}}` |
| `POST /admins` | super | `{phone, display_name}`(无 email,Q9) | 201 `{admin, temporary_password}` |
| `PATCH /admins/{id}/status` | super | `{status}` | 200 更新后 Admin |
| `POST /admins/{id}/reset-password` | super | 无 body | 200 `{temporary_password}` |
| `GET /users` | admin(三期) | `?role&q&registered_from&registered_to&page&page_size` | 200 `{items:[AdminUser], page}` |
| `GET /users/{id}` | admin(三期) | — | 200 AdminUser |
| `PATCH /users/{id}/status` | super(三期) | `{status}` | 200 更新后 AdminUser |
| `PATCH /users/{id}` | super(三期) | `{display_name}` | 200 更新后 AdminUser |

补充契约点:

- 前端 access token **仅存内存**、refresh 全靠 cookie(`credentials:"include"`),从不接触 refresh 明文——cookie 契约是硬前提,无 body 双轨。
- 分页:`page` 默认 1(clamp≥1)、`page_size` 默认 20(clamp 1..100);`q` ILIKE 子串检索(`%`/`_` 不作通配,入参转义)——admins 列表查 phone/display_name(无 email,Q9),users 列表查 phone/email/display_name;列表按 `created_at DESC`;`items` 空为 `[]` 恒非 null。
- `registered_from/to`:RFC3339,**半开区间 `[from, to)`** 过滤 created_at。
- `AdminUser`(三期):`{id, phone?, email?, display_name, avatar_url(未设为 ""), roles:[student|teacher], status, created_at, updated_at}`——复用 web 侧 `get_roles_by_user_id`;不含 level/coin_balance。
- OpenAPI:每落一条端点同步 utoipa 注解 + `docs/openapi.json` 重导出 → 前端 `sync:openapi` → 从 PENDING 白名单移除(契约测试强制)。cookie 参数声明沿用 web 侧 ⑧ 的手法。

## 12. 错误与不可区分汇总

| 场景 | 状态码 | body |
|---|---|---|
| 账号不存在 ≡ 密码错 ≡ **验证码错**(login) | 401 | `invalid_credentials` Problem 三态逐字节一致 + dummy_hash 平衡时序(码错可区分 = 密码爆破 oracle,§6④) |
| 锁定中(密码对错皆然) | **423** | `account_locked` Problem |
| disabled(login,密码对) | 403 | `account_disabled` Problem |
| refresh:未知/已吊销/过期/重放/属主 disabled | 401 | `invalid_refresh_token` Problem 全笼统 + 清 cookie |
| refresh/logout 缺 cookie | 401 / 204 | logout 幂等 204(web T4 定案沿用);refresh 401 |
| 旧密码错(change-password) | 401 | `invalid_credentials` Problem |
| 新旧相同 / 弱密码 | 400 | message 说明具体规则 |
| 非 super 碰 super 端点 | 403 | `forbidden` Problem |
| 被强制改密碰守卫组 | 403 | `must_change_password` Problem |
| provision 撞手机号 | 409 | `phone_already_registered` Problem（email 冲突态已随 Q9 消失） |

**`AppError` 需两处扩展**:① `Locked(String)` → 423;② 带 `code` 的 403/400 变体(如 `ForbiddenCode{error, code}` / `BadRequestCode{...}`,或 admin 域局部响应类型——实现时选,前端只认 body 形状)。`code` 是前端路由依据(`must_change_password` ⇒ 跳改密页),属硬契约。

## 13. 模块结构 + Rust 骨架

```
src/admin/
  mod.rs          // pub const ADMIN_MOUNT: &str = "/api/v1/admin"; re-exports
  model.rs        // Admin 行结构 + AdminRole/AdminStatus 枚举(sqlx TEXT 映射)
                  //   + validate_admin_password + generate_temporary_password
  repository.rs   // AdminRepository{pool}: create/get_by_id/get_by_phone(仅手机号,Q7/Q9)
                  //   /list(分页筛选)/set_status/set_password(hash,must_change 原子)
                  //   /register_failed_login/clear_failed_logins
  session.rs      // AdminRefreshTokenRepository + AdminSessionService(web session 平行复刻,
                  //   consume_and_insert 继承 expires_at,Q8)
  service.rs      // AdminService: login(2FA)/change_password/provision/reset_password(编排+治理矩阵)
  extract.rs      // AdminAuth 提取器 + RequireSuperAdmin + MustChangeGuard
  handler.rs      // 三组路由:公开/逃生/守卫 + DTO + 错误映射 + cookie helpers
                  //   + MENU_PERMISSIONS 全量菜单 key 常量(profile 死数据,§10)
```

装配(`state.rs`/`lib.rs`):

```rust
// AppState 追加
pub admin_token_manager: Arc<TokenManager>,   // Realm::Admin + admin_jwt_secret
pub admin_refresh_ttl: Duration,

// lib.rs 挂载(公开/逃生/守卫三组见 §7)
.nest(admin::ADMIN_MOUNT, admin::router())
```

config 新增:

| 键 | 必填/默认 | 说明 |
|---|---|---|
| `ADMIN_JWT_SECRET` | **必填** | 与 `JWT_SECRET` 隔离的第二把密钥(部署先加,见 §4) |
| `ADMIN_ACCESS_TTL_MINUTES` | 默认 15 | 独立于 web,默认相同 |
| `ADMIN_REFRESH_TTL_DAYS` | 默认 **7** | **绝对上限非滑动**(Q8):轮换不续期,到期必重登 |

**解析后校验(用户拍板 2026-07-19)**:`admin_jwt_secret == jwt_secret` ⇒ 启动即失败——两把密钥相同则 per-realm 隔离塌一半(只剩 aud 一道墙),而复制粘贴同一串恰是最易犯的部署失误。校验放 `Config::from_pairs`(生产与测试共用的接缝),错误可用 `envy::Error::Custom` 免改签名。

锁定参数(5 次/15 分钟)与宽限(20s)**硬编码 service 层**,镜像测试常量——go 也未做成配置,无场景要热调。

## 14. 与 web 域的复用清单(实现时照抄,别重造)

- `TokenManager`:第二实例即可,零改动。
- session 全套模式:`consume_and_insert` CAS、`RotatedRefresh` 无 Debug、peek→轮换压轴、宽限窗口、验尸防 DoS——换表名平移。
- cookie 三 helper 形状、挂载前缀常量惯例、`cookie_secure` 配置。
- `dummy_hash()` 时序平衡、「先密码后状态」顺序、23505→409 映射、bcrypt DEFAULT_COST + 72 字节守卫。
- OTP `generate_code` 的拒绝采样手法(→临时密码)。
- `DisplayName::parse`(provision 的 display_name 校验,约束一致则直接复用)。
- utoipa cookie 契约声明 + `cookie_contract_is_documented` 守护测试的手法。

## 15. 与 tsz-go 的刻意偏离清单(重写即校准)

| # | go 行为 | tsz-rust 决定 | 理由 |
|---|---|---|---|
| 1 | 宽限窗口 60s、窗口内**铸新 token** | 20s、窗口内 401 不铸币 | 复用 web 侧已验证语义;前端 single-flight 已消掉竞速场景 |
| 2 | 409 带 `"field":"phone"` 机器可读字段 | 暂不带,message 区分 | 前端未消费 field;要加时只动一处 DTO |
| 3 | 每写操作入 audit 表 | **推迟**,先 `tracing` 结构化日志(`admin.create` 等同名事件) | tsz-rust 尚无 audit 域;表设计另立文档,事件名先对齐便于日后回灌 |
| 4 | login 挂 per-IP 限流 | **推迟** | 账号锁定已挡撞库主路径;生产暴露前与 CORS/告警一并重估(记入上线清单) |
| 5 | 失败登录 Grafana 告警(hardening-D9) | 推迟 | 观测栈未搬;423/401 计数进 tracing,接观测时补规则 |
| 6 | nginx Basic Auth 门控时序(hardening-D10) | 不适用 | tsz-rust 生产没有这层;admin 端点上线即裸奔于公网,**一期上生产前必须确认锁定+密码策略已就位** |
| 7 | seed 是独立 Go cmd | 独立 Rust bin(Q5 已定,密码运行时传入) | — |
| 8 | admin login = identifier(手机/邮箱)+密码**单因子** | **手机号+密码+验证码 2FA**,仅手机号标识(Q7,2026-07-19) | 后台安全主动升级 |
| 9 | (web 侧)`/otp/send` 冷却中返 429 | admin `login-code` **恒 202 压平**(含冷却/日限/查无此号) | 反管理员名录枚举;前端本地倒计时,不依赖 429 |
| 10 | refresh 滑动续期,TTL 默认 30 天(go admin 与 web 同) | **绝对上限 7 天,轮换继承 expires_at 不续期**(Q8) | 后台会话定期强制重认证;2FA 下重登成本可控 |
| 11 | admins 有可选 email 列(可用于登录) | **无 email 列**(Q9) | 产品定案:admin 仅手机号,连联系资料都不留 |
| 12 | RBAC 全套已落地(角色/权限/派角色) | **整体取消**(Q10):两级身份 + `RequireSuperAdmin` 单闸;permissions 恒全量死数据 | 产品定案:无角色概念 |
| 13 | 身份列/wire 字段名 `level` | 列/枚举/claim/wire **统一 `role`**(Q11) | 与 JWT claim 同名;RBAC 取消后无 role_id 撞车 |
| 14 | must_change_password 列默认 false | 默认 **true**(fail-secure,漏赋值路径最多逼人改密);**seed 必须显式写 false** | 绿地建表,方向可以选更稳的 |

## 16. 可测性(我写测试要用)

- schema 约束:两张新表的 CHECK/UNIQUE/FK 级联/默认值(含 must_change 默认 **true**),`tests/admin_schema.rs`。
- repository:锁定状态机(计数累积/第 5 次触锁+归零/过期锁清理/成功清零,**并发 register_failed_login 原子性**)、`set_password` 的 hash+flag 同条 UPDATE、分页筛选(q 转义 `%_`)、23505 映射;admin 会话仓库对照 `session_repository.rs` 全套(含 CAS 三条:happy/落空不插/回滚)。
- service:login 2FA 顺序矩阵(锁定先于一切因子/密码先于 disabled/disabled 先于验证码且不烧码/码错也累计锁定/锁内全对也 423 且不累计)、验证码注入走 `for_test_with_otp_store` 手法(login-otp 测试同款)、密码策略逐条、临时密码(长度/charset/回验重试)、change-password 新旧相同拦截、治理矩阵全部 403。
- login-code 反枚举:查无此号/已禁用/冷却中/日限满四态**响应逐字节一致恒 202**;仅 active admin 真正落码(用注入 store 反查 Redis 键存在性断言);Redis 挂 503。
- 绝对会话上限(Q8):rotate 后新枚 `expires_at` 与旧枚**逐值相等**(DB 行比对,不是「约等于」);连续多次轮换 deadline 恒定;回拨 `expires_at` 到过去 ⇒ refresh 401;refresh 响应的 `refresh_token_expires_at` 与 login 时一致;新 cookie `Max-Age` = 剩余寿命(≤ 首发值)。
- handler E2E:cookie 全属性断言(名/Path=/api/v1/admin/Strict)、login 响应形状与 DB 行比对(`refresh_token_expires_at`)、must_change 守卫矩阵(守卫组 403+code/逃生组通/改密后即通)、**不可区分逐字节断言**(401 两态、refresh 全笼统)、423 文案、重放连坐与宽限(回拨 `rotated_at` 手法照搬)、web/admin token 互换必 401(跨 realm 隔离端到端)。
- 契约:openapi 快照 → 前端 PENDING 白名单逐条移除;`cookie_contract_is_documented` 扩到 admin 端点。

## 17. 落地顺序

1. **一期(auth 闭环)**:迁移两张表 → config 三键 + AppState 装配 → `AppError` 扩展(423 + code 变体)→ model(枚举/密码策略)→ OTP 域 `Purpose::AdminLogin` 变体 → AdminRepository(含锁定方法)→ admin session 平移 → login-code/login(2FA)/refresh/logout/logout-all/change-password/profile(permissions 恒全量死数据)→ must_change 守卫(机制先就位,flag 触发点在二期)→ CORS(Q6)→ OpenAPI + 前端白名单移除 6 条 + **新增 login-code 契约**(admin 前端登录页需加发码按钮+验证码栏,本地 60s 倒计时)。
   ⚠️ **真短信 sender 从「web 线待办」升格为 admin 一期的生产前置**:2FA 下验证码是登录必经因子,`OtpSender::Mock`(码打 journald)意味着生产环境后台无法登录——admin 线上生产前必须接真短信通道(测试环境可 `journalctl -u tsz-rust | grep otp_code_sent` 取码过渡)。
2. **二期(admins 管理)**:provision/list/status/reset-password + 临时密码内核 + seed 机制 → 白名单再移 4 条。
3. **三期(users 管理)**:users 4 端点(list/详情/status/编辑昵称)→ 白名单清空 admin 身份区。
   (原三期的 RBAC 已取消,Q10:白名单里 6 条 RBAC 端点——roles×4、permissions、派角色——属「取消」而非「待实现」,待前端下架 Roles 页面后从白名单与 admin.ts 一并删除。)

每期节奏照旧:设计核对 → 我出测试骨架/红灯 → 你实现 → 全绿 → clippy → 部署冒烟。

## 18. 决策记录(Q1–Q6 已定,2026-07-19)

- **Q1 单登录:定案「严格单登录」**——admin issue 前 `revoke_all_by_admin_id`(go 语义,后台账号不允许多处在线)。
- **Q2 宽限语义:定案「沿 web 侧」**——20s 内 401 不铸币不连坐,窗口外 revoke_all 连坐(§15 偏离 1 成立)。
- **Q3 AppError 扩展:定案「全局加」**——`Locked(String)` → 423 + 带 `code` 的 403/400 变体(通用能力,web 侧将来也用得上)。
- **Q4 profile 过渡期 permissions:定案「返回死数据」**——一、二期恒返全量目录 key(前端菜单全开,= go backfill 零行为变化语义);三期接真 RBAC 表。
- **Q5 seed 形态:定案「独立 bin,服务器上跑,密码运行时传入」**——`src/bin/seed_admin.rs`,`cargo run --bin seed_admin`,密码走命令行参数/交互输入,**不进 .env**(避免明文残留);幂等语义照 §9(存在则自愈提升,不动密码)。
- **Q6 CORS:定案「一期顺带做」**——tower-http `CorsLayer`,一次配好 web + admin 两个来源,允许 credentials(cookie 契约需要);配置与加固细节补进 deployment.md。
- **Q9 无 email(2026-07-19 产品定案)**:admins 无 email 列——不作登录标识,连联系资料都不留。provision 请求、Admin 序列化、409 冲突态、`lower(email)` 部分索引全线随之删除;admins 列表 `q` 只查 phone/display_name。
- **Q10 无 RBAC(2026-07-19 产品定案)**:没有角色/权限委派概念,只有 admin/super_admin 两级身份。原三期 RBAC(三表迁移、roles/permissions 端点、派角色)整体取消;权限执法只剩 `RequireSuperAdmin` 单闸;`profile.permissions` 保留字段恒返全量菜单 key 死数据(§10)。
- **Q11 身份命名统一 `role`(2026-07-19)**:列/Rust 枚举(`AdminRole`)/JWT claim/wire 字段四处同名——RBAC 取消后无 `role_id` 撞车,且与 claim 名天然一致。**偏离 go 与存量前端类型的 `level`**,前端随 openapi sync 一并改。
- **Q12 must_change_password 列默认 true(2026-07-19,采纳用户迁移方案)**:fail-secure——漏赋值的路径最多逼人改一次密码(反之 false 是 fail-open)。纪律:**seed 建超管必须显式写 false**;provision 显式写 true(不依赖列默认,双保险)。
- **Q8 会话寿命:定案「绝对上限 7 天,轮换不续期」(2026-07-19 追加拍板)**——`ADMIN_REFRESH_TTL_DAYS` 默认 7;`rotate` 继承旧枚 `expires_at` 不重算(admin 版 `consume_and_insert` 不收 expires_at 参数,CTE 在 DB 层原子继承);到期 refresh 401 ⇒ 必重走 2FA。与 web 的滑动续期刻意相反(§15 偏离 10);cookie Max-Age 随剩余寿命。
- **Q7 登录因子:定案「手机号+密码+验证码 2FA,三参数必填」(2026-07-19 追加拍板)**。背景:曾评估四方案——仅密码 / 仅验证码 / 双轨任选 / 双因子;「仅验证码」因 SIM 攻击面+可用性耦合+Mock 阻塞被否,「双轨任选」安全性=两通道最弱者也被否,最终取最强的双因子。admin 仅以**手机号**为登录标识(email 降为纯联系资料)。随之的执行细则(§6,如有异议再议):①码错≡密码错≡账号不存在,401 逐字节一致(防密码爆破 oracle);②码错也累计账号锁定;③disabled 在验证码之前检查(禁用账号不烧码);④发码端点 `login-code` 恒 202 压平一切非基础设施结果(反名录枚举,含冷却/日限);⑤真短信 sender 升格为 admin 生产前置。
