# user 域 service 层待测断言清单

> 用途：写 `user/service.go`（Rust: `user/service.rs`）+ OTP/session 服务时**对着打勾**。每条都是一个可落成测试的断言，源自 **tsz-go 的现成测试 + 服务逻辑**（不是我凭空编的，每条都附 go 出处），并已对齐 Rust 侧的 schema 与 [MVP 范围](user-domain-reference.md)。
>
> 来源：`tsz-go/internal/{user,otp,session,auth}` 的 `service.go` + `service_test.go` + `contract_test.go` + `handler_test.go`，逐条抽取后过了一遍完整性批判补漏。共约 246 条。
>
> **怎么用**：先写 §0 的安全不变量对应的测试（这些错了就是安全事故）；再按 A–F 分模块，先做 `[MVP]`、`延后` 的先留着占位别删。层次标签 `[...]` 告诉你这条测试写在哪一层。

## 图例

| 层次标签 | 含义 | 写在哪 |
|----------|------|--------|
| `[纯函数]` | 无依赖的纯逻辑（校验、归一化、生成） | 同文件 `#[cfg(test)]`，最好测 |
| `[服务+mock仓储]` | service 逻辑，仓储用 fake | service 单测 |
| `[契约·真库]` | 需要真实仓储/DB 行为 | 契约测试（fake + `#[sqlx::test]` 双跑） |
| `[handler层]` | 请求绑定、HTTP 状态码、cookie、响应体 | handler 测试 / e2e |

`MVP` = 第一周登录注册主线要有；`延后` = 第二周或非 MVP，先把断言留着占位。

---

## 🔒 §0. 安全 / 隐私不变量（错了就是安全事故，最先测）

这批是整份清单的核心，从各模块抽出来单独盯。每条后面括号是它所在的详细小节。

1. **错误不可区分（不泄露账号是否存在）**：登录/验证码/重置/注销/绑定里「码错 / 过期 / 已用 / 号未注册 / 密码错」全部返回**同一个**领域错误；并且 `发码` / `忘记密码` 这类**请求码的 HTTP 端点，号注册与否都一律返回 200**——端点不能当账号探测器。(B, D, G)
2. **爆破上限**：每个验证码**最多 5 次错误猜测**；第 6 次（或 5 次错后再给对码）因 `attempts >= 5` 在比对前被拒；前 5 次里给对码仍成功。(D)
3. **不存明文**：密码 `bcrypt(DefaultCost)`；refresh token 只存 **SHA-256 哈希**，绝不存原串。(A, E)
4. **禁用账号懒拦截**：`disabled` 在**每个登录 + refresh 边界**被拒，一个 access-token TTL 内生效；`SetStatus(disabled)` **不主动吊销**现有会话（下次 refresh 才拦）。(B, G)
5. **单设备 + 轮换防盗**：签发新 refresh 会吊销该用户其它 token；已轮换的 token 在**宽限窗内**重放放行（并行标签页/丢响应重试），**超窗重放视为被盗 → 吊销该用户全部会话**；硬吊销无宽限。(E)
6. **改密 / 注销吊销全会话**：`ResetPassword`、`DeleteAccount` 都**先吊销全部会话**，让持旧 token 的攻击者到处被登出。(G)
7. **码绑定到目标与渠道**：码绑定到 `normalizeIdentifier(target)`，一个渠道/标识的码**不能跨用**；注销码发到账号**已有**联系方式（证明所有权），绑定码发到请求里的**新**联系方式（证明控制新联系方式）。(D, G)
8. **单次使用**：所有码消费后重放必失败。(D, G)
9. **display_name 防注入**：禁 `<` `>`、控制符、Unicode Cf（零宽/BOM/bidi）；**允许** `'` `"` `&`（O'Brien、Tom&Jerry）；trim 后非空；1–50 字符。(A)
10. **refresh cookie 加固**：`HttpOnly + Secure + SameSite=Strict + Path 限定`；refresh token **只进 `Set-Cookie`，绝不进 JSON body**（否则 JS 可读、被日志/缓存，HttpOnly 设计白搭）。(G)
11. **6 位码用拒绝采样生成**（数字均匀），别用 `byte % 10`；比对用常量时间。(D)

> ⚠️ 两个设计缺口（详见 §H）：
> - **纯 DB 约束别在 service 层重测**：CHECK/DEFAULT/UNIQUE/PK/成对-null 这些已在 `tests/*_schema.rs` 覆盖；service 测试只断言「约束冲突 → 领域错误」的**映射**（如 `23505` on `users_phone_key` → `ErrPhoneTaken`），别再断裸约束。
> - **双身份两套 key 现在还满足不了**：`src/config.rs` 只有**单个** `jwt_secret`。§F 里 admin/web 跨 realm 密钥分离的断言，在加第二把签名密钥前无意义——目前只有 realm-claim 检查（防御纵深，不是主边界）。

---


## A. 注册 + 身份 + 输入校验

**MVP：**

- [ ] **given a valid phone+email+password+display_name+role, when Register is called, then it creates a persisted user (retrievable by id), records the single role on the user, hashes the password, and returns an access token (subject=user id, role=registered role) plus a non-empty refresh token** `[服务+mock仓储]`  
  <sub>TestService_Register_Success (service_test.go:25); service.go Register:179</sub>
- [ ] **given a phone but empty email, when Register is called, then it succeeds and the stored user's email is empty/None** `[服务+mock仓储]` — phone-or-email二选一：单手机账号合法（users_phone_or_email_present 只要求至少一个）  
  <sub>TestService_Register_OptionalEmail (service_test.go:72)</sub>
- [ ] **given empty phone but a valid email, when Register is called, then it succeeds, the stored phone is empty/None, the email is lowercased, and both access+refresh tokens are returned** `[服务+mock仓储]` — 单邮箱账号合法（000014 把 phone 改为可空）  
  <sub>TestService_Register_EmailOnly (service_test.go:83)</sub>
- [ ] **given both phone and email empty (including whitespace-only that normalizes to empty), when Register is called, then it returns ErrMissingIdentifier and no user is created** `[服务+mock仓储]` — 应用层在写库前拦截'手机邮箱都空'，与 DB 的 users_phone_or_email_present CHECK 双重保障；避免依赖 DB 报错兜底  
  <sub>TestService_Register_MissingIdentifier (service_test.go:100); service.go Register:184-187</sub>
- [ ] **given a phone with surrounding whitespace (e.g. ' 13800138000 '), when Register is called, then the stored phone is trimmed ('13800138000')** `[服务+mock仓储]`  
  <sub>TestService_Register_Success (service_test.go:34); service.go normalizePhone:701</sub>
- [ ] **given an email with mixed case (e.g. 'Alice@Example.com'), when Register is called, then the stored email is lowercased ('alice@example.com')** `[服务+mock仓储]` — 配合 lower(email) 部分唯一索引；应用层先小写化以保证与索引一致的大小写不敏感唯一性  
  <sub>TestService_Register_Success (service_test.go:37); service.go normalizeEmail:697</sub>
- [ ] **given an email with surrounding whitespace, when Register is called, then the email is trimmed before lowercasing and write** `[纯函数]`  
  <sub>service.go normalizeEmail:697 (strings.ToLower(strings.TrimSpace)); TestNormalizeIdentifier (service_test.go:960)</sub>
- [ ] **given a plaintext password, when Register is called, then password_hash is non-empty, is not the plaintext, and bcrypt-verifies against the original password** `[服务+mock仓储]` — 密码绝不明文存储；bcrypt DefaultCost  
  <sub>TestService_Register_Success (service_test.go:48-53); service.go Register:194 (bcrypt.DefaultCost)</sub>
- [ ] **given any password, when Register hashes it, then the bcrypt cost equals the library default cost (go: bcrypt.DefaultCost; Rust: the equivalent default)** `[服务+mock仓储]` — 哈希强度是安全参数，需固定为 DefaultCost 而非任意值  
  <sub>service.go Register:194; reference doc section 6 '密码哈希 bcrypt DefaultCost'</sub>
- [ ] **given a password longer than 72 bytes, when Register is called, then it returns an error (bcrypt 72-byte cap) and does not persist a half-built user** `[服务+mock仓储]` — bcrypt 拒绝 >72 字节；service 必须把它当错误返回而不是落一个残缺用户。Rust bcrypt 同样有 72 字节上限，需显式处理  
  <sub>TestService_Register_BcryptError (service_test.go:203); service.go Register:194-197</sub>
- [ ] **given a role that is not 'student' or 'teacher' (e.g. 'superuser', 'admin', 'Student' with wrong case, or empty), when Register is called, then it returns ErrInvalidRole before any hashing or DB write** `[服务+mock仓储]` — 角色必须是已知 web 角色；'admin' 不是 web 角色（独立身份域），大小写敏感  
  <sub>TestService_Register_InvalidRole (service_test.go:213); TestRole_Valid (service_test.go:973); service.go Register:180-182</sub>
- [ ] **given a valid registration with role R, when Register succeeds, then the user holds exactly [R] and last_active_role is R (the returned access token's role claim is R)** `[服务+mock仓储]` — 注册必须至少一个角色，且首个角色成为激活角色  
  <sub>TestService_Register_Success (service_test.go:44,59); repository.go Create:70-71</sub>
- [ ] **given a display_name with surrounding whitespace (e.g. '  Alice  '), when Register succeeds, then the stored display_name is trimmed ('Alice')** `[纯函数]`  
  <sub>TestService_Register_Success (service_test.go:40); validateDisplayName (service.go:536); TestValidateDisplayName (service_test.go:141)</sub>
- [ ] **given a display_name that is whitespace-only (or empty), when validateDisplayName/Register runs, then it returns ErrInvalidDisplayName** `[纯函数]` — binding 层 min=1 只能拦原始长度，拦不住 trim 后为空的情况，故服务层必须再校验  
  <sub>TestValidateDisplayName (service_test.go:116-117); TestService_Register_DisplayNameValidation (service_test.go:151); validateDisplayName:537-539</sub>
- [ ] **given a display_name containing '<' or '>' anywhere (e.g. '<script>', 'a<b', 'a>b'), when validateDisplayName/Register runs, then it returns ErrDisplayNameForbiddenChars** `[纯函数]` — 禁 < > 防 XSS 标签；这是高可见字段的额外防护带（输出转义+CSP+nosniff 仍是后端保证）  
  <sub>TestValidateDisplayName (service_test.go:118-120); service.go displayNameForbiddenChars:528, validateDisplayName:540-542</sub>
- [ ] **given a display_name containing a control rune (NUL \x00, newline \n, or any unicode control), when validateDisplayName runs, then it returns ErrDisplayNameForbiddenChars** `[纯函数]` — NUL 会让 Postgres 报编码错(500)；控制字符会破坏渲染。服务层拦下以免 500 泄露内部错误  
  <sub>TestValidateDisplayName (service_test.go:121-122); validateDisplayName:543-547</sub>
- [ ] **given a display_name containing a Unicode format (Cf) rune (zero-width space ​, BOM ﻿, bidi override ‮), when validateDisplayName runs, then it returns ErrDisplayNameForbiddenChars** `[纯函数]` — Cf 字符能通过 TrimSpace 却让昵称视觉空白/错乱或做 bidi 攻击。Rust 侧需用等价的 Unicode Cf 分类判断，不能只靠 trim  
  <sub>TestValidateDisplayName (service_test.go:123-125); validateDisplayName:544 (unicode.Is(unicode.Cf,r))</sub>
- [ ] **given legitimate names containing apostrophe, double-quote, or ampersand (O'Brien, Tom&Jerry, a"b), when validateDisplayName runs, then they are accepted unchanged** `[纯函数]` — ' " & 在没有 < > 时构不成标签，是合法昵称的一部分（O'Brien、Tom&Jerry），不能误杀  
  <sub>TestValidateDisplayName (service_test.go:134-138); TestService_Register_DisplayNameValidation O'Brien (service_test.go:160)</sub>
- [ ] **given a display_name with non-ASCII letters (e.g. Chinese '新昵称', '他/她'), when validateDisplayName runs, then it is accepted unchanged** `[纯函数]` — 多语言昵称合法；校验不能是 ASCII-only  
  <sub>TestValidateDisplayName (service_test.go:134)</sub>
- [ ] **given a forbidden display_name (e.g. '<script>'), when Register is called, then it surfaces ErrDisplayNameForbiddenChars (validation is wired into the register path, not only the standalone validator)** `[服务+mock仓储]` — 确认 Register 实际调用了校验，而非只在 UpdateDisplayName  
  <sub>TestService_Register_DisplayNameValidation (service_test.go:156); service.go Register:189-192</sub>
- [ ] **given a phone already held by an existing account, when Register is called with that phone (even with a different email), then it returns ErrPhoneTaken and no second user is created** `[契约·真库]` — phone 部分唯一索引冲突；错误须区分 phone 字段以便前端定位  
  <sub>TestService_Register_DuplicatePhone (service_test.go:165); repository.go Create:53-56 (pg 23505 constraint name contains 'phone')</sub>
- [ ] **given an email already registered, when Register is called with the same email in different case (e.g. 'DUP' vs 'dup') and a different phone, then it still returns ErrEmailTaken** `[契约·真库]` — lower(email) 部分唯一索引→大小写不敏感冲突；应用层小写化+索引共同保证  
  <sub>TestService_Register_DuplicateEmail (service_test.go:178-190); users migration users_email_key on lower(email)</sub>
- [ ] **given two email-only accounts (both with NULL phone), when both Register, then they do NOT conflict on phone (the partial unique index only covers rows WHERE phone IS NOT NULL)** `[契约·真库]` — 部分唯一索引：多个'无手机'账号不算冲突。这是可空标识设计的关键，必须验证 NULL 不参与唯一性  
  <sub>reference doc section 1 users_phone_unique 'WHERE phone IS NOT NULL'; users migration:24; inferred from partial-index semantics (no direct go test — mark inferred)</sub>
- [ ] **given two phone-only accounts (both with NULL email), when both Register, then they do NOT conflict on email (partial unique index covers only rows WHERE email IS NOT NULL)** `[契约·真库]` — 同上，镜像到 email。验证 NULL email 不参与 lower(email) 唯一性  
  <sub>reference doc section 1 users_email_unique 'WHERE email IS NOT NULL'; users migration:27; inferred from partial-index semantics (mark inferred)</sub>
- [ ] **given a direct insert with both phone and email NULL (bypassing the service guard), when the row is written, then the DB rejects it via users_phone_or_email_present CHECK** `[契约·真库]` — DB 边界是最后防线，即使服务层 guard 被绕过也拒绝无标识账号  
  <sub>reference doc section 1/6; users migration:20 CONSTRAINT users_phone_or_email_present</sub>
- [ ] **given a registration, when Create runs, then the user row, its user_roles row, and the matching role profile are written in a single transaction — a failure at any step leaves NO user persisted (never half-built)** `[契约·真库]` — 全有或全无写入：杜绝残缺账号（用户存在但无角色/无 profile）  
  <sub>repository.go Create:38-73 (tx Begin/Commit, addRoleTx); reference doc section 0 '全有或全无写入'</sub>
- [ ] **given the repository returns a non-conflict error (e.g. DB down), when Register is called, then that error is propagated (wrapped) and not masked as success or as a validation error** `[服务+mock仓储]` — 底层错误不能被吞成成功或误判为已存在  
  <sub>TestService_Register_PropagatesStoreError (service_test.go:192); service.go Register:206-208</sub>
- [ ] **given a register request whose phone is present but shorter than 5 or longer than 20 chars, when the handler binds it, then binding fails with 400 before the service runs** `[handler层]` — phone 长度 5–20 在 binding 层（handler）。Rust 侧需在请求校验层等价实现 min=5,max=20 且 optional  
  <sub>handler.go registerRequest:90 (binding:omitempty,min=5,max=20); reference doc section 6 'phone 校验 长度 5–20'</sub>
- [ ] **given a register request whose email is present but not a valid email format, when the handler binds it, then binding fails with 400 before the service runs** `[handler层]` — email 格式在 binding 层校验（optional 但若给出必须合法）  
  <sub>handler.go registerRequest:91 (binding:omitempty,email)</sub>
- [ ] **given a register request whose password is absent, shorter than 8, or longer than 72 bytes, when the handler binds it, then binding fails with 400** `[handler层]` — 密码 8–72：下限是弱口令防护，上限对齐 bcrypt 72 字节上界（避免落到服务层再报 bcrypt 错）  
  <sub>handler.go registerRequest:92 (binding:required,min=8,max=72)</sub>
- [ ] **given a register request whose display_name is absent, empty, or longer than 50 chars, when the handler binds it, then binding fails with 400** `[handler层]` — display_name 1–50 在 binding 层；trim-to-blank 由服务层补充拦截  
  <sub>handler.go registerRequest:93 (binding:required,min=1,max=50); reference doc section 6</sub>
- [ ] **given a register request whose role is absent or not exactly 'student'/'teacher' (e.g. 'admin'), when the handler binds it, then binding fails with 400 — admin self-registration is blocked at the binding tag** `[handler层]` — admin 是独立身份域，禁止通过 web 注册自助获取；在 handler binding 层拦截（服务层现在把 admin 视为无效 web 角色，但 handler 是第一道闸）  
  <sub>handler.go registerRequest:94 (binding:required,oneof=student teacher); service_test.go:216-217 comment</sub>
- [ ] **given the service returns ErrMissingIdentifier, when the register handler runs, then it responds 400 (not 500)** `[handler层]` — '手机或邮箱二选一'是客户端可纠正的输入错误，须映射 400  
  <sub>handler.go Register:117-119</sub>
- [ ] **given the service returns ErrInvalidDisplayName or ErrDisplayNameForbiddenChars, when the register handler runs, then it responds 400** `[handler层]` — trim-to-blank 与禁用字符都是可纠正输入错误  
  <sub>handler.go Register:120-125</sub>
- [ ] **given an identifier value, when the service classifies it, then presence of '@' means email (normalized lowercased+trimmed) and absence means phone (trimmed)** `[纯函数]` — 单字段承载两种标识；分类规则必须一致地驱动查找/规范化。Rust 侧需同样以 '@' 判定，避免大小写/空白导致查不到账号  
  <sub>service.go isEmail:687, normalizeIdentifier:690, lookupByIdentifier:659; TestNormalizeIdentifier (service_test.go:960)</sub>
- [ ] **given a registered user is serialized in a response, when the auth response is built, then password_hash is never included in the JSON** `[handler层]` — password_hash 绝不出现在任何响应中  
  <sub>model.go User.PasswordHash json:"-" (model.go:46)</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **given the service returns ErrPhoneTaken or ErrEmailTaken, when the register handler runs, then it responds 409 with a machine-readable 'field' key ('phone' or 'email'), not a substring-matchable English message** `[handler层]` — 客户端据 'field' 确定性标注冲突输入，措辞变更不影响解析  
  <sub>handler.go Register:129-134 (field key); mvp=false: 'field' response shape是增强，MVP 可先返回 409 但保留字段区分</sub>
- [ ] **given a user with only one of phone/email, when serialized, then the absent identifier is omitted rather than emitted as null or an empty string** `[handler层]` — 保持 `phone?: string` / `email?: string` 的“可选但非 null”契约，避免客户端同时兼容缺省、`null` 和空串
  <sub>model.go User.Phone/Email json omitempty (model.go:44-45); mvp=false: response-shape polish</sub>

> 注：Scope: I kept strictly to Registration + identity + input validation. Login/code/reset/delete/bind/refresh/role flows appear in the same service_test.go but belong to OTHER concerns (auth-login, password-reset, account-deletion, contact-bind, sessions, roles) — I deliberately excluded them except where they share the exact validation rule (e.g. classifyContact mirrors register's phone 5–20 / email-format rule; the identifier-classification assertion overlaps with the login concern and can be de-duped there).

Layer notes:
- pure-fn assertions (validateDisplayName, normalizeEmail/Phone, isEmail/classify) are the highest-value first tests — the go team tests validateDisplayName directly (TestValidateDisplayName) and pins register wiring with ONE representative case each. Recommend the Rust port do the same: exhaustive table test on the pure validator + one wiring test in Register.
- 'contract' assertions need a real Postgres to exercise the partial-unique indexes and the users_phone_or_email_present CHECK. Two of them (multiple-no-phone / multiple-no-email accounts not conflicting) have NO direct go test — they are inferred from the partial-index WHERE clause in the reference doc + migration and are marked as inferred in the `why`. They matter because the whole 'phone optional' redesign hinges on NULLs not colliding; worth an explicit contract test in Rust.
- The duplicate-phone/email conflict mapping lives in the go repository (pg error 23505 → constraint-name substring → ErrPhoneTaken/ErrEmailTaken). In Rust/sqlx you must replicate this mapping from the unique-index name (users_phone_key / users_email_key per the Rust migration) — the constraint names differ from go's, so the substring/name match logic must be re-derived against the Rust migration names, not copied.

MVP flags per reference doc section 8:
- Everything here is MVP=true EXCEPT: the 409 'field' machine-readable response shape and the omitempty response polish (mvp=false — response-shape enhancements, not core register correctness). avatar/teacher_profiles/other OTP purposes don't intersect this concern. All core register + validation logic is first-week MVP.

Go-vs-Rust divergences to watch:
1. bcrypt 72-byte cap exists in both ecosystems — the Rust `bcrypt` crate also truncates/errors; the over-long-password assertion must be re-verified against whichever Rust crate is chosen (some silently truncate at 72 instead of erroring — that would be a behavior change worth a deliberate decision).
2. Unicode Cf/control detection: go uses unicode.IsControl + unicode.Is(unicode.Cf). Rust needs the `unicode-general-category` (or equivalent) crate or a hand-rolled range check; a naive char::is_control() alone will MISS the Cf runes (zero-width space, BOM, bidi override) that the go test explicitly requires rejecting.
3. Password length binding (8–72) is at the handler in go; in Rust decide whether it's request-DTO validation (validator crate) or service-layer — the assertion is tagged handler to match go, but the MVP may fold it into the service if there's no separate binding layer yet.

Open question for the synthesizer: the go handler enforces phone min=5 at the binding layer, while the SERVICE only checks 'non-empty after trim' for phone (no length check in Register itself — length 5–20 is only enforced in classifyContact for the bind flow and in the register binding tag). If the Rust design has no gin-style binding layer, the phone 5–20 check must be moved INTO the service Register (currently it would be un-enforced there). Flag this — it's a real gap if the request layer is thinner in Rust.

## B. 登录 + 账号状态 + 角色 + 隐私

**MVP：**

- [ ] **given an active user registered with email 'login@example.com'; when LoginPassword is called with identifier 'LOGIN@example.com' (different case) and the correct password; then it succeeds and returns the same user, a valid access token whose subject is that user id, and a non-empty refresh token.** `[服务+mock仓储]` — email lookup must normalize (lowercase+trim) before matching the case-insensitive unique index, or a legitimate user is locked out.  
  <sub>TestService_LoginPassword_Success (service_test.go:223)</sub>
- [ ] **given an active user registered with phone '13800138000'; when LoginPassword is called with that phone (an identifier with no '@') and the correct password; then it succeeds.** `[服务+mock仓储]` — identifier without '@' must route to phone lookup, not email.  
  <sub>TestService_LoginPassword_Success phone branch (service_test.go:248)</sub>
- [ ] **given a freshly registered single-role (student) user; when LoginPassword succeeds; then the returned access token's role claim equals that role (student).** `[服务+mock仓储]` — the access token must be scoped to the active role so downstream authz is correct.  
  <sub>TestService_LoginPassword_Success (service_test.go:240)</sub>
- [ ] **given an active registered user; when LoginPassword is called with the correct identifier but a wrong password; then it returns the generic invalid-credentials error (never a distinct 'wrong password' vs 'no such account' signal).** `[服务+mock仓储]` — error must be indistinguishable so an attacker cannot tell whether the identifier is registered vs the password was wrong.  
  <sub>TestService_LoginPassword_WrongPassword (service_test.go:253); service.go:225-227</sub>
- [ ] **given no account for an identifier; when LoginPassword is called with an unknown email OR an unknown phone; then it returns the SAME generic invalid-credentials error as a wrong password, and never performs a password comparison it could time-differentiate.** `[服务+mock仓储]` — wrong-identifier and wrong-password must be indistinguishable to avoid leaking which phones/emails are registered (account enumeration).  
  <sub>TestService_LoginPassword_UnknownIdentifier (service_test.go:264); service.go:216-218</sub>
- [ ] **given a registered user; when the password is compared; then a constant-time hash comparison (bcrypt verify) is used rather than a plaintext/string equality check.** `[纯函数]` — constant-time compare avoids a timing side-channel that would leak password correctness; grounded in the service comment and bcrypt usage.  
  <sub>service.go:223-227 (comment 'Constant-time comparison'); bcrypt.CompareHashAndPassword</sub>
- [ ] **given the store returns a non-NotFound error (e.g. DB down) during identifier lookup; when LoginPassword is called; then that underlying error is propagated (not swallowed into the generic invalid-credentials error).** `[服务+mock仓储]` — a real infrastructure failure must surface as an error, not be masked as 'bad credentials', which would hide outages and corrupt metrics.  
  <sub>TestService_LoginPassword_PropagatesUnexpectedStoreError (service_test.go:276); service.go:219-221</sub>
- [ ] **given a registered user whose status is disabled; when LoginPassword is called with the CORRECT password; then it returns the distinct account-disabled error (not success, not the generic credentials error), and the disabled check happens only after the password verifies.** `[服务+mock仓储]` — credentials are valid so a 403-style disabled signal is appropriate; but ordering the check after password verify is required so a wrong password never reveals disabled state (see next assertion).  
  <sub>TestService_LoginPassword_Disabled (service_test.go:427); service.go:230-234</sub>
- [ ] **given a disabled user; when LoginPassword is called with a WRONG password; then it returns the generic invalid-credentials error, never the account-disabled error.** `[服务+mock仓储]` — revealing 'account disabled' to someone who does not know the password would leak both account existence and its disabled state; the disabled check must sit behind a successful password compare.  
  <sub>TestService_LoginPassword_Disabled wrong-password branch (service_test.go:440); service.go:225-234</sub>
- [ ] **given an active user and a login code previously issued to their (normalized) identifier via RequestLoginCode; when LoginCode is called with that identifier and the correct code; then it succeeds, returns the matching user, and a valid access token.** `[服务+mock仓储]` — verification-code login is a first-class login path (purpose='login').  
  <sub>TestService_LoginCode_Success (service_test.go:287); service.go:247-266</sub>
- [ ] **given a login code that was just consumed by a successful LoginCode; when LoginCode is called again with the same code; then it returns the generic invalid-credentials error.** `[服务+mock仓储]` — an OTP is single-use; a consumed code must be indistinguishable from a wrong code (same generic error).  
  <sub>TestService_LoginCode_Success reuse branch (service_test.go:311-314); service.go:256-258</sub>
- [ ] **given an active user with a login code outstanding; when LoginCode is called with a wrong code; then it returns the generic invalid-credentials error.** `[服务+mock仓储]` — wrong code must map to the same generic error as expired/consumed/unregistered, per the error-indistinguishability rule.  
  <sub>TestService_LoginCode_WrongCode (service_test.go:317); service.go:256-258</sub>
- [ ] **given no account for the identifier; when LoginCode is called; then it returns the generic invalid-credentials error and returns BEFORE calling the code Verify step (identifier lookup fails first).** `[服务+mock仓储]` — an unregistered identifier must yield the same generic error and must not even reach code verification, so timing/behavior cannot distinguish 'no account' from 'wrong code'.  
  <sub>TestService_LoginCode_UnknownUser (service_test.go:328); service.go:248-254</sub>
- [ ] **given code login; when the code is wrong, OR expired, OR already consumed, OR the identifier is unregistered; then all four cases return the identical generic invalid-credentials error, distinguishable by neither error value nor which step failed.** `[服务+mock仓储]` — core privacy rule from reference section 0: 'code wrong / expired / consumed / phone-not-registered' must all be one error so registration status of an identifier never leaks.  
  <sub>reference doc section 0 & section 4; service.go:249-258; fake Codes returns ErrInvalidCredentials for wrong/consumed (fake_test.go:439)</sub>
- [ ] **given a disabled user with a valid login code; when LoginCode is called with the correct code; then it returns the account-disabled error (not success), checked only after the code verifies.** `[服务+mock仓储]` — a valid code must not unlock a disabled account; disabled is a distinct signal but only revealed once code proves the caller controls the identifier.  
  <sub>TestService_LoginCode_Disabled (service_test.go:445); service.go:260-263</sub>
- [ ] **given a login code issued to a normalized target; when LoginCode is called with a differently-cased/whitespaced identifier; then the code is verified against the SAME normalized target (email lowercased+trimmed, phone trimmed) that RequestLoginCode used.** `[服务+mock仓储]` — request and verify must normalize identically or a legitimate code never matches; also prevents a target-mixup bypass.  
  <sub>service.go:242,256 (normalizeIdentifier on both request and verify); TestNormalizeIdentifier (service_test.go:960)</sub>
- [ ] **given RequestLoginCode is called with any identifier; when it runs; then it dispatches a code request for purpose='login' WITHOUT first checking whether the identifier is registered, so the response is identical for registered and unregistered identifiers.** `[服务+mock仓储]` — requesting a code must not become an oracle for account existence; an unregistered identifier simply gets a code that never resolves at LoginCode time.  
  <sub>service.go:241-243 (RequestLoginCode); reference doc section 0</sub>
- [ ] **given a user with an outstanding refresh token from a prior login; when they log in again (password or code); then a new, distinct refresh token is issued AND the prior refresh token is revoked (a subsequent Refresh with the old token fails as invalid-refresh-token) while the new token still refreshes.** `[服务+mock仓储]` — strict single-device: a new login kicks the old device off after its access TTL; grounded in the Sessions.Issue contract.  
  <sub>TestService_Login_SingleDevice (service_test.go:338); service.go:650 (issue -> sessions.Issue); Sessions doc service.go:150-152</sub>
- [ ] **given a valid refresh token for an active user; when Refresh is called; then it returns a fresh access token (valid, scoped to the user id) and a NEW refresh token distinct from the old one, and the old refresh token is single-use (replaying it fails as invalid-refresh-token).** `[服务+mock仓储]` — refresh rotation is the session mechanism; a replayed rotated token must fail (MVP hard-revoke, no grace).  
  <sub>TestService_Refresh_RotatesAndReSignsAccess (service_test.go:362); service.go:395-416</sub>
- [ ] **given a revoked, expired, or unknown refresh token (including one revoked by a login on another device); when Refresh is called; then it returns session.ErrInvalidRefreshToken and does not mint any token.** `[服务+mock仓储]` — all invalid-refresh inputs collapse to one error so the handler clears the cookie and returns 401 without leaking why.  
  <sub>service.go:396-399; TestService_Login_SingleDevice old-device branch (service_test.go:353)</sub>
- [ ] **given a user who added the teacher role and switched to it (last_active_role=teacher persisted); when Refresh is called with their refresh token; then the new access token's role claim is teacher, NOT the default/first role — the prior switch is not silently reset.** `[服务+mock仓储]` — migration 000004 regression: refresh must resume users.last_active_role from the DB, or every refresh silently revokes the user's role switch; the refresh token itself carries no role.  
  <sub>TestService_Refresh_PreservesActiveRole (service_test.go:387); service.go:411 activeRole(u)</sub>
- [ ] **given a user whose status was flipped to disabled AFTER a valid refresh token was issued; when Refresh is called with that still-valid token; then it returns session.ErrInvalidRefreshToken (the same error as any invalid token), rather than succeeding.** `[服务+mock仓储]` — a disable must take effect within one access-token TTL even for already-issued refresh tokens; surfacing the generic invalid-refresh error makes the handler clear the cookie and 401 without a distinct 'disabled' leak on the refresh path.  
  <sub>TestService_Refresh_Disabled (service_test.go:462); service.go:401-410</sub>
- [ ] **given Refresh; when the refresh token rotates successfully but the owning user cannot be loaded (store error / user gone); then Refresh returns that error rather than minting an access token for a nonexistent/errored user.** `[服务+mock仓储]` — the access token must reflect the current user record (role, status); minting without a successful load would bypass the disabled check.  
  <sub>service.go:401-404</sub>
- [ ] **given a student-only user; when SwitchRole(teacher) is called; then it returns the role-not-owned error and does NOT persist last_active_role or issue a token.** `[服务+mock仓储]` — a user must not be able to activate a role they do not hold; prevents privilege escalation to teacher.  
  <sub>TestService_SwitchRole first branch (service_test.go:735); service.go:437-443</sub>
- [ ] **given SwitchRole is called with a role value that is not student or teacher (e.g. 'admin' or ''); when invoked; then it returns the invalid-role error before any store call.** `[纯函数]` — admin is a separate identity realm and unknown roles are invalid; validating first avoids a spurious HasRole lookup and blocks activating an out-of-domain role.  
  <sub>service.go:434-436; TestRole_Valid (service_test.go:973)</sub>
- [ ] **given a user who holds the teacher role; when SwitchRole(teacher) is called; then it persists last_active_role=teacher via the store AND returns an access token whose role claim is teacher.** `[服务+mock仓储]` — the switch must be durable (survive refresh) AND immediately reflected in the token; persisting is what migration 000004 requires.  
  <sub>TestService_SwitchRole success branch (service_test.go:743); service.go:444-455</sub>
- [ ] **given a student-only user; when AddRole(teacher) is called; then the teacher role is added, last_active_role is persisted to teacher, and the returned token's role claim is teacher.** `[服务+mock仓储]` — AddRole grants an identity and immediately switches to it, persisting so refresh keeps it.  
  <sub>TestService_Refresh_PreservesActiveRole setup (service_test.go:393); service.go:460-477</sub>
- [ ] **given a user who already holds the student role; when AddRole(student) is called; then it returns the role-taken error and does not re-activate or re-issue.** `[服务+mock仓储]` — the (user_id, role) composite PK forbids duplicate role rows; the service surfaces this as a distinct role-taken error.  
  <sub>TestService_AddRole_Duplicate (service_test.go:756); service.go:464-466; migration user_roles PK (20260709083746_create_user_roles.up.sql:12)</sub>
- [ ] **given AddRole is called with a non-student/teacher role value; when invoked; then it returns the invalid-role error before any store write.** `[纯函数]` — unknown/out-of-domain roles (including admin) must be rejected up front.  
  <sub>service.go:461-463; TestRole_Valid (service_test.go:973)</sub>
- [ ] **given a user whose last_active_role is NULL/empty (never switched); when a token is issued (login/register/refresh); then the active role falls back to the user's default (first) role in stable load order.** `[纯函数]` — NULL last_active_role means 'never switched' and must resolve to the default role, not an empty role claim.  
  <sub>activeRole/defaultRole (service.go:669-685); TestDefaultRole (service_test.go:988)</sub>
- [ ] **given a user whose stored last_active_role is a role they no longer hold; when a token is issued; then the active role falls back to their default role rather than emitting the stale/unheld role.** `[纯函数]` — defensive: a persisted active role that is no longer in the user's role set must not leak into a token; prevents acting as a role that was removed.  
  <sub>activeRole loop (service.go:669-676)</sub>
- [ ] **given a user with an empty role set; when defaultRole is computed; then it returns the empty/zero role (not a panic and not an arbitrary role).** `[纯函数]` — a user holding no identity yet must resolve to empty rather than crash; deterministic.  
  <sub>TestDefaultRole nil/empty cases (service_test.go:988-995); service.go:680-685</sub>
- [ ] **given roles loaded in a stable order [teacher, student]; when defaultRole is computed; then it returns the first element (teacher), deterministically.** `[纯函数]` — role load order must be stable so the fallback active role is deterministic across requests.  
  <sub>TestDefaultRole ordering case (service_test.go:996-999); service.go:679-685</sub>
- [ ] **given Role.Valid; when the value is 'admin', '', or 'Student' (wrong case); then Valid returns false; when it is 'student' or 'teacher'; then Valid returns true.** `[纯函数]` — admin is a separate identity realm (never a web role), role checks are case-sensitive, and empty is not a role.  
  <sub>TestRole_Valid (service_test.go:973); model.go:22-24</sub>
- [ ] **given a real repo+DB, a user who added the teacher role; when SetActiveRole(teacher) then GetByID; then the loaded user's last_active_role equals teacher (round-trips through the users.last_active_role column).** `[契约·真库]` — role-switch durability depends on the DB column actually persisting and being read back; the CHECK(student/teacher) constraint must accept the value.  
  <sub>contract_test.go:235-254 ('set active role persists'); migration users.last_active_role (20260709075649_create_users.up.sql:12)</sub>
- [ ] **given a real repo+DB; when a user is created via Create(student) and re-read; then status defaults to active (DB column default) and last_active_role is student (set at create time).** `[契约·真库]` — the service's disabled-account and role logic assume the DB column defaults exactly (active status, initial active role); a divergent default would break every login-boundary check.  
  <sub>contract_test.go:83-88; migration users.status default (20260709075649_create_users.up.sql:9)</sub>
- [ ] **given a real repo+DB, a student user; when AddRole(teacher) then HasRole(teacher); then HasRole returns true and the user's loaded role set contains both student and teacher; a second AddRole(teacher) returns the role-taken error.** `[契约·真库]` — role membership drives SwitchRole authorization; the composite PK must enforce no-duplicate at the DB boundary and surface as role-taken.  
  <sub>contract_test.go:214-233; repository_integration_test.go:81-90; migration user_roles PK</sub>
- [ ] **given a real repo+DB, an active user; when SetStatus(disabled) then GetByID; then status is disabled; SetStatus on a nonexistent user id returns ErrNotFound.** `[契约·真库]` — the login/refresh disabled-rejection paths depend on the status column persisting exactly and on a missing-user write being a clean ErrNotFound; CHECK(active/disabled) must accept 'disabled'.  
  <sub>contract_test.go:467-478; migration users.status CHECK (20260709075649_create_users.up.sql:9)</sub>
- [ ] **given a login request for a disabled account with correct credentials; when the handler runs; then the account-disabled service error maps to HTTP 403 (distinct from 401 for invalid credentials).** `[handler层]` — disabled is a valid-credentials-but-locked case that should surface as 403 to the client, while invalid credentials stay 401 — the two error types must not collapse at the boundary.  
  <sub>service.go:28-31 (ErrAccountDisabled doc '403'); ErrInvalidCredentials vs ErrAccountDisabled distinction</sub>
- [ ] **given a refresh request whose owner is now disabled; when the handler runs; then the invalid-refresh-token error maps to 401 and the refresh cookie is cleared (same handling as any invalid refresh token).** `[handler层]` — the refresh path deliberately returns the generic invalid-refresh error for disabled accounts so the handler uniformly clears the cookie and 401s, giving no disabled-state leak on refresh.  
  <sub>service.go:405-410 (Refresh disabled comment)</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **given SwitchRole for a held role; when the store's SetActiveRole fails; then SwitchRole returns that error and does not return a token claiming the new role.** `[服务+mock仓储]` — a token claiming a role that was not durably recorded would be reset on the next refresh, silently reverting the switch — persistence must succeed first.  
  <sub>service.go:446-449 (inferred: no dedicated go test; error path exists in service, mvp per role management being in scope but this specific failure path untested in go)</sub>

> 注：Scope/MVP notes: All assertions here are MVP=true except `switch-role-persist-before-token` (an inferred error-ordering path with no dedicated go test — flagged mvp=false to signal 'nice to have / not go-tested' rather than out-of-scope; the feature itself is in-scope). Everything in this concern (password login, code login, disabled rejection, role switch/add, last_active_role persistence) is inside the reference doc's MVP scope (users, user_roles, verification_codes login purpose, refresh_tokens single-device+revoked_at).

Overlaps with OTHER concerns (avoid double-counting in synthesis):
- The verification-code internals (attempts<=5 cap, TTL/expiry, consumed_at single-use, reject-sampling codegen, cooldown/dailyLimit) belong to the OTP concern. Here I only assert the SERVICE-observable contract: wrong/expired/consumed/unregistered => one generic error. The `fakeCodes` returns ErrInvalidCredentials on wrong/consumed, so service-unit tests exercise indistinguishability but NOT the real expiry/consumed transitions — those need the OTP concern's contract tests.
- Refresh-token rotation grace (rotated_at vs revoked_at, replay-within-window, steal-detection) is the SESSION concern and is explicitly deferred in the reference (MVP = single-device + hard revoke via revoked_at). My refresh assertions assume MVP hard-revoke semantics (replayed rotated token simply fails) — do NOT add grace-window assertions here.
- ResetPassword / DeleteAccount / BindContact and their code-indistinguishability errors (ErrInvalidResetCode, ErrInvalidDeletionCode, ErrInvalidBindCode, ErrChannelUnavailable) share the SAME privacy principle but are separate purposes explicitly DEFERRED in reference section 8 (password_reset, account_deletion, contact_bind). I deliberately did NOT enumerate those here to stay within this concern's login/status/role scope; the synthesizer should route them to a 'password reset / account lifecycle' concern with mvp=false. The one exception worth flagging: ResetPassword and LoginCode share the 'disabled rejected after code check' pattern and the 'valid code + unregistered identifier => generic error' pattern — consistent design, but keep them in their own concern.

Go-vs-Rust divergences:
1. Error types: go uses sentinel errors (ErrInvalidCredentials, ErrAccountDisabled, ErrRoleNotOwned, ErrInvalidRole, session.ErrInvalidRefreshToken). Rust should model these as an enum variant per case; the KEY invariant to preserve is that wrong-password / unknown-identifier / wrong-code / consumed-code / unregistered all map to ONE outward error (InvalidCredentials), while AccountDisabled and (on refresh) InvalidRefreshToken are distinct but ordered AFTER the credential/code check.
2. Enum storage: reference section 7 recommends Rust enum <-> TEXT for role/status. The migration uses TEXT + CHECK(active/disabled) and CHECK(student/teacher) for both users.status, users.last_active_role, and user_roles.role. Rust sqlx mapping must round-trip these and reject unknown values (the CHECK is the DB backstop; Role::valid()/Status parsing is the app-layer guard).
3. last_active_role is NULLable in the Rust schema (matches go's 'NULL = never switched -> default role'). The activeRole fallback (last-active-if-still-held else default-first-role) is pure logic to port verbatim — it is the crux of the 000004 regression fix.
4. Single-device revoke-on-issue and refresh-rotation are delegated to the Sessions trait/service in go; in Rust keep the same seam so the user service can be unit-tested with a fake Sessions. Same for Codes (OTP) trait.

Open questions for synthesizer:
- Whether the Rust handler will keep the exact 403-for-disabled / 401-for-invalid-refresh mapping (I asserted it from the go service doc comments, but there is no go handler_test line cited for these two specific mappings in what I read — treat handler-layer assertions as design intent to confirm against the Rust handler).
- `switch-role-persist-before-token` and `refresh-loads-user-after-rotate` are error-ordering guards inferred from service.go control flow with no dedicated go test; include as design constraints.

## C. 学习设置 + 资料（student/teacher）

**MVP：**

- [ ] **Given a valid CEFR level and English variant, when SetLearningSettings runs, then it constructs LearningSettings{level, variant} and calls the repo once, returning the persisted pair with no error.** `[服务+mock仓储]`  
  <sub>service.go:498-507 SetLearningSettings; contract_test.go:438 'learning settings round-trip for a student'</sub>
- [ ] **Given an unknown/unparseable CEFR level (e.g. "Z9"), when SetLearningSettings runs, then it returns ErrInvalidLearningSettings BEFORE touching the repo (no write attempted).** `[服务+mock仓储]` — Fail-closed on bad enum so an invalid value can never reach the DB or a partial write; keeps validation in the service, not only the CHECK.  
  <sub>service.go:499-501 (!level.Valid()); model.go:87-93 CEFRLevel.Valid; handler_test.go:860 '400 invalid level'</sub>
- [ ] **Given an unknown English variant, when SetLearningSettings runs, then it returns ErrInvalidLearningSettings before touching the repo.** `[服务+mock仓储]` — Same fail-closed enum guard as level; also the second half of the paired write.  
  <sub>service.go:499-501 (!variant.Valid()); model.go:106-108 EnglishVariant.Valid</sub>
- [ ] **Given an empty CEFR level or empty English variant (""), when SetLearningSettings runs, then it returns ErrInvalidLearningSettings — the empty string is not a valid enum and cannot be written.** `[服务+mock仓储]` — Prevents writing one-set-one-empty which would be a 'half-settings' state; the service enforces all-or-nothing before the DB CHECK ever sees it.  
  <sub>service.go:499-501; model.go:87-93,106-108 (empty string not in Valid set)</sub>
- [ ] **Given SetLearningSettings is the only write path, when it persists, then it ALWAYS writes cefr_level and english_variant together (never one alone), so the paired invariant holds by construction, not just by DB CHECK.** `[服务+mock仓储]` — All-or-nothing write (reference doc section 0 '全有或全无写入'): the service API takes both as required args, making a partial write structurally impossible at the app layer.  
  <sub>service.go:502 ls := &LearningSettings{CEFRLevel: level, EnglishVariant: variant}; repository.go:480-493 SetLearningSettings UPDATE sets both</sub>
- [ ] **Given a direct write that would set exactly one of (cefr_level, english_variant) to NULL and the other non-NULL, when it hits the DB, then the student_profiles_learning_settings_paired CHECK ((cefr_level IS NULL) = (english_variant IS NULL)) rejects it.** `[契约·真库]` — DB is the last-line guard against a half-settings row even if a future code path or migration bypasses the service; both-null and both-set both pass, only-one-set fails.  
  <sub>tsz-rust migration 20260709085134_create_student_profiles.up.sql CONSTRAINT student_profiles_learning_settings_paired; reference doc section 3 note</sub>
- [ ] **Given a user with no student_profiles row (e.g. teacher-only account), when SetLearningSettings runs with valid enums, then the repo UPDATE affects 0 rows and the service propagates ErrNoStudentProfile (handler maps to 409).** `[服务+mock仓储]` — Distinguishes 'no place to store settings' from a silent no-op so the caller can 409 rather than pretend success.  
  <sub>repository.go:480-492 (RowsAffected()==0 -> ErrNoStudentProfile); fake_test.go:159-166; contract_test.go:426 'learning settings require a student profile'; handler_test.go:874 '409 teacher-only user has no student profile'</sub>
- [ ] **Given a real DB and a teacher-only user, when SetLearningSettings runs, then it returns ErrNoStudentProfile (UPDATE matches no row because no student_profiles row exists).** `[契约·真库]` — Confirms the RowsAffected==0 mapping against real Postgres, not just the fake.  
  <sub>repository_integration_test.go:160-168 TestRepository_LearningSettings teacher branch</sub>
- [ ] **Given a fresh student who has a student_profiles row but both learning-setting columns NULL, when GetLearningSettings runs, then it returns (nil, nil) — nil is a normal 'not onboarded yet' result, not an error.** `[服务+mock仓储]` — 'onboarded' is DERIVED from whether both columns are set; nil is the not-onboarded signal, so it must not surface as an error.  
  <sub>service.go:490-492; repository.go:459-473 (level==nil||variant==nil -> nil,nil); contract_test.go:444-446; repository_integration_test.go:124-135</sub>
- [ ] **Given a user with no student_profiles row at all (teacher-only), when GetLearningSettings runs, then it returns (nil, nil) — no rows is treated as not-onboarded, not an error.** `[契约·真库]` — Read side must never error just because the user isn't a student; nil keeps the derived onboarded=false path uniform.  
  <sub>repository.go:464-465 (pgx.ErrNoRows -> nil,nil); repository_integration_test.go:169-171</sub>
- [ ] **Given learning settings were written, when GetLearningSettings runs, then it returns the exact (cefr_level, english_variant) pair previously written.** `[契约·真库]`  
  <sub>contract_test.go:448-458; repository_integration_test.go:137-148</sub>
- [ ] **Given a student already onboarded with one pair, when SetLearningSettings runs with a different valid pair, then it overwrites both columns (settings-screen / accent-toggle edit) and a subsequent read returns the new pair.** `[契约·真库]` — Learning settings are editable after onboarding; overwrite must replace both fields together, preserving the paired invariant.  
  <sub>repository.go:480-493 (UPDATE overwrites); repository_integration_test.go:150-158 'overwrite (settings screen / accent toggle)'</sub>
- [ ] **Given the app needs onboarding status, when it is computed, then 'onboarded' is DERIVED as (learning_settings != nil) — there is no separate onboarded boolean column or field anywhere.** `[handler层]` — Reference doc section 3: derive onboarded from whether both settings are set, never add a flag field; single source of truth avoids the flag drifting out of sync with the columns.  
  <sub>handler.go:514 "onboarded": settings != nil; model.go:110-117 (no onboarded field); reference doc section 3 note</sub>
- [ ] **Given a freshly registered student, when GET /me is handled, then the response has learning_settings: null and onboarded: false.** `[handler层]`  
  <sub>handler.go:504-515 Me; handler_test.go:792-798 TestHandler_Me 'found'</sub>
- [ ] **Given a student who has completed learning settings, when GET /me is handled, then the response has onboarded: true and learning_settings reflecting the saved pair.** `[handler层]`  
  <sub>handler.go:510-515; handler_test.go:834-857 '200 sets settings and flips onboarded' (Me sub-assertion)</sub>
- [ ] **Given valid {cefr_level, english_variant}, when PUT/POST learning-settings is handled, then it responds 200 with {learning_settings:{...}, onboarded:true}.** `[handler层]`  
  <sub>handler.go:527-549 UpdateLearningSettings; handler_test.go:834-846 '200 sets settings and flips onboarded'</sub>
- [ ] **Given a request missing english_variant (or cefr_level), when the learning-settings handler binds it, then request binding rejects it with 400 before the service runs (both fields are binding:required).** `[handler层]` — Handler-layer all-or-nothing: the request contract itself forbids sending only one field, so a half-update can't even be expressed.  
  <sub>handler.go:518-521 learningSettingsRequest binding:required,oneof; handler.go:531-534; handler_test.go:867 '400 missing variant'</sub>
- [ ] **Given a cefr_level/english_variant outside the allowed set, when the handler binds it, then binding's oneof validator returns 400 (a second guard in front of the service's ErrInvalidLearningSettings).** `[handler层]` — Handler rejects bad enums early with 400; the service's ErrInvalidLearningSettings is the defensive backstop for direct/service callers.  
  <sub>handler.go:519-520 binding oneof=A1..C2 / BrE AmE; handler_test.go:860-865 '400 invalid level'</sub>
- [ ] **Given an authenticated teacher-only user, when the learning-settings handler runs, then ErrNoStudentProfile is mapped to HTTP 409 (not 400/500).** `[handler层]` — Distinct status lets the client tell 'you're not a student' apart from a validation error.  
  <sub>handler.go:538-539; handler_test.go:874-880 '409 teacher-only user has no student profile'</sub>
- [ ] **Given a user is created with (or later granted) the student role, when the role is written, then an empty student_profiles row (grade='', both settings NULL) is inserted in the same transaction.** `[契约·真库]` — Guarantees a student always has a profile row to hold settings, so SetLearningSettings' UPDATE has a target; the empty NULL/NULL row is the not-onboarded starting state.  
  <sub>repository.go:236-247 addRoleTx (INSERT INTO student_profiles); migration student_profiles grade DEFAULT '' + nullable settings</sub>
- [ ] **Given a student_profiles row is inserted with only user_id, when read back, then grade is '' (NOT NULL DEFAULT '').** `[契约·真库]` — grade is required-non-null but starts empty; no NULL grade state exists.  
  <sub>tsz-rust migration 20260709085134 grade TEXT NOT NULL DEFAULT ''; repository.go:239 INSERT with only user_id</sub>
- [ ] **Given student_profiles.user_id is the PRIMARY KEY referencing users(id), when a second profile insert is attempted for the same user, then it is rejected (a user has at most one student profile — 1:1 with users).** `[契约·真库]` — user_id-as-PK enforces exactly-one-profile-per-user; prevents duplicate/ambiguous settings rows.  
  <sub>tsz-rust migration 20260709085134 user_id PRIMARY KEY REFERENCES users(id); reference doc section 3</sub>
- [ ] **Given a student (or teacher) with a profile row, when the user is deleted, then the profile row is removed via ON DELETE CASCADE (no orphaned profile), and re-creating a user with the freed phone/email succeeds.** `[契约·真库]` — Account deletion must not leave dangling profile rows; cascade keeps referential integrity and frees the unique identifiers.  
  <sub>repository.go:207-221 Delete + cascade comment; tsz-rust migration ON DELETE CASCADE; contract_test.go:281-308 'delete removes the user and cascades'</sub>
- [ ] **Given the repository returns a non-ErrNoRows error, when GetLearningSettings runs, then the service/handler propagates it (Me returns 500), never masking it as onboarded=false.** `[服务+mock仓储]` — A real query failure must not be silently reported as 'not onboarded', which would corrupt the derived flag.  
  <sub>repository.go:467-469; handler.go:505-508 (internalError on err)</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **Given the go reference, when scanning service/handler code, then there is NO service method to read or update grade (it is only default-initialized) — a Rust grade update path would be net-new, not a port.** `[服务+mock仓储]` — Marks that grade mutation is unimplemented in go; avoids inventing an assertion with no basis. Include only if the Rust design adds a grade endpoint.  
  <sub>service.go (no grade method); repository.go (grade only in INSERT default, never UPDATEd)</sub>
- [ ] **Given a user is created with or granted the teacher role, when the role is written, then an empty teacher_profiles row (bio='', verified=false) is inserted in the same transaction.** `[契约·真库]` — Deferred per reference doc section 8 (teacher_profiles postponed). Row-creation parity only; bio/verified have no read/write path in go.  
  <sub>repository.go:240-241 addRoleTx INSERT INTO teacher_profiles; tsz-rust migration 20260709085817 bio DEFAULT '' verified DEFAULT false</sub>
- [ ] **Given a teacher_profiles row inserted with only user_id, when read back, then bio='' and verified=false (both NOT NULL with defaults).** `[契约·真库]` — Deferred (reference doc section 8). No teacher CHECK pairing needed since it has no learning settings.  
  <sub>tsz-rust migration 20260709085817 bio TEXT NOT NULL DEFAULT '', verified BOOLEAN NOT NULL DEFAULT false</sub>
- [ ] **Given the go reference, when searching for bio/verified mutation, then there is NO service or handler method to set bio or flip verified — verified is admin-domain and unimplemented in the user service.** `[服务+mock仓储]` — Deferred/admin (reference doc section 8, section 0 双身份分离). Documents that any Rust bio/verified endpoint is net-new, not a port; do not invent behavior.  
  <sub>service.go/handler.go/repository.go (grep bio/verified: no setter); reference doc section 8</sub>
- [ ] **Given teacher_profiles.user_id is PRIMARY KEY referencing users(id), when a second teacher profile insert for the same user is attempted, then it is rejected (1:1 with users).** `[契约·真库]` — Deferred (reference doc section 8) but same 1:1 structural rule as student profile.  
  <sub>tsz-rust migration 20260709085817 user_id PRIMARY KEY REFERENCES users(id)</sub>

> 注：Grounding: learning-settings behavior is best-covered in tsz-go's contract_test.go (fake+real, 'learning settings require a student profile' / 'round-trip'), repository_integration_test.go:TestRepository_LearningSettings, and handler_test.go:TestHandler_UpdateLearningSettings + TestHandler_Me. There are NO dedicated service_test.go cases for learning settings — the service-unit assertions are grounded in service.go logic + the fakeStore behavior it's exercised through, plus the enum-validation the handler tests drive end-to-end.

Key design facts for the Rust port:
- The service's SetLearningSettings API takes BOTH level and variant as required args and constructs the pair, so the all-or-nothing invariant is enforced structurally at the app layer; the DB CHECK (student_profiles_learning_settings_paired) is defense-in-depth and, in the go write path, is never actually exercised into failure. I split these into a service-unit assertion (set-ls-always-writes-both) and a contract assertion (paired-check-defense-in-depth) — the latter can only be tested by a raw SQL write bypassing the service.
- 'onboarded' is derived (settings != nil) at the handler; there is deliberately no flag field. The repo returns (nil,nil) both for 'no student profile row' and 'row exists but columns NULL' — the service can't distinguish these on read, and that's intentional.
- Student profile rows are AUTO-CREATED inside addRoleTx when the student role is granted (repository.go:236-247), so SetLearningSettings' UPDATE (not upsert) has a target. This is a repo/contract-level fact, not visible in the service unit, but essential: without it every SetLearningSettings would hit ErrNoStudentProfile. Worth calling out to the Rust implementer as a prerequisite of the whole flow.

teacher_profiles (all mvp=false per reference doc section 8): go only ever auto-creates an empty row on role grant and cascades on delete. bio and verified have NO service/handler read or write path (verified belongs to the admin domain, which is a separate identity realm per reference doc section 0). Any Rust bio/verified endpoint is net-new design, not a port — flagged in teacher-profile-no-bio-verified-service and grade-no-service-path so the synthesizer doesn't treat them as existing behavior.

Overlaps with other concerns: profiles-cascade-on-user-delete overlaps with the account-deletion / DeleteAccount concern and the user_roles cascade concern (contract_test.go 'delete removes the user and cascades' also covers roles + refresh tokens). Student-profile-autocreated-on-role overlaps with the role-management concern (AddRole/Register). The 400 binding assertions overlap with the handler-validation concern. De-dup at synthesis: keep the profile-cascade assertion here scoped to the profile row specifically.

Open question for the Rust design: grade has a NOT NULL DEFAULT '' column but no mutation path in go — if the Rust MVP wants an editable grade it needs a net-new assertion (marked in grade-no-service-path). Also: the Rust migration keeps the same nullable-paired settings design as go, so all learning-settings assertions port 1:1.

## D. OTP / 验证码服务

**MVP：**

- [ ] **given generateCode() is called; when it returns; then the result is exactly 6 characters, all ASCII digits 0-9 (zero-padded, e.g. '000000' is valid).** `[纯函数]` — Length is the anti-brute-force denominator: 6 digits = 1e6 space, which combined with the 5-attempt cap keeps online guessing negligible.  
  <sub>otp/service.go:170-189 generateCode; codeDigits const service.go:27; asserted in service_test.go:138-140 TestService_RequestAndVerify</sub>
- [ ] **given the CSPRNG byte stream; when generating each digit; then bytes >= 250 are discarded (rejection sampling) and digit = byte%10 only for accepted bytes, so all ten digits are equiprobable — NOT plain byte%10 over 0-255 which biases 0-5.** `[纯函数]` — 256 is not a multiple of 10; naive byte%10 skews digits 0-5 higher, shrinking effective entropy of the code and helping an attacker. Statistical/property test over many samples should show ~uniform distribution.  
  <sub>otp/service.go:170-189 generateCode (v>=250 reject branch, comment lines 165-169); reference doc section 4 bullet 3</sub>
- [ ] **given code generation; when drawing random bytes; then it uses a cryptographically secure RNG (Rust: OsRng / rand::rngs equivalent of crypto/rand), never a seeded/predictable PRNG.** `[纯函数]` — A predictable RNG lets an attacker precompute or narrow candidate codes, defeating the 1e6 space entirely.  
  <sub>otp/service.go:5 crypto/rand import, service.go:175 rand.Read</sub>
- [ ] **given the RNG source returns an error; when generateCode() runs; then it returns an error (and RequestCode aborts without saving or sending).** `[纯函数]` — Must not emit an empty/partial or non-random code on RNG failure.  
  <sub>otp/service.go:175-177 (rand.Read err), service.go:83-86 RequestCode wraps 'generate code'</sub>
- [ ] **given rate limits pass; when RequestCode(target, purpose) runs; then it generates a code, calls store.Save exactly once with a Code{target, channel=ChannelFor(target), purpose, code, expires_at=now+ttl, consumed_at=null, attempts=0}, THEN calls sender.Send(channel, target, code) exactly once; returns Ok.** `[服务+mock仓储]` — Defines the canonical success ordering; Save-before-Send guarantees a sent code is always persisted/verifiable.  
  <sub>otp/service.go:78-104 RequestCode; service_test.go:128-140 TestService_RequestAndVerify</sub>
- [ ] **given ttl configured; when RequestCode saves a code; then the saved code's expires_at == request_time + ttl (not a hardcoded value).** `[服务+mock仓储]` — Expiry window must be config-driven and correct; drives the expired-code rejection path.  
  <sub>otp/service.go:95 ExpiresAt: time.Now().Add(s.ttl)</sub>
- [ ] **given store.Save returns an error; when RequestCode runs; then it returns an error and sender.Send is NEVER called (no code delivered for a code that was not persisted).** `[服务+mock仓储]` — Avoids delivering a code the user can never redeem, and avoids masking DB failure.  
  <sub>otp/service.go:97-99; service_test.go:246-253 TestService_RequestCode_SaveError</sub>
- [ ] **given store.Save succeeds but sender.Send returns an error; when RequestCode runs; then it returns an error wrapping the send failure.** `[服务+mock仓储]` — Caller must learn delivery failed so it can surface a retry/failure to the user.  
  <sub>otp/service.go:100-102 send code error branch (no dedicated go test; grounded in service.go)</sub>
- [ ] **given a target containing '@'; when ChannelFor(target) is called; then it returns email channel.** `[纯函数]` — Channel is derived, not passed in; wrong channel routes a code to the wrong provider.  
  <sub>otp/sender.go:23-28 ChannelFor; service_test.go:83-90 TestChannelFor</sub>
- [ ] **given a target without '@' (e.g. a phone number); when ChannelFor(target) is called; then it returns sms channel.** `[纯函数]` — Phone targets must route to SMS; the derivation is the only channel source.  
  <sub>otp/sender.go:23-28 ChannelFor; service_test.go:87-89 TestChannelFor</sub>
- [ ] **given a stored unconsumed unexpired code with attempts<5; when Verify(target, purpose, correctCode) runs; then it calls store.MarkConsumed(code.id) and returns Ok.** `[服务+mock仓储]` — Core success path; consumption is what enforces single-use.  
  <sub>otp/service.go:133-162 Verify success; service_test.go:147-150 TestService_RequestAndVerify</sub>
- [ ] **given a code already successfully verified (consumed_at set); when Verify is called again with the same correct code; then it returns ErrInvalidCode (LatestUnconsumed no longer returns it).** `[服务+mock仓储]` — One-time semantics: a code redeemed once must never grant access again (replay prevention).  
  <sub>otp/service.go:134 LatestUnconsumed excludes consumed; service_test.go:151-154 TestService_RequestAndVerify reuse case</sub>
- [ ] **given a valid stored code; when Verify with an incorrect code runs; then it calls store.IncrementAttempts(code.id) exactly once and returns ErrInvalidCode.** `[服务+mock仓储]` — Every wrong guess MUST be counted; if wrong guesses neither consumed nor counted the code, the whole 1e6 space is brute-forceable within the TTL.  
  <sub>otp/service.go:151-157 wrong-code branch; service_test.go:142-145 TestService_RequestAndVerify</sub>
- [ ] **given a code whose attempts have reached maxVerifyAttempts (5); when Verify is called with the CORRECT code; then it returns ErrInvalidCode (the code is locked) and does NOT consume it.** `[服务+mock仓储]` — Anti-brute-force lock: after 5 wrong guesses even the right code is dead, so an attacker gets at most 5 tries per 1e6 code before lockout.  
  <sub>otp/service.go:147-149 attempts>=maxVerifyAttempts branch; service_test.go:159-180 TestService_Verify_AttemptLimit</sub>
- [ ] **given a fresh code; when 4 wrong guesses are made; then the 5th guess (correct or wrong) is still processed against the code, but after 5 recorded wrong attempts the code is locked — i.e. the threshold is exactly attempts>=5, not 4 or 6.** `[服务+mock仓储]` — Off-by-one here changes the attacker's guess budget; the constant 5 (maxVerifyAttempts) is a security parameter to pin down.  
  <sub>otp/service.go:32 maxVerifyAttempts=5, service.go:147; service_test.go:171-175 loop of exactly maxVerifyAttempts guesses</sub>
- [ ] **given a code that is locked from 5 failed attempts; when a NEW code is requested for the same target+purpose and then verified; then the fresh code is accepted (lockout does not carry over to the new code).** `[服务+mock仓储]` — Lockout is scoped to a single code, not to the target — otherwise a legitimate user could be permanently locked out by an attacker's guessing.  
  <sub>otp/service.go comment 147-148 'per-code'; service_test.go:182-188 TestService_Verify_AttemptLimit fresh-code case</sub>
- [ ] **given a stored code whose expires_at is in the past; when Verify with the correct code runs; then it returns ErrInvalidCode and does NOT consume or increment.** `[服务+mock仓储]` — Time-bounding limits the window for guessing/interception; expired codes must be dead.  
  <sub>otp/service.go:142-144 time.Now().After(ExpiresAt); service_test.go:198-211 TestService_Verify_Expired</sub>
- [ ] **given no unconsumed code exists for target+purpose (LatestUnconsumed returns ErrNotFound / unknown target); when Verify runs; then it returns ErrInvalidCode.** `[服务+mock仓储]` — Unknown/unregistered target must be INDISTINGUISHABLE from wrong/expired/consumed — leaking 'no code here' reveals whether a phone/email is registered.  
  <sub>otp/service.go:134-137 ErrNotFound->ErrInvalidCode; service_test.go:191-196 TestService_Verify_NoCode</sub>
- [ ] **given the four failure modes (wrong code, expired, already-consumed, no code for target); when Verify runs; then ALL four return the exact same error (ErrInvalidCode) with identical message — a caller cannot tell them apart.** `[服务+mock仓储]` — Privacy invariant: differentiated errors would let an attacker probe which targets have a pending code or which numbers are registered. This is the single most important security assertion for this concern.  
  <sub>otp/service.go:16-20 ErrInvalidCode doc + 137,144,149,156 all return it; reference doc section 0 '错误不可区分' and section 4</sub>
- [ ] **given a submitted code; when comparing it to the stored code; then a constant-time comparison is used (Rust: subtle/ct-eq equivalent), not a short-circuiting == on strings.** `[服务+mock仓储]` — A non-constant-time compare leaks matched-prefix length via timing, letting an attacker reconstruct the code digit-by-digit and bypass the 1e6 space.  
  <sub>otp/service.go:6 crypto/subtle import, service.go:151 subtle.ConstantTimeCompare (comment line 150)</sub>
- [ ] **given two codes issued for the same target+purpose (first then second, both unconsumed); when Verify is called with the FIRST code; then it returns ErrInvalidCode, and only the SECOND (newest) code verifies successfully.** `[服务+mock仓储]` — Verify operates on the single latest-unconsumed code; issuing a new code must invalidate the old one so a user can't be tricked into confirming a stale/attacker-triggered code.  
  <sub>otp/service.go:134 LatestUnconsumed; service_test.go:213-244 TestService_SecondCodeInvalidatesFirst</sub>
- [ ] **given LatestUnconsumed returns a non-ErrNotFound error (e.g. DB failure), or MarkConsumed/IncrementAttempts return an error; when Verify runs; then it returns a wrapped internal error — NOT ErrInvalidCode (a genuine infra failure must not be masked as a bad-code result).** `[服务+mock仓储]` — Masking DB errors as ErrInvalidCode would hide outages AND could let a code silently escape consumption/attempt-counting (e.g. failed MarkConsumed leaving the code reusable).  
  <sub>otp/service.go:138-140 lookup err wrap, 153-155 record-attempt err, 159-161 consume err</sub>
- [ ] **given cooldown>0 and a code was issued to target+purpose within the cooldown window; when RequestCode is called again for the same target+purpose; then it returns ErrRateLimited and NOTHING is generated, saved, or sent.** `[服务+mock仓储]` — Cooldown bounds SMS/email cost and abuse; must reject BEFORE side effects.  
  <sub>otp/service.go:79-81, 108-118 checkRateLimit cooldown (CountSince over now-cooldown, n>0); service_test.go:92-109 TestService_RequestCode_Cooldown</sub>
- [ ] **given target A is in cooldown; when RequestCode is called for a DIFFERENT target B; then B is not rate-limited (succeeds).** `[服务+mock仓储]` — Rate limits are per-target; one target's cooldown must not deny service to others.  
  <sub>otp/service.go:111 CountSince(target,...); service_test.go:106-108 TestService_RequestCode_Cooldown other-target case</sub>
- [ ] **given dailyLimit=N and N codes already issued to target+purpose within the rolling 24h; when the (N+1)th RequestCode is called; then it returns ErrRateLimited (threshold is count>=dailyLimit) and sends nothing.** `[服务+mock仓储]` — Daily cap bounds per-target abuse/cost over the day even with no active cooldown.  
  <sub>otp/service.go:119-127 daily-limit branch (CountSince over now-24h, n>=dailyLimit); service_test.go:111-126 TestService_RequestCode_DailyLimit</sub>
- [ ] **given the daily cap; when counting prior codes; then only codes issued within the last 24h (created_at >= now-24h) are counted, so codes older than 24h no longer count toward the cap.** `[服务+mock仓储]` — It is a rolling 24h window, not a fixed calendar day; getting the window wrong either over-blocks or lets abuse through.  
  <sub>otp/service.go:120 now.Add(-24*time.Hour)</sub>
- [ ] **given cooldown==0; when RequestCode runs; then the cooldown check is skipped entirely (no CountSince call for cooldown, back-to-back requests allowed by cooldown).** `[服务+mock仓储]` — 0 is the documented disable sentinel; must be honored so tests/configs without cooldown behave as configured.  
  <sub>otp/service.go:110 if s.cooldown>0; service_test.go:114 svc created with cooldown 0</sub>
- [ ] **given dailyLimit==0; when RequestCode runs; then the daily-cap check is skipped entirely (unbounded by daily cap).** `[服务+mock仓储]` — 0 is the documented disable sentinel for the daily cap.  
  <sub>otp/service.go:119 if s.dailyLimit>0; service_test.go:95 svc created with dailyLimit 0</sub>
- [ ] **given both cooldown>0 and dailyLimit>0; when RequestCode runs; then cooldown is evaluated first and, if it passes, the daily cap second; a hit on EITHER returns ErrRateLimited before any generate/save/send.** `[服务+mock仓储]` — Ordering and short-circuit define which limit fires; both must gate side effects.  
  <sub>otp/service.go:108-129 checkRateLimit (cooldown block then daily block)</sub>
- [ ] **given store.CountSince returns an error during a rate-limit check; when RequestCode runs; then it returns a wrapped error and does NOT generate/save/send (fails closed, not open).** `[服务+mock仓储]` — A rate-limit check that errors must not silently allow the request through (fail-open would defeat the limiter).  
  <sub>otp/service.go:112-114 cooldown check err, 121-123 daily-limit check err</sub>
- [ ] **given a code Saved to the store; when LatestUnconsumed(target, purpose) is queried; then it returns the same code (id and code value round-trip intact).** `[契约·真库]` — Basic persistence contract both the fake and the real Postgres repo must satisfy identically.  
  <sub>otp/contract_test.go:37-51 'save then latest-unconsumed round-trips'</sub>
- [ ] **given two unconsumed codes for the same target+purpose (older then newer by created_at); when LatestUnconsumed is queried; then it returns the NEWEST code, matching the (target, purpose, created_at DESC) index.** `[契约·真库]` — The 'latest unconsumed' lookup is what makes a re-issued code supersede the prior one; ordering must be by created_at DESC.  
  <sub>otp/contract_test.go:53-71 'latest-unconsumed returns the newest code'; reference doc section 4 index; rust migration verification_codes_lookup</sub>
- [ ] **given no code exists for target+purpose; when LatestUnconsumed is queried; then it returns ErrNotFound (a distinct sentinel the Service maps to ErrInvalidCode).** `[契约·真库]` — The store distinguishes not-found from other errors; the Service is responsible for collapsing it to the indistinguishable ErrInvalidCode.  
  <sub>otp/service.go:15-16 ErrNotFound; contract_test.go:73-78 'no unconsumed code returns ErrNotFound'</sub>
- [ ] **given a code that has been MarkConsumed; when LatestUnconsumed(target, purpose) is queried; then that code is excluded (returns ErrNotFound if it was the only one).** `[契约·真库]` — consumed_at is the single-use marker; the lookup MUST filter on consumed_at IS NULL or single-use breaks.  
  <sub>otp/contract_test.go:80-93 'consumed codes are excluded'; rust migration consumed_at nullable comment</sub>
- [ ] **given a stored code; when IncrementAttempts is called twice; then a subsequent LatestUnconsumed returns attempts==2 (each call increments by exactly one, durably).** `[契约·真库]` — The attempt counter is the state that drives the anti-brute-force lock; it must persist across reads, not reset.  
  <sub>otp/contract_test.go:95-115 'increment attempts persists'</sub>
- [ ] **given N codes for target A and some for target B within the window; when CountSince(A, purpose, since) is queried; then it returns exactly N (inclusive of created_at>=since, scoped to target A only, B not counted).** `[契约·真库]` — Rate limiting correctness depends on CountSince counting only the given target and including the boundary; over/under-counting mis-applies limits.  
  <sub>otp/contract_test.go:117-139 'count since is inclusive and scoped to target'</sub>
- [ ] **given codes exist for a target; when CountSince(target, purpose, since=now+1min) is queried (window opening in the future); then it returns 0.** `[契约·真库]` — Confirms the >=since boundary excludes everything for a future window; guards against inverted comparisons.  
  <sub>otp/contract_test.go:140-147 future-window case</sub>
- [ ] **given the same Store contract suite; when run against BOTH the in-memory fake and the real Postgres repository; then both pass identically (the fake never diverges from DB behavior).** `[契约·真库]` — Service unit tests rely on the fake; if the fake drifts from the real repo, green unit tests hide real bugs. In Rust, mirror this with one shared contract test over both impls.  
  <sub>otp/contract_test.go:12-15 doc + 151-155 TestStoreContract_Fake (Postgres counterpart repository_integration_test.go)</sub>
- [ ] **given the Sender trait; when unit-testing the service; then a mock Sender is injected that records the last code per target (LastCode) and sends nothing externally; the real SMS/email provider is only wired at composition root.** `[服务+mock仓储]` — Tests must observe issued codes without external side effects; delivery is abstracted so provider swaps don't touch call sites.  
  <sub>otp/sender.go:38-40 Sender interface, 46-75 MockSender/LastCode; used throughout service_test.go</sub>
- [ ] **given purpose='login'; when RequestCode/Verify run; then codes are issued, looked up, and verified scoped by (target, purpose) so a login code cannot satisfy a different-purpose verification.** `[服务+mock仓储]` — purpose scoping prevents cross-purpose code reuse; login is the only MVP purpose.  
  <sub>otp/service.go:78,133 purpose threaded through Save/LatestUnconsumed/CountSince; reference doc section 8 '先 login 一个 purpose'</sub>
- [ ] **given a code issued for (target, 'login'); when Verify(target, 'password_reset', code) is called; then LatestUnconsumed('password_reset') does not return the login code and Verify returns ErrInvalidCode.** `[服务+mock仓储]` — Purpose is a scoping dimension: a code proven for one purpose must not authorize another (e.g. a login OTP must not delete an account).  
  <sub>otp/service.go:134 LatestUnconsumed(target,purpose); contract_test.go:117-120 note that schema/lookup scope by purpose (behavior is purpose-scoped even though MVP only exercises login)</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **given a REAL Sender implementation; when it logs for diagnostics; then it MUST NOT log the code (a live credential) and MUST NOT log the raw target (PII) — targets are masked (e.g. '138****1234', 'a***@example.com'). The mock may log both since it is dev/test-only.** `[服务+mock仓储]` — Codes in logs are exploitable credentials; raw phone/email in logs is a PII leak. Rust real Sender must follow this even though the mock does not.  
  <sub>otp/sender.go:31-37 logging note, 60-66 MockSender logs 'because dev/test only'. mvp=false: real provider integration is post-MVP; the mock is what MVP ships.</sub>
- [ ] **given purposes password_reset / account_deletion / contact_bind; when scoping MVP; then these are valid schema values (CHECK allows them) but their end-to-end flows are NOT implemented in MVP.** `[服务+mock仓储]` — Schema CHECK already permits all four for forward-compat, but the reference doc defers three purposes; listing so they aren't forgotten. mvp=false per reference doc section 8.  
  <sub>reference doc section 4 purpose evolution + section 8 暂缓; rust migration purpose CHECK IN ('login','password_reset','account_deletion','contact_bind')</sub>

> 注：Grounding: all assertions trace to tsz-go/internal/otp/{service.go, service_test.go, sender.go, contract_test.go} and the reference doc; Rust schema (migrations/20260709090303_create_verification_codes.up.sql) confirms attempts DEFAULT 0, consumed_at nullable single-use marker, channel CHECK(sms/email), purpose CHECK with all four values, and index verification_codes_lookup on (target, purpose, created_at DESC).

Layer notes for the Rust rewrite:
- 'pure-fn' = generateCode + ChannelFor: no deps, ideal for unit + property tests (esp. gen-rejection-sampling-uniform via a distribution/statistical test).
- 'service-unit' = Service with a fake Store + mock Sender; this is where the bulk of security assertions live.
- 'contract' = the store contract suite (contract_test.go's runStoreContract). In Rust, implement ONE shared contract test run over both the in-memory fake and the real sqlx Postgres repo (mirrors TestStoreContract_Fake + repository_integration_test.go) to prevent fake/DB drift.
- No 'handler' assertions here: this concern is the service+store+sender layer only. Request binding/validation (e.g. phone/email format of `target`, rejecting empty target, purpose whitelist at the API edge) belongs to the auth/handler concern — flag for the synthesizer to place there. Note service.go does NOT itself validate target shape or purpose membership; it trusts the caller, so those checks must exist at the handler boundary.

Overlaps to de-dup with other concerns:
- verify-indistinguishable-error / verify-no-code-invalid overlap with the auth login concern's 'login must not reveal whether phone/email is registered' — same privacy principle, but here it's asserted at the OTP service boundary.
- ChannelFor / target-shape overlaps with users-table phone/email identity concern (email lowercasing, phone length) — those validations are NOT in otp; do not duplicate them into this concern.

Go-vs-Rust divergences:
- Rust should use a `subtle`-style constant-time eq crate for verify-constant-time-compare (go uses crypto/subtle.ConstantTimeCompare). Property: comparison time independent of match prefix.
- Rust RNG: use OsRng (rand crate) or getrandom to mirror crypto/rand; keep rejection sampling (reject byte>=250) exactly — do not swap for rand's uniform int helper without confirming it is also unbiased, but the explicit reject>=250 loop is the spec'd behavior to test.

Open questions for synthesizer:
1. maxVerifyAttempts (5), ttl, cooldown, dailyLimit: go hardcodes maxVerifyAttempts as a const; the other three are constructor params. Decide in Rust whether maxVerifyAttempts is also configurable — assertion verify-cap-is-five-exactly assumes the current fixed 5.
2. sender-no-log-code-or-raw-target is marked mvp=false because the real provider is post-MVP, but if the Rust MVP ships ANY logging in the OTP path, the 'never log code' rule should be enforced immediately even for the mock in non-test builds — worth a synthesizer decision.
3. The go Service performs no explicit cleanup/expiry of old codes; expiry is enforced only at Verify time (expires_at check) and supersession via LatestUnconsumed. No GC assertion is listed because go has none — flag if Rust wants a reaper.

## E. 会话 / refresh token

**MVP：**

- [ ] **Given a token issue, when the raw refresh token is generated, then it is drawn from a cryptographically secure RNG over 32 random bytes (256 bits) and base64url-encoded (no padding), yielding an opaque high-entropy string.** `[纯函数]` — High entropy is what makes a plain SHA-256 equality lookup safe (no bcrypt, no timing concerns); a guessable/low-entropy token would be brute-forceable.  
  <sub>service.go generateToken() lines 232-238; rawTokenBytes=32 line 57</sub>
- [ ] **Given a token is issued, when the row is persisted, then only the SHA-256 hex hash of the raw token is stored in token_hash and the raw secret is returned to the caller exactly once (never re-derivable from the DB).** `[服务+mock仓储]` — Storing plaintext refresh tokens means a DB leak = full session takeover; storing only the hash makes the DB dump useless for impersonation.  
  <sub>service.go issue() lines 213-228 (TokenHash: hashToken(raw)); hashToken lines 242-245; docs section 5 / migration 20260709090643 line 6</sub>
- [ ] **Given a presented raw refresh token, when the service looks it up, then it hashes with SHA-256 and does an equality lookup on token_hash (same hashing function for issue and lookup, hex encoding), so a valid raw token resolves to its stored row.** `[服务+mock仓储]` — Lookup by hash (not plaintext) is the mechanism that keeps plaintext out of the DB while still being O(1); the issue/lookup hash must be identical or valid tokens would fail.  
  <sub>service.go Rotate() line 120-121, Revoke() line 189, hashToken lines 242-245</sub>
- [ ] **Given a user already holds a refresh token, when the service issues a new token for that same user (new login), then all of the user's other tokens are hard-revoked (revoked_at set) before the new one is saved, leaving exactly one active token.** `[服务+mock仓储]` — Strict single-device login: a new login must kick the old device so a stolen/shared session cannot silently persist across a re-login.  
  <sub>service.go Issue() lines 105-110 (RevokeAllForUser then issue); TestService_Issue_RevokesPrevious lines 151-181</sub>
- [ ] **Given a second login revoked the first token, when the old (first) token is later presented to Rotate, then it fails with the generic invalid-refresh-token error while the newly issued token still rotates successfully.** `[服务+mock仓储]` — Confirms the revoked old device is actually locked out on its next refresh (delay bounded by access TTL), not just marked.  
  <sub>TestService_Issue_RevokesPrevious lines 173-180</sub>
- [ ] **Given user Alice holds a token, when user Bob logs in (issue), then Alice's token is untouched and still rotates successfully.** `[服务+mock仓储]` — Single-device revoke must be scoped to the logging-in user only; a cross-user revoke would let one login knock out unrelated users' sessions.  
  <sub>TestService_Issue_OtherUsersUntouched lines 183-196</sub>
- [ ] **Given two successive issues for the same user, when both raw tokens are returned, then they are distinct (a fresh random secret each time).** `[服务+mock仓储]` — Reissuing an identical token would defeat rotation/revocation and let an old cookie value still work.  
  <sub>TestService_Issue_RevokesPrevious lines 166-168</sub>
- [ ] **Given the store's Save fails, when Issue is called, then it returns a non-nil (internal) error and no token is minted.** `[服务+mock仓储]` — A silent success on a failed save would hand the client a token the server never persisted, breaking every later refresh.  
  <sub>TestService_Issue_SaveError lines 643-650</sub>
- [ ] **Given a live (unrotated, unrevoked, unexpired) token, when Rotate is called, then MarkRotated stamps rotated_at exactly once, a fresh distinct token is issued for the same user, and the returned user_id equals the token's user_id.** `[服务+mock仓储]` — Refresh-token rotation: each refresh consumes the presented token and mints a successor, so a leaked older token becomes detectable on reuse.  
  <sub>service.go Rotate() lines 132-143; TestService_Rotate_Success lines 198-218</sub>
- [ ] **Given a token was rotated and returned a successor, when the successor is itself presented to Rotate, then it rotates successfully (the chain continues).** `[服务+mock仓储]` — Rotation must be repeatable indefinitely; a successor that could not itself be rotated would break normal long-lived sessions.  
  <sub>TestService_Rotate_Success lines 214-217</sub>
- [ ] **Given a raw token that was never issued, when Rotate is called, then it returns the single generic invalid-refresh-token error (indistinguishable from revoked/expired).** `[服务+mock仓储]` — Error must be undifferentiated so a caller cannot probe which tokens ever existed / were valid.  
  <sub>service.go lines 122-123; TestService_Rotate_Unknown lines 560-565; ErrInvalidRefreshToken doc lines 49-52</sub>
- [ ] **Given a token that was hard-revoked, when Rotate is called, then it returns the same generic invalid-refresh-token error as an unknown token.** `[服务+mock仓储]` — Revoked and unknown must be indistinguishable to avoid leaking session/account state to an attacker.  
  <sub>service.go line 128; TestService_Rotate_Revoked lines 567-577</sub>
- [ ] **Given a token whose expires_at is in the past, when Rotate is called, then it returns the generic invalid-refresh-token error (checked as now > expires_at, so exact-boundary expiry is treated as invalid).** `[服务+mock仓储]` — Expiry bounds token lifetime; must be enforced and indistinguishable from other invalid cases.  
  <sub>service.go line 128 (time.Now().After(t.ExpiresAt)); TestService_Rotate_Expired lines 579-587</sub>
- [ ] **Given FindByHash returns a non-ErrNotFound internal error, when Rotate is called, then it returns an internal error (NOT the generic invalid-refresh-token error).** `[服务+mock仓储]` — A DB hiccup masquerading as a 401 would make clients drop a healthy session; only genuine invalidity should map to the 401 error.  
  <sub>service.go lines 125-127; TestService_Rotate_StoreErrors 're-read after lost claim fails' lines 515-531</sub>
- [ ] **Given MarkRotated returns an internal error, when Rotate is called on a live token, then Rotate returns an internal error, not the generic invalid-refresh-token error.** `[服务+mock仓储]` — Same fail-safe: a store failure must not be reported as an invalid token (client would needlessly log out).  
  <sub>service.go lines 133-135; TestService_Rotate_StoreErrors 'mark rotated fails' lines 505-513</sub>
- [ ] **Given the winning Rotate consumed the token but issuing the successor fails (Save error), when Rotate returns, then it returns an internal error (not invalid-refresh-token), signalling a lost-response condition rather than an auth failure.** `[服务+mock仓储]` — The client must retry rather than treat the session as dead; misclassifying as 401 would log the user out on a transient DB error.  
  <sub>service.go lines 138-141; TestService_Rotate_WinnerIssueFails_RetryWithinGraceRecovers lines 485-489</sub>
- [ ] **Given a raw token, when Revoke is called, then the token's revoked_at is set; and calling Revoke again on the same token, or on a never-issued token, returns no error (logout always succeeds).** `[服务+mock仓储]` — Logout must be idempotent so a client retry / double-click / already-expired cookie never produces a user-visible error.  
  <sub>service.go Revoke() lines 188-200; TestService_Revoke_Idempotent lines 589-605</sub>
- [ ] **Given a raw token that maps to ErrNotFound, when Revoke is called, then it returns nil without attempting a store Revoke.** `[服务+mock仓储]` — Unknown-token logout is a no-op success; it also avoids leaking (via error) whether the token existed.  
  <sub>service.go lines 189-192; TestService_Revoke_Idempotent lines 602-604</sub>
- [ ] **Given a user holds multiple active tokens, when RevokeAll is called for that user, then every one of that user's tokens is revoked (active count = 0) and each subsequently fails Rotate with the generic error.** `[服务+mock仓储]` — Logout-everywhere / password-change / theft response must kill the whole family, not just the latest token.  
  <sub>service.go RevokeAll() lines 204-209; TestService_RevokeAll lines 607-629</sub>
- [ ] **Given Alice and Bob each hold tokens, when RevokeAll is called for Alice, then only Alice's tokens are revoked and Bob's token still rotates.** `[服务+mock仓储]` — Bulk revoke must be user-scoped; leaking across users would be a denial-of-service on unrelated accounts.  
  <sub>TestService_RevokeAll lines 630-633; contract 'revoke all for user touches only that user' lines 230-258</sub>
- [ ] **Given a user with no active tokens (or already-revoked tokens), when RevokeAll is called, then it returns no error.** `[服务+mock仓储]` — Idempotency so repeated logout-all / a user who never logged in is not an error.  
  <sub>TestService_RevokeAll lines 635-640; RevokeAll() lines 204-209</sub>
- [ ] **Given RevokeAllForUser returns an internal error, when RevokeAll is called, then it propagates a wrapped internal error.** `[服务+mock仓储]` — A failed bulk revoke (e.g. during a theft response) must surface so the caller does not believe sessions were killed when they weren't.  
  <sub>service.go lines 205-207; TestService_Rotate_StoreErrors 'theft revoke-all fails' lines 546-557</sub>
- [ ] **Given grace=0 (or a lapsed grace) and a token already rotated, when that rotated token is replayed, then the service treats it as theft: it revokes ALL the user's sessions (including the legitimate successor) and returns the generic invalid error; active token count becomes 0.** `[服务+mock仓储]` — A rotated token only the past should hold reappearing can only mean a leak; killing the whole family is the theft response. The generic 401 (not a distinct error) keeps it indistinguishable to the client/attacker.  
  <sub>service.go lines 172-183; TestService_Rotate_ReuseBeyondGrace_RevokesEverything lines 254-276; TestService_Rotate_ReuseAfterGraceExpires_RevokesEverything lines 299-324</sub>
- [ ] **Given a token was rotated and then hard-revoked (logout) while still inside the grace window, when it is replayed, then Rotate returns the generic invalid error and does NOT mint a token — revocation admits no grace.** `[服务+mock仓储]` — revoked_at is the hard kill (logout / login-elsewhere / password change); a revoked token must never resurrect a session even mid-grace, or logout would be defeatable via a racing tab.  
  <sub>service.go line 128 (RevokedAt check runs before any grace path); TestService_Rotate_RevokedAdmitsNoGrace lines 278-297</sub>
- [ ] **Given grace=0 and a competitor wins MarkRotated, when the loser re-reads and finds the token rotated, then the loser is treated as reuse/theft: generic error and all the user's tokens revoked (active=0).** `[服务+mock仓储]` — Strict mode promises a rotated token is never honoured twice regardless of interleave; with no grace even a legitimate race maps to theft.  
  <sub>service.go lines 144-183; TestService_Rotate_LostClaim_ToRotation_NoGrace lines 368-388</sub>
- [ ] **Given a token is saved, when it is looked up by hash, then the same id and user_id round-trip and a freshly saved token has both revoked_at and rotated_at NULL.** `[契约·真库]` — Baseline store contract; a fresh token must not appear revoked or rotated or every first refresh would fail.  
  <sub>contract_test.go 'save then find by hash round-trips' lines 29-48</sub>
- [ ] **Given a hash that was never stored, when FindByHash is called, then it returns ErrNotFound (a distinct sentinel the service maps to the generic invalid error).** `[契约·真库]` — The store must signal absence via ErrNotFound so the service can convert it to the undistinguished 401 without leaking existence.  
  <sub>contract_test.go 'unknown hash returns ErrNotFound' lines 50-55</sub>
- [ ] **Given the refresh_tokens schema, when two rows attempt the same token_hash, then the unique index on token_hash rejects the second insert.** `[契约·真库]` — Hash collision / duplicate would let one lookup match ambiguous rows; uniqueness is required for the equality-lookup security model.  
  <sub>migration 20260709090643_create_refresh_tokens.up.sql line 14 (refresh_tokens_hash UNIQUE); docs section 5 index note</sub>
- [ ] **Given a token is revoked, when FindByHash is called, then the store still returns the row with revoked_at set (it does not hide revoked rows).** `[契约·真库]` — Deciding validity is the service's job; a store that hid revoked rows would mask service-level validity bugs and make the theft-detection re-read impossible.  
  <sub>contract_test.go 'revoke marks the token but find still returns it' lines 57-76</sub>
- [ ] **Given a token is revoked twice, when its revoked_at is read after each call, then the timestamp is unchanged by the second revoke (first-write-wins).** `[契约·真库]` — Idempotent revoke must not keep bumping the timestamp, which would corrupt any grace/audit reasoning based on it.  
  <sub>contract_test.go 'revoke is idempotent' lines 78-101</sub>
- [ ] **Given a live token, when MarkRotated is called twice, then the first call wins (returns true, stamps rotated_at, leaves revoked_at NULL) and the second loses (returns false, does not change rotated_at).** `[契约·真库]` — The single-winner guarantee is what stops concurrent refreshes from each minting a token off the same claim; the loser must fall back to the grace decision.  
  <sub>contract_test.go 'mark rotated stamps exactly once' lines 103-143; Store.MarkRotated doc lines 79-83</sub>
- [ ] **Given a revoked token, when MarkRotated is called, then it returns false and does not stamp rotated_at.** `[契约·真库]` — A revoked token must never be rotatable; stamping it would let a hard-killed session be resurrected.  
  <sub>contract_test.go 'mark rotated loses on a revoked token' lines 145-168</sub>
- [ ] **Given an unknown token id, when MarkRotated is called, then it returns false with no error.** `[契约·真库]` — An unknown id is not an error; it must simply lose the claim so the service treats it as invalid.  
  <sub>contract_test.go 'mark rotated on an unknown id loses without error' lines 170-179</sub>
- [ ] **Given a rotated token, when Revoke is called, then the row ends with both rotated_at AND revoked_at set (rotation is not revocation; a hard kill can still land on a token inside its grace).** `[契约·真库]` — Logout / revoke-all must be able to hard-kill a token sitting in its reuse grace, or grace would create an un-revocable window.  
  <sub>contract_test.go 'revoke still lands on a rotated token' lines 181-202</sub>
- [ ] **Given a user has one rotated and one live token, when RevokeAllForUser is called, then both end with revoked_at set.** `[契约·真库]` — The theft/logout-all response must revoke rotated tokens too, else a leaked rotated token could survive the storm.  
  <sub>contract_test.go 'revoke all covers rotated tokens' lines 204-228</sub>
- [ ] **Given tokens for two users, when RevokeAllForUser is called for one, then only that user's tokens get revoked_at set and the other user's token stays active.** `[契约·真库]` — Bulk revoke must be strictly user-scoped at the store layer as well.  
  <sub>contract_test.go 'revoke all for user touches only that user' lines 230-258</sub>
- [ ] **Given the schema and store, when a token is rotated, then rotated_at is stamped while revoked_at remains NULL (the two columns are independent: rotation-consumed vs hard-revoke).** `[契约·真库]` — The whole grace design depends on rotated_at and revoked_at being separate signals; conflating them removes the ability to distinguish honest reuse from a hard kill. Grace logic itself deferred per MVP, but the column separation is present in the Rust schema now.  
  <sub>migration 20260709090643_create_refresh_tokens.up.sql lines 8-9; docs section 5 mechanism 2; contract 'mark rotated stamps exactly once' lines 126-128</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **Given FindByHash or Revoke returns a non-ErrNotFound internal error, when Revoke is called, then it propagates a wrapped internal error.** `[服务+mock仓储]` — A real store failure during logout should surface, not be swallowed as success.  
  <sub>service.go lines 193-198 (error wrapping paths); no dedicated go test — inferred from code, low priority</sub>
- [ ] **Given a positive reuse grace and a token already rotated less than grace ago, when the same rotated token is presented again, then Rotate succeeds, returns the correct user_id, and mints a fresh distinct sibling token (distinct from both the raw token and the winner's successor) WITHOUT revoking anything.** `[服务+mock仓储]` — Parallel tabs sharing one cookie / a client retrying a lost response are honest reuse; killing the session on them would break normal browsing. Deferred per reference doc MVP scope (rotation grace).  
  <sub>service.go lines 162-171; TestService_Rotate_ReuseWithinGrace lines 220-252; doc section 8 defers rotation grace</sub>
- [ ] **Given an in-grace reuse minted a sibling, when both the winner's successor and the sibling are later rotated, then both succeed (honest reuse leaves the whole family live, not just one).** `[服务+mock仓储]` — In-grace reuse must not be treated as theft; both outstanding tokens must keep working. Rotation grace deferred per MVP.  
  <sub>TestService_Rotate_ReuseWithinGrace lines 245-251</sub>
- [ ] **Given a positive grace and a token whose rotated_at is backdated beyond the grace window, when it is replayed, then it is treated as theft (revoke-all + generic error), i.e. the grace comparison is on rotated_ago <= grace so replays strictly after the window trigger theft.** `[服务+mock仓储]` — Confirms the window boundary: grace protects only in-window reuse; the production theft path is a replay arriving after the window lapses. Rotation grace deferred per MVP.  
  <sub>service.go lines 162-163, 172-183; TestService_Rotate_ReuseAfterGraceExpires_RevokesEverything lines 299-324</sub>
- [ ] **Given two requests read the same live token and a competitor wins MarkRotated first, when the loser re-reads and finds the token rotated (not revoked) within grace, then the loser still gets its own fresh usable token that itself chains.** `[服务+mock仓储]` — The core race the grace exists for: the loser of a concurrent claim must walk away with a session, not a 401. Rotation grace deferred per MVP.  
  <sub>service.go lines 144-171; TestService_Rotate_LostClaim_ToRotation_WithinGrace lines 338-366</sub>
- [ ] **Given a competitor hard-revokes the token between the read and the claim, when the loser re-reads and finds revoked_at set, then Rotate returns the generic error, mints no token, and does not resurrect the session even under a generous grace.** `[服务+mock仓储]` — A logout that raced a refresh and won must beat the refresh: revocation has no grace, so the loser fails closed.  
  <sub>service.go lines 146-155; TestService_Rotate_LostClaim_ToRevocation lines 390-410</sub>
- [ ] **Given the row disappears (deleted) between the initial read and the re-read, when Rotate re-reads and gets ErrNotFound, then it returns the generic invalid error (fail closed).** `[服务+mock仓储]` — A vanished row is ambiguous; failing closed avoids minting a token off a state the service can no longer verify.  
  <sub>service.go lines 146-149; TestService_Rotate_LostClaim_TokenVanished lines 412-423</sub>
- [ ] **Given MarkRotated reports a loss but the re-read finds the row neither rotated nor revoked (a store-contract-violating state), when Rotate evaluates it, then it returns the generic invalid error rather than trusting the contradictory store.** `[服务+mock仓储]` — Fail closed rather than mint a token off a store that contradicts its own single-winner contract.  
  <sub>service.go lines 153-155; TestService_Rotate_LostClaim_RowUntouched lines 425-438</sub>
- [ ] **Given the re-read (second FindByHash) after a lost claim returns an internal error, when Rotate runs, then it returns an internal error, not the generic invalid-refresh-token error.** `[服务+mock仓储]` — A transient store failure on the re-read must not be reported as an auth failure.  
  <sub>service.go lines 150-152; TestService_Rotate_StoreErrors 're-read after lost claim fails' lines 515-531</sub>
- [ ] **Given N parallel requests race one cookie within grace, when all Rotate concurrently, then every request ends with no error and a fresh distinct token (no duplicates, none equal to the original).** `[服务+mock仓储]` — Real parallel-tab behaviour must be safe under concurrency: exactly-once MarkRotated plus grace guarantees each racer gets its own live session. Rotation grace deferred per MVP.  
  <sub>TestService_Rotate_ConcurrentReuse_AllSucceed lines 440-473</sub>
- [ ] **Given a Rotate consumed the token but the successor's Save failed (lost mid-flight), when the client retries with the same cookie inside the grace, then Rotate recovers and returns a usable fresh token for the user.** `[服务+mock仓储]` — A lost response must be recoverable via retry rather than silently logging the user out. Rotation grace deferred per MVP.  
  <sub>TestService_Rotate_WinnerIssueFails_RetryWithinGraceRecovers lines 475-498</sub>
- [ ] **Given an in-grace reuse whose successor Save fails, when Rotate runs, then it returns an internal error (not invalid-refresh-token).** `[服务+mock仓储]` — Fail-safe consistency: store failures on the grace path must not be reported as auth failures. Rotation grace deferred per MVP.  
  <sub>service.go lines 166-169; TestService_Rotate_StoreErrors 'issue within grace fails' lines 533-544</sub>
- [ ] **Given a user row is deleted, when the cascade runs, then all that user's refresh_tokens rows are deleted (ON DELETE CASCADE).** `[契约·真库]` — Account deletion must not leave orphaned live sessions pointing at a gone user.  
  <sub>migration 20260709090643_create_refresh_tokens.up.sql line 5 (REFERENCES users ON DELETE CASCADE); account_deletion purpose deferred per doc section 8</sub>

> 注：Scope notes for the synthesizer:

1. MVP tagging follows reference doc section 8, which defers "轮换宽限" (rotation grace). I set mvp=false for every assertion that only matters when reuseGrace > 0: in-grace sibling minting, both-tokens-live, window-boundary, lost-claim-within-grace-recovers, concurrent-reuse, retry-recovers, grace-issue-failure, and the after-grace-expiry boundary test. The go tests achieve "strict mode" by constructing NewService(store, ttl, 0). For the Rust MVP the user builds only single-device + hard revoke (revoked_at); reuseGrace defaults to 0.

2. IMPORTANT go/Rust divergence on "reuse beyond grace = theft" (reuse-beyond-grace-revokes-all, lost-claim-to-rotation-no-grace-is-theft): with grace=0 these fire on ANY replay of a rotated token, INCLUDING a legitimate concurrent race that simply lost the MarkRotated claim. In strict MVP mode the loser of an honest parallel-tab race gets a 401 AND the whole family is revoked. This is the documented tradeoff (service.go package comment lines 22-29 and TestService_Rotate_LostClaim_ToRotation_NoGrace). The user should decide whether MVP wants (a) strict grace=0 theft-on-any-reuse, or (b) a minimal grace even in MVP to avoid punishing honest races. I kept the theft-revoke-all assertion mvp=true because single-device+hard-revoke MVP still needs *some* defined behaviour for a replayed rotated token; the grace-window refinement is what's deferred.

3. Rust schema (migration 20260709090643) already includes both rotated_at and revoked_at columns even though grace logic is deferred ("宽限窗口逻辑在 service 层，先留列"). So store-level assertions about the two columns being independent are mvp=true (schema is present now), while the SERVICE grace behaviour that reads rotated_at within a window is mvp=false.

4. Concurrency assertions (single-winner MarkRotated, concurrent-reuse-all-succeed, lost-claim variants) are the hardest to port: go relies on a fake with fault-injection hooks (markRotatedFn/findByHashFn/saveErr) and an integration test running the SAME contract against real Postgres. For Rust, the "contract" layer assertions (store-*) should run against BOTH a fake AND a real sqlx repo, mirroring contract_test.go's dual-target design (lines 12-19). MarkRotated's atomic single-winner claim maps to a single UPDATE ... WHERE rotated_at IS NULL AND revoked_at IS NULL RETURNING, and the "won" bool = rows affected == 1.

5. The generic-error / indistinguishability theme recurs (unknown, revoked, expired, theft-response all -> one ErrInvalidRefreshToken). Rust should use a single error variant for all four; I flagged the WHY on each. The theft-response revoke-all still returns the SAME generic 401 to the client — the only signal it was a theft is a server-side log line (service.go lines 176-179); consider whether Rust wants structured logging/metrics here (not a testable assertion, noted for design).

6. No handler-layer assertions here: this concern is the service. Handler/request-binding for /auth/refresh, /auth/logout (cookie extraction, 401 mapping) belongs to a separate handler concern; I did not invent them.

7. revoke-store-error-surfaces (mvp=false) and lost-claim-reread-error-surfaces (mvp=false) are grounded but lower priority / partly inferred from error-wrapping code paths rather than a dedicated top-level test; flagged accordingly. Everything else maps directly to a named go test or contract subtest.

## F. Auth token（JWT access）

**MVP：**

- [ ] **Given a TokenManager for realm=web with a secret and positive TTL, when it Generates a token for a random user UUID and role "student" and then Parses it, then the returned Claims.subject equals the original UUID exactly.** `[纯函数]`  
  <sub>token_test.go TestTokenRoundTrip (lines 11-33); token.go Generate/Parse</sub>
- [ ] **Given a web TokenManager, when it Generates a token with role "student" and Parses it back, then Claims.role == "student" (the active role round-trips unchanged).** `[纯函数]` — Active role travels in the JWT (reference doc §0 身份与角色解耦: 当前激活角色放 JWT), so it must survive sign/verify verbatim.  
  <sub>token_test.go TestTokenRoundTrip lines 27-29</sub>
- [ ] **Given a web TokenManager, when it Generates and Parses a token, then Claims.realm == "web" (the manager stamps its own realm into every token it issues).** `[纯函数]` — The realm claim is the defence-in-depth signal that the cross-realm check re-verifies; it must match the issuing manager.  
  <sub>token_test.go TestTokenRoundTrip lines 30-32; token.go Generate line 75 (Realm: m.realm)</sub>
- [ ] **Given a TokenManager, when it Generates a token, then the produced JWT header alg is HS256 (HMAC-SHA256 symmetric signing), not RS*/ES*/none.** `[纯函数]` — The module deliberately uses HS256 symmetric signing (stateless tokens, one secret per realm); asserting the alg guards against accidental algorithm drift.  
  <sub>token.go Generate line 83 (jwt.NewWithClaims(jwt.SigningMethodHS256, ...))</sub>
- [ ] **Given a TokenManager with TTL=T, when it Generates a token at time now, then the token's exp claim == iat + T (expiry is set to issue time plus the manager's TTL, and iat is present).** `[纯函数]` — Access tokens are short-lived and stateless; a correct exp is the only thing that bounds a compromised token's lifetime (reference doc §0 失效在一个 access-token TTL 内生效).  
  <sub>token.go Generate lines 79-80 (IssuedAt=now, ExpiresAt=now.Add(m.ttl))</sub>
- [ ] **Given a token whose exp is already in the past (e.g. manager built with negative TTL), when Parse is called, then it returns an error and yields no valid Claims.** `[纯函数]` — An expired token must never authenticate — this is how logout/disable eventually take effect within one TTL.  
  <sub>token_test.go TestParseRejectsExpired lines 35-44</sub>
- [ ] **Given a token signed by a manager with secret-a, when a second manager with secret-b (same realm) Parses it, then it returns an error (signature verification fails under a different key).** `[纯函数]` — Signature-under-wrong-key rejection is the core integrity guarantee; without it any HS256 secret would validate.  
  <sub>token_test.go TestParseRejectsWrongSecret lines 46-54</sub>
- [ ] **Given a validly signed token whose payload bytes are then mutated (e.g. flip a character in the claims segment) without re-signing, when Parse is called, then it returns an error because the HMAC no longer matches.** `[纯函数]` — Tamper detection is the whole point of signing — a modified subject/role/exp must invalidate the signature. Inferred: generalizes the go wrong-secret/malformed tests to explicit payload tampering; grounded in HS256 integrity semantics.  
  <sub>token.go Parse lines 98-100 (err || !token.Valid); generalization of token_test.go TestParseRejectsWrongSecret</sub>
- [ ] **Given a token forged with alg="none" and an empty signature (JWT downgrade attack), when Parse is called, then it returns an error and never accepts the token.** `[纯函数]` — SECURITY: the classic JWT 'none' downgrade lets an attacker mint unsigned tokens; Parse must pin the signing method to HMAC and reject anything else.  
  <sub>token_test.go TestParseRejectsNoneAlg lines 79-93; token.go Parse lines 93-95 (SigningMethodHMAC type assertion)</sub>
- [ ] **Given a token whose header alg is a non-HMAC family the manager does not expect (e.g. RS256/ES256), when Parse is called, then the keyfunc rejects it with an 'unexpected signing method' error rather than attempting verification.** `[纯函数]` — SECURITY: pinning to HMAC prevents algorithm-confusion attacks (e.g. RS256 public key used as HMAC secret). Inferred from the explicit HMAC-only type assertion; go tests only cover 'none' but the guard rejects all non-HMAC algs.  
  <sub>token.go Parse lines 93-95 (if _, ok := t.Method.(*jwt.SigningMethodHMAC); !ok)</sub>
- [ ] **Given each of the malformed inputs "" (empty), "abc", "a.b.c", "....", and "Bearer xyz", when Parse is called on it, then every one returns an error.** `[纯函数]` — Guards against garbage/partial tokens and the common mistake of passing the whole "Bearer <tok>" header value instead of the stripped token.  
  <sub>token_test.go TestParseRejectsMalformed lines 68-75</sub>
- [ ] **Given a token that is correctly signed and carries the matching realm but whose subject claim is not a valid UUID (e.g. "not-a-uuid"), when Parse is called, then it returns an error even though signature and realm are valid.** `[纯函数]` — The principal id must be a well-formed UUID before it is trusted downstream; a valid signature is not sufficient if the subject is unparseable.  
  <sub>token_test.go TestParseRejectsBadSubject lines 97-110; token.go Parse lines 105-108</sub>
- [ ] **Given any invalid token (expired, wrong key, wrong realm, malformed, bad subject, none-alg), when Parse fails, then it returns a zero/empty Claims value alongside the error — never partially populated claims a caller might mistakenly trust.** `[纯函数]` — SECURITY: ensures a caller that ignores the error cannot read attacker-controlled subject/role/realm from a half-built Claims. Inferred from every error path returning Claims{}.  
  <sub>token.go Parse lines 99, 102, 107 (all return Claims{}, err)</sub>

**延后（第二周 / 非 MVP，先留断言占位）：**

- [ ] **Given a token minted by the web manager, when the admin manager Parses it, then it returns an error — even in the hypothetical where both managers share the same secret — because the realm claim ("web") does not equal the admin manager's realm.** `[纯函数]` — SECURITY: keeps a web user's token off the admin API. The realm-claim check is defence-in-depth layered on top of per-realm keys, so it must reject even if keys coincide.  
  <sub>token_test.go TestParseRejectsWrongRealm lines 58-66; token.go Parse lines 101-103</sub>
- [ ] **Given a token minted by the admin manager, when the web manager Parses it, then it returns an error (symmetric to the web→admin case: admin realm claim does not match web).** `[纯函数]` — SECURITY: symmetric direction of the realm boundary — an admin token must not authenticate on the web/user API. Inferred symmetric case of the go test.  
  <sub>token_test.go TestParseRejectsWrongRealm lines 58-66 (symmetric direction); token.go Parse lines 101-103</sub>
- [ ] **Given a web manager and an admin manager constructed with DIFFERENT secrets (the intended production config), when either Parses the other realm's token, then it fails at signature verification before the realm check is even reached.** `[服务+mock仓储]` — SECURITY / dual-identity: web and admin are separate identity realms; distinct signing keys are the primary boundary. NOTE the current Rust config has a single JWT_SECRET (config.rs line 9) — the Rust design must add a second admin key for this to hold; flag as design gap.  
  <sub>token.go lines 30-36 & 64-68 (per-realm secret contract); token_test.go TestParseRejectsWrongSecret + TestParseRejectsWrongRealm</sub>
- [ ] **Given a signed token, when Parse validates it, then the realm claim is checked BEFORE the subject UUID is parsed, so a realm mismatch is reported as a realm error (not a subject error) regardless of subject validity.** `[纯函数]` — Defines the deterministic validation order so error semantics are stable and the realm boundary is enforced first. Inferred from code ordering (realm check at line 101 precedes uuid.Parse at line 105).  
  <sub>token.go Parse lines 101-108 (realm check then subject parse)</sub>
- [ ] **Given two managers of the SAME realm but with different secrets, when each generates a token for the same subject/role, then neither manager can Parse the other's token (parsing is keyed strictly by the secret, independent of realm equality).** `[纯函数]` — Confirms secret rotation / per-deployment secrets invalidate old tokens; complements the realm boundary with the key boundary. Inferred consolidation of wrong-secret behavior within one realm.  
  <sub>token_test.go TestParseRejectsWrongSecret lines 46-54 (same realm, different secret)</sub>

> 注：Scope note: this is the auth/token module, adjacent to user/service — listed for completeness per the task. In tsz-go it lives at internal/auth/token.go and is a self-contained HS256 sign/verify unit with no repo/DB deps, so nearly every assertion is layer=pure-fn (testable with just a TokenManager, a uuid, and time). The two service-unit-flavored ones (dual-key-distinct-secrets) really only need two constructed managers, no DB.

RUST DESIGN GAPS TO FLAG for the user:
1. The current Rust config (src/config.rs line 9) exposes a SINGLE `jwt_secret`. The go dual-identity design requires TWO independent signing keys (web realm + admin realm). For assertions reject-cross-realm-* and dual-key-distinct-secrets to hold, the Rust config must grow a second admin secret (e.g. JWT_SECRET_ADMIN) and construct one TokenManager per realm. Without it, cross-realm tokens would share a key and only the realm-claim check would separate them (weaker than go).
2. No auth/token module exists in Rust yet (src/ has only user/, platform/, config, error). This checklist defines that module's contract from scratch.

MVP scoping: Per reference doc §8, the admin domain is deferred to week 2 (MVP only does `users`). So the cross-realm / dual-key assertions (reject-cross-realm-web-to-admin, reject-cross-realm-admin-to-web, dual-key-distinct-secrets, distinct-secrets independence, check-order) are marked mvp=false — the web realm alone is MVP. However, the reference doc's §0 双身份分离 and the token module's core purpose is exactly this separation, so these are HIGH-PRIORITY for the week-2 admin work; keep them visible. All single-realm sign/verify/reject assertions are mvp=true because access-token issuance is required for login to work in MVP.

Overlap with other concerns: exp/TTL-based invalidation (reject-expired) is the mechanism behind the reference doc's "失效在一个 access-token TTL 内生效" rule — the account-disable/logout concerns rely on it but enforce disable at the refresh/DB boundary, not here. The `role` claim ties to the last_active_role / role-switching concern (user/service) but this module only guarantees the claim round-trips; it does not decide which role is active.

No handler-layer assertions belong here — extracting the token from the Authorization header and stripping "Bearer " is middleware, though reject-malformed-strings deliberately includes the "Bearer xyz" case to document that Parse expects the already-stripped token.

---

## G. 批判补充：enumerate 漏掉的（含三个延后流程 + handler 层安全）

### password-reset / account-lifecycle

- [ ] **given a registered user and a valid reset code sent to their identifier; when ResetPassword(identifier, code, newPassword) runs; then it sets the new bcrypt hash, the OLD password no longer logs in, the NEW password does, AND every prior session is revoked (the pre-reset refresh token no longer rotates -> ErrInvalidRefreshToken).** `[服务+mock仓储·延后]` — The entire ResetPassword flow (a whole concern with 6 dedicated go tests) was omitted from the union. Revoking all sessions on reset is a security requirement: a password reset must sign an attacker holding old tokens out everywhere.  
  <sub>TestService_ResetPassword_Success (service_test.go:473-506); service.go ResetPassword:284-318 (SetPassword + sessions.RevokeAll); reference doc section 8 defers password_reset</sub>

### password-reset

- [ ] **given a valid reset code for an identifier that has NO account (e.g. unregistered phone); when ResetPassword runs; then it returns ErrInvalidResetCode — the SAME generic error as a wrong code — never revealing that the identifier is unregistered.** `[服务+mock仓储·延后]` — PRIVACY/existence-non-leak: a valid code for a non-existent account must stay indistinguishable from a wrong code, or reset becomes an account-enumeration oracle. Missed entirely.  
  <sub>TestService_ResetPassword_UnknownPhone (service_test.go:525-535); service.go:291-296 (ErrNotFound -> ErrInvalidResetCode)</sub>
- [ ] **given a wrong/expired reset code; when ResetPassword runs; then it returns ErrInvalidResetCode and the password is UNCHANGED (the original password still logs in).** `[服务+mock仓储·延后]` — A failed reset must not partially mutate state; the code error must be undifferentiated. Missed.  
  <sub>TestService_ResetPassword_WrongCode (service_test.go:508-521); service.go:287-289</sub>
- [ ] **given a disabled account with a valid reset code; when ResetPassword runs; then it returns ErrAccountDisabled — checked AFTER the code verifies (mirroring code login) so a wrong code never reveals disabled state; a disabled account cannot reset its way back in.** `[服务+mock仓储·延后]` — Ordering (code-check before disabled-check) + disabled enforcement on the reset path. Missed. Same privacy pattern as login-code disabled.  
  <sub>TestService_ResetPassword_Disabled (service_test.go:539-550); service.go:300-302</sub>
- [ ] **given a reset code delivered to the PHONE target; when ResetPassword is called with the same code but the EMAIL identifier of the same account; then it returns ErrInvalidResetCode — the code is bound to the normalized target it was sent to, so a code from one channel cannot reset via another.** `[服务+mock仓储·延后]` — SECURITY: guards against a channel-mixup where a code obtained on a weaker channel resets through a different identifier. Verify checks the code against normalizeIdentifier(identifier). Missed.  
  <sub>TestService_ResetPassword_CrossChannelRejected (service_test.go:686-709); service.go:285-287,291</sub>
- [ ] **given a successful ResetPassword; when the same reset code is replayed; then it returns ErrInvalidResetCode (single-use — the code was consumed by the first successful verify).** `[服务+mock仓储·延后]` — Single-use replay protection on the reset path specifically. Missed.  
  <sub>TestService_ResetPassword_Success reuse branch (service_test.go:502-505)</sub>

### account-deletion

> Rust 当前实现与权威契约见 `docs/account-deletion-design.md`；下列延后用例已按当前 Redis OTP、
> PostgreSQL 事务、RFC 9457 和 cookie 架构落地，不沿用旧 Go 路径/错误名。

- [ ] **given an authenticated user and a valid deletion code sent to their own contact on the chosen channel; when DeleteAccount(userID, channel, code) runs; then it FIRST revokes all sessions THEN deletes the user (cascading to roles/profiles/tokens); afterwards GetByID -> ErrNotFound and the pre-delete refresh token no longer rotates.** `[服务+mock仓储·延后]` — Whole DeleteAccount concern (5 go tests) omitted. Revoke-before-delete is deliberate so 'delete signs you out everywhere' holds even if the FK cascade action changes.  
  <sub>TestService_DeleteAccount_Success (service_test.go:552-578); service.go DeleteAccount:345-365; reference doc section 8 defers account_deletion</sub>
- [ ] **given the deletion code is sent to the account's OWN contact on file (never a value supplied in the request); when RequestAccountDeletion(userID, channel) runs; then the code target is u.Phone or u.Email resolved from the loaded user, so possession of the code proves ownership.** `[服务+mock仓储·延后]` — SECURITY: the code goes to the contact already on the account, not a caller-supplied target — that is what makes it proof of ownership. Missed; distinct from bind (which targets a new value).  
  <sub>service.go RequestAccountDeletion:326-336, deletionTarget:372-387; TestService_DeleteAccount_ViaEmailChannel (service_test.go:580-599)</sub>
- [ ] **given a phone-only account (no email on file); when RequestAccountDeletion or DeleteAccount is called with channel=email (and mirror: email-only account with channel=phone); then it returns ErrChannelUnavailable BEFORE any code is sent, and the account is untouched.** `[服务+mock仓储·延后]` — The 'phone optional' redesign opened a path where a channel has no contact; without the guard a code would be 'sent' to an empty target. Both directions are tested. Missed.  
  <sub>TestService_DeleteAccount_EmailChannelUnavailable (service_test.go:619-633), TestService_DeleteAccount_PhoneChannelUnavailable (service_test.go:638-652); service.go deletionTarget:372-387</sub>
- [ ] **given a wrong/expired deletion code; when DeleteAccount runs; then it returns ErrInvalidDeletionCode and the account SURVIVES (GetByID still succeeds).** `[服务+mock仓储·延后]` — A failed confirmation must not delete; code error stays undifferentiated. Missed.  
  <sub>TestService_DeleteAccount_WrongCode (service_test.go:601-614); service.go:355-357</sub>
- [ ] **given a deletion channel that is neither 'phone' nor 'email'; when deletionTarget is reached (direct service caller bypassing handler binding); then it returns ErrInvalidChannel.** `[服务+mock仓储·延后]` — Defensive guard for direct callers; handler binding is first line but service must not trust the channel string. Missed.  
  <sub>service.go deletionTarget default branch:384-386; ErrInvalidChannel doc:47-50</sub>

### contact-bind

- [ ] **given a phone-only account binding a NEW email; when RequestContactBindCode(userID, 'New@Example.com') runs; then the bind code is sent to the NEW, normalized (lowercased) contact value in the request — not to any contact already on file — so verifying it later proves the user controls that new contact.** `[服务+mock仓储·延后]` — Whole BindContact concern (7 go tests) omitted. Unlike login/reset/deletion (code -> existing contact), bind sends to the NEW value; this is the core proof-of-control property. Missed.  
  <sub>TestService_BindContact_Email (service_test.go:824-854); service.go RequestContactBindCode:558-567, BindContact:575-590; reference doc section 8 defers contact_bind</sub>
- [ ] **given a contact value already held by ANOTHER account; when RequestContactBindCode is called; then it returns ErrEmailTaken/ErrPhoneTaken and NO code is sent (availability checked before dispatch), and BindContact re-checks availability again before consuming the code.** `[服务+mock仓储·延后]` — Prevents wasting a code on a conflicting bind and prevents hijacking another account's contact; the unique index is the final race guard. Double-check (request-time and consume-time) is deliberate. Missed.  
  <sub>TestService_BindContact_Taken (service_test.go:911-927); service.go ensureContactAvailable:596-620, called at :563 and :580</sub>
- [ ] **given a wrong bind code; when BindContact runs; then it returns ErrInvalidBindCode and the contact is NOT written (user's email/phone stays empty); a successful bind's code is single-use (replay -> ErrInvalidBindCode).** `[服务+mock仓储·延后]` — Failed/replayed bind must not mutate the identifier; undifferentiated code error. Missed.  
  <sub>TestService_BindContact_WrongCode (service_test.go:878-891), TestService_BindContact_Email replay (service_test.go:850-853); service.go:583-585</sub>
- [ ] **given a caller re-binding a contact they ALREADY own; when ensureContactAvailable evaluates it; then it treats 'held by me' (existing.ID == userID) as available (harmless no-op), not a self-conflict.** `[服务+mock仓储·延后]` — Rebinding one's own value must not error as taken. Missed.  
  <sub>TestService_BindContact_RebindOwn (service_test.go:931+); service.go ensureContactAvailable:613-614</sub>

### contact-bind / input-validation

- [ ] **given a malformed contact ('not-an-email@', '@nope', a <5-char phone '123', 'Name <a@b.com>'); when RequestContactBindCode/BindContact classify it; then it returns ErrInvalidContact and nothing is sent — classification mirrors registration (an '@' => email that must ParseAddress and round-trip exactly; otherwise a 5-20 char phone).** `[纯函数·延后]` — classifyContact is where the register phone-5-20 / email-format rule actually lives in the SERVICE (the union flagged this as an open gap for Rust: if there is no handler binding layer, this length/format check must be in the service). Missed.  
  <sub>TestService_BindContact_InvalidContact (service_test.go:893-907); service.go classifyContact:625-639 (len 5-20, mail.ParseAddress round-trip)</sub>

### sessions / logout

- [ ] **given a refresh token; when Logout(rawRefreshToken) runs; then the token no longer rotates (Refresh -> ErrInvalidRefreshToken) AND a second Logout on the same (or unknown) token returns nil — logout is idempotent.** `[服务+mock仓储·MVP]` — Logout/LogoutAll idempotency at the USER-service level was omitted (the sessions concern covered Revoke/RevokeAll on the session service, but not the user-service Logout wrapper wiring + idempotency). Idempotency matters so a retry/double-click never errors.  
  <sub>TestService_Logout_RevokesRefresh (service_test.go:711-727); service.go Logout:419-421, LogoutAll:427-429</sub>

### display-name / profile edit

- [ ] **given an authenticated user; when UpdateDisplayName(userID, name) runs; then the name is trimmed and persisted (a subsequent GetByID reflects it); a whitespace-only name -> ErrInvalidDisplayName and the stored name is UNCHANGED; an '<img ...>' name -> ErrDisplayNameForbiddenChars and UNCHANGED; a missing user -> ErrNotFound.** `[服务+mock仓储·MVP]` — UpdateDisplayName wires the SAME validator as Register but on the edit path, and asserts no-mutation-on-rejection. The union only covered Register's wiring; the edit endpoint (and its unchanged-on-reject guarantee) was omitted.  
  <sub>TestService_UpdateDisplayName (service_test.go:784-822); service.go UpdateDisplayName:514-523</sub>

### sessions / handler

- [ ] **given a successful register/login/refresh; when the handler sets the refresh cookie; then the cookie is HttpOnly=true, Secure=true (when configured), SameSite=Strict, and scoped to the refresh Path — so JS cannot read it, it is not sent cross-site, and it is only sent to the refresh/logout endpoints.** `[handler层·MVP]` — SECURITY (major omission): the refresh-token cookie hardening is the primary XSS/CSRF defense for the whole session scheme and is explicitly go-tested, yet no assertion in the union covers cookie attributes. Rust must set these on its Set-Cookie.  
  <sub>TestHandler_RefreshCookieAttributes (handler_test.go:156-187); handler.go refreshCookieName/refreshCookiePath</sub>
- [ ] **given any auth response (register/login/refresh); when the body is serialized; then the refresh token appears ONLY in the Set-Cookie header and NEVER in the JSON body (the body carries access_token but no refresh_token key).** `[handler层·MVP]` — SECURITY: a refresh token in the response body would be readable by JS and logged/cached, defeating the HttpOnly-cookie design. Explicitly go-tested ('refresh_token leaked into response body'). Missed.  
  <sub>TestHandler_Refresh (handler_test.go:454-456)</sub>

### login-code / password-reset / handler

- [ ] **given SendCode (request login code) or ForgotPassword (request reset code); when called with an identifier that HAS an account and again with one that does NOT; then BOTH return HTTP 200 — the response is identical, so the endpoint cannot be used to probe which identifiers are registered.** `[handler层·MVP]` — PRIVACY: this is the account-enumeration defense at the HTTP boundary (the service-level 'no existence leak' was asserted, but the handler-observable 'always 200 for known and unknown' is the actual attacker-facing contract). Explicitly go-tested. Missed.  
  <sub>TestHandler_SendCodeAndLoginCode (handler_test.go:262-268), TestHandler_ForgotAndResetPassword (handler_test.go:301-307)</sub>

### OTP / handler

- [ ] **given the code service returns ErrRateLimited from RequestCode; when the SendCode/ForgotPassword/RequestAccountDeletion handler runs; then it maps to HTTP 429 Too Many Requests.** `[handler层·MVP]` — The rate-limit outcome must reach the client as 429 (not 500/200) so clients back off; ErrRateLimited is the only non-generic error the code request path surfaces. The OTP concern asserted ErrRateLimited internally but not its HTTP mapping. Missed.  
  <sub>TestHandler_SendCode_RateLimited (handler_test.go:287-294), TestHandler_ForgotPassword_RateLimited (handler_test.go:344-351)</sub>

### login-code / handler

- [ ] **given a code-login request; when the code is wrong; then the handler returns 401; when the code field is missing; then it returns 400 (binding). LoginCode wrong-code stays 401 (indistinguishable from unknown user), missing-code is a 400 input error.** `[handler层·MVP]` — The handler-level status mapping for code login (401 vs 400) was omitted; distinguishes an auth failure from a malformed request while keeping wrong-code indistinguishable from unknown-account. Missed.  
  <sub>TestHandler_SendCodeAndLoginCode (handler_test.go:274-284)</sub>

### OTP

- [ ] **given store.Save succeeds but sender.Send returns an error; when RequestCode runs; then it returns an error wrapping the send failure (the caller learns delivery failed).** `[服务+mock仓储·延后]` — The union LISTED request-send-error-propagates but flagged it as 'no dedicated go test'. Confirm it is grounded in service.go:100-102 and keep it — but note the union's OTP concern is correct here; this entry is a reminder it has no go test and is a design constraint, not a regression. (Included so the synthesizer keeps it, low priority.)  
  <sub>otp/service.go:100-102 (send code error branch); no dedicated go test</sub>

### account-status / admin

- [ ] **given SetStatus(disabled) is applied to a user with active sessions; when it runs; then it does NOT force-revoke existing refresh tokens — enforcement happens lazily at the next login/refresh (a disabled user's still-valid refresh token is rejected on its next Refresh within one access-token TTL).** `[服务+mock仓储·延后]` — Pins the EXACT enforcement model the reference doc states ('失效在一个 access-token TTL 内生效') and clarifies disable is a boundary check, not an eager session kill. The union covered refresh-disabled-rejected but not the 'sessions are not eagerly revoked' half. mvp=false (SetStatus is admin-domain) but the enforcement semantics gate the MVP refresh path.  
  <sub>admin.go SetStatus:46-60 ('active sessions are not force-revoked here'); reference doc section 0 '失效在一个 access-token TTL 内生效'</sub>

### identity / normalization

- [ ] **given RequestLoginCode/RequestPasswordReset/LoginCode/ResetPassword; when the identifier is passed with mixed case or surrounding whitespace; then the code target is normalizeIdentifier(identifier) (email lowercased+trimmed, phone trimmed) so request-time and verify-time resolve to the SAME target ('Reset@Example.com' -> 'reset@example.com').** `[纯函数·延后]` — The union asserted normalization for login/code-login, but not that the reset/deletion/bind request-and-verify pair use the identical normalization (a mismatch would make a legitimate code never resolve). Tested via ResetPassword_ViaEmail sending to normalized target. Partially missed.  
  <sub>TestService_ResetPassword_ViaEmail (service_test.go:657-679, requests 'Reset@Example.com' -> code recorded under 'reset@example.com'); service.go normalizeIdentifier:690-695 used on every request+verify</sub>

### users / user_roles schema

- [ ] **given the users table; when a row sets last_active_role or user_roles.role to a value outside {student,teacher}, or status outside {active,disabled}; then the CHECK constraint rejects it at the DB.** `[契约·真库·MVP]` — The reference doc lists these CHECKs as the DB backstop behind the app-layer Role/Status validators. Worth ONE contract/schema assertion so the Rust sqlx enum<->TEXT mapping is verified to reject unknown values at the DB. Partially implied but not enumerated as its own assertion.  
  <sub>migration 20260709075649 (status CHECK, last_active_role CHECK), 20260709083746 (role CHECK); reference doc section 1/2</sub>

---

## H. 修正与设计缺口（批判层对上面断言的纠偏，务必读）

1. **handler-login-disabled-maps-to-403 (Login concern) — union WHY says 'I asserted it from the go service doc comments, but there is no go handler_test line cited for these two specific mappings'**  
   - 问题：The 403-for-disabled mapping IS directly go-tested at the handler for BOTH password login and code login, contradicting the union's claim that it is only 'design intent'. The union under-grounded a real, tested assertion.  
   - 修正：Re-cite to TestHandler_Login_Disabled (handler_test.go:1040-1081): password login -> 403 with error 'account disabled'; code login -> 403 with error 'account disabled'. Treat as a firm, go-tested handler contract (mvp=true), not merely design intent to confirm.

2. **handler-refresh-disabled-maps-to-401-clear-cookie (Login concern)**  
   - 问题：The 'clears the refresh cookie on 401' half is asserted but not clearly grounded in a cited go handler test; TestHandler_Refresh covers disabled/replayed/unknown/missing -> 401 but the union's cite is only a service.go comment. Also this overlaps the sessions/handler concern.  
   - 修正：Ground the 401 mapping in TestHandler_Refresh (handler_test.go:462-473). Keep the cookie-clear as design intent (verify against the Rust handler) but split it out; do not conflate the tested 401 with the untested cookie-clear.

3. **contract-default-status-and-active-role-after-create / contract-set-status-disabled-persists / contract-set-active-role-persists / student-profile-grade-default-empty / student-profile-pk-one-to-one / teacher-profile-* / register-db-check-rejects-both-null / paired-check-defense-in-depth / store-token-hash-unique**  
   - 问题：These are pure DB-constraint / column-default round-trip tests, not service-layer behavior. They belong to (and are largely already covered by) the schema tests (tests/*_schema.rs) — e.g. CHECK acceptance/rejection, DEFAULT values, PK uniqueness, partial-unique-index semantics, the paired-null CHECK, the token_hash UNIQUE index.  
   - 修正：Tag them explicitly as schema/DB-constraint assertions (verified in tests/*_schema.rs), NOT service-unit or service-behavior. Keep them as 'contract' but note the service tests should NOT re-assert the raw constraint — the service tests should only assert the service's MAPPING of a constraint violation to a domain error (e.g. 23505 on users_phone_key -> ErrPhoneTaken). Otherwise they duplicate schema coverage.

4. **register-duplicate-phone-conflict / register-duplicate-email-case-insensitive-conflict**  
   - 问题：The union tags these 'contract' and grounds the pg-error->error mapping in go's repository.go constraint-NAME substring match. The go constraint names differ from the Rust migration names (users_phone_key / users_email_key), so the mapping logic is NOT a 1:1 port — the assertion as phrased risks copying go's substring against the wrong name.  
   - 修正：Add an explicit note (already partly in union notes) that the Rust sqlx layer must derive ErrPhoneTaken/ErrEmailTaken from the RUST index names (users_phone_key / users_email_key), and the service-layer assertion should test the DOMAIN ERROR surfaced, with the name-mapping as a repo-layer detail. Keep the case-insensitive email conflict as genuinely service-relevant (app lowercases before write).

5. **verify-cap-is-five-exactly (OTP concern)**  
   - 问题：Phrasing 'after 5 recorded wrong attempts the code is locked' is slightly imprecise about the boundary. The code checks Attempts >= maxVerifyAttempts BEFORE the compare, and IncrementAttempts runs only on a wrong compare. So: 5 wrong guesses are each processed (attempts 0->5), and the lock takes effect on the 6th verify attempt (or the correct code presented after 5 wrong). The attacker budget is exactly 5 wrong guesses per code.  
   - 修正：Restate precisely: 'A code permits at most 5 wrong guesses; the 6th verification (or a correct code after 5 failed) is rejected because Attempts>=5 is checked before the compare. A correct guess among the first 5 still succeeds.' Grounded in service.go:147-157 + TestService_Verify_AttemptLimit loop of exactly maxVerifyAttempts.

6. **identifier-classified-email-by-at-sign (Registration concern) vs pw-login-by-phone/pw-login-by-email + code-login-normalizes-identifier-for-verify (Login concern)**  
   - 问题：Cross-concern duplicate: the '@'-classification + normalize-lowercase/trim rule is asserted three times (registration, login, code-login) plus mirrored in classifyContact (bind). It is one pure-fn behavior.  
   - 修正：De-dup into ONE pure-fn assertion for isEmail/normalizeIdentifier (service.go:687-703) owned by an 'identity/normalization' concern, and have login/register/reset/bind reference it rather than re-stating. Note bind's classifyContact ADDS format validation (mail.ParseAddress round-trip + phone 5-20) that the login-path normalize does NOT do — keep that distinction.

7. **handler-register-conflict-maps-409-with-field (Registration concern) marked mvp=false**  
   - 问题：The union marks the 409 duplicate mapping mvp=false calling the whole thing a 'response-shape enhancement'. But the 409 status itself (phone/email already registered) is core register correctness and IS go-tested; only the machine-readable 'field' key is the enhancement.  
   - 修正：Split: 409-on-duplicate = mvp=true (core, TestHandler_Register_Duplicate409 asserts 409). The 'field':'phone'/'email' machine-readable key = mvp=false (polish). Do not defer the whole 409 mapping.

8. **dual-key-distinct-secrets / reject-cross-realm-* (Auth token concern) labeled layer=service-unit / pure-fn**  
   - 问题：These are correctly mvp=false (admin realm deferred), but the union should more strongly flag the concrete Rust design gap: src/config.rs exposes a SINGLE jwt_secret, so as written the Rust code CANNOT satisfy cross-realm key separation until a second admin key is added. Risk of the assertion silently 'passing' with one shared key where only the realm-claim check separates realms (weaker than go).  
   - 修正：Keep mvp=false but elevate to a blocking design-gap note on the config: the web/admin two-key separation requires adding a second signing secret before these assertions are meaningful. Until then, only the realm-claim check exists (defense-in-depth alone, not the primary key boundary).

---

## I. 覆盖评估（批判层）

> Well-covered by the enumerate pass: Registration + input validation (display-name Cf/control/tag-char rules, normalization, bcrypt cost + 72-byte cap, partial-unique-index NULL semantics), password/code login with the full error-indistinguishability matrix, OTP (rejection-sampling codegen, 5-attempt cap, constant-time compare, cooldown/daily rate limits, purpose scoping, all-failures-one-error), refresh-token session mechanics (SHA-256-hash-only storage, single-device revoke-on-issue, rotation, theft/grace, generic-error collapse), JWT sign/verify (HS256 pinning, none-alg rejection, realm claim, expiry), and learning-settings paired-write + derived-onboarded. These concerns are strong.
> 
> Thinly covered or MISSING (biggest gaps): (1) Three entire service flows were omitted — ResetPassword, DeleteAccount, and BindContact — despite ~18 dedicated go tests among them; they are mvp=false (deferred purposes) but the task requires listing them, and each carries PRIVACY assertions (unknown-identifier->generic reset error, cross-channel code binding, code-to-own-contact proof-of-ownership, channel-unavailable guard, availability re-check before code consumption). (2) Logout/LogoutAll idempotency and UpdateDisplayName-on-edit (no-mutation-on-reject) at the user-service level. (3) HANDLER-layer security constraints were badly under-covered: the refresh cookie hardening (HttpOnly + Secure + SameSite=Strict + Path-scoped) and the 'refresh token is cookie-only, never in the JSON body' rule are both go-tested and both absent — these are the primary XSS/CSRF defenses of the whole session scheme and should be mvp=true. (4) The account-enumeration defense AT THE HTTP BOUNDARY (SendCode/ForgotPassword always return 200 for known and unknown identifiers alike) and the ErrRateLimited->429 mapping were missed. 
> 
> Privacy/security constraints from the reference doc NOT yet asserted: the refresh-cookie attribute set (section 5 implied, the strongest gap), the 'always-200' existence-non-leak at the request-code endpoints, the reset/delete/bind indistinguishable-code errors, and the precise disabled-account enforcement MODEL (lazy, at login/refresh boundary within one TTL — SetStatus does not eagerly revoke sessions). 
> 
> Corrections: the disabled->403 handler mapping was under-grounded (it IS go-tested, TestHandler_Login_Disabled); many 'contract' assertions are pure DB-constraint/schema tests that duplicate tests/*_schema.rs and should be retagged so the service tests only assert the constraint-violation->domain-error mapping, not the raw CHECK/UNIQUE/DEFAULT; the '@'-classify+normalize rule is triplicated across concerns and should be a single pure-fn assertion; the 409-duplicate mapping should be mvp=true with only the 'field' key deferred; and the OTP attempt-cap boundary phrasing needs tightening (exactly 5 wrong guesses, checked before compare).
