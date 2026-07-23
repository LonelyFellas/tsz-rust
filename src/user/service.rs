use uuid::Uuid;

use crate::platform::{
    Password, PasswordError, dummy_hash, hash_password, normalize_email, normalize_phone,
    verify_password,
};
use crate::user::display_name::generate_display_name;
use crate::user::model::UserStatus;
use crate::user::{
    model::{SubjectError, User, UserRole},
    repository::{NewUser, UserError, UserRepository},
};

pub fn normalize_identifier(identifier: &str) -> String {
    if identifier.contains('@') {
        normalize_email(identifier)
    } else {
        normalize_phone(identifier)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct RegisterInput {
    pub phone: Option<String>,
    pub email: Option<String>,
    pub password: String,
    // pub code: String,
}

pub struct UserService {
    repository: UserRepository,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error(transparent)]
    Register(#[from] SubjectError),
    #[error(transparent)]
    Password(#[from] PasswordError),
    // #[error(transparent)]
    // Code(#[from] CodeError),
    #[error(transparent)]
    Repository(#[from] UserError),
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("invalid credentials")]
    InvalidCredentials, // 用户不存在或者密码不正确
    #[error("account is disabled")]
    AccountDisabled,
    #[error(transparent)]
    Repository(#[from] UserError),
}

impl UserService {
    pub fn new(repository: UserRepository) -> Self {
        Self { repository }
    }

    /// 注册用户
    pub async fn register(&self, input: RegisterInput) -> Result<User, RegisterError> {
        // 1) 手机号 / 邮箱
        let phone = input
            .phone
            .as_deref()
            .map(normalize_phone)
            .filter(|s| !s.is_empty());
        let email = input
            .email
            .as_deref()
            .map(normalize_email)
            .filter(|s| !s.is_empty());
        // 1.1) phone or email empty
        if phone.is_none() && email.is_none() {
            return Err(RegisterError::Register(SubjectError::PhoneOrEmailMissing));
        }

        // 2) 密码哈希
        let password = Password::parse(&input.password).map_err(RegisterError::Password)?;
        let password_hash = hash_password(password.into_string()).await?;

        // 3) 验证码校验
        // let _ = Code::parse(&input.code)?;

        // TODO: 验证码验

        // 3) create user
        self.repository
            .create(NewUser {
                id: Uuid::now_v7(),
                phone,
                email,
                password_hash,
                display_name: generate_display_name(),
                first_role: UserRole::Student,
            })
            .await
            .map_err(|e| match e {
                UserError::PhoneNumberAlreadyExists | UserError::EmailAlreadyExists => {
                    RegisterError::Register(SubjectError::UserAlreadyExists)
                }
                other => RegisterError::Repository(other),
            })
    }

    pub async fn authenticate(&self, identifier: &str, password: &str) -> Result<User, LoginError> {
        let id = normalize_identifier(identifier);

        match self.repository.get_by_identifier(&id).await {
            Ok(user) => {
                // 先验证密码
                if !verify_password(password.to_string(), user.password_hash.clone()).await {
                    return Err(LoginError::InvalidCredentials);
                }
                // 密码对了，身份已证实 -> 再看状态（禁用只透传给证明了拥有改账号的人）
                if user.status == UserStatus::Disabled {
                    return Err(LoginError::AccountDisabled);
                }
                Ok(user)
            }
            // 查无此人：也跑一次假 verify 平衡时序，然后返回【和密码错同一个】错误
            Err(UserError::NotFound) => {
                verify_password(password.to_string(), dummy_hash().to_string()).await;
                Err(LoginError::InvalidCredentials)
            }
            // 其余底层错 -> 如实透传
            Err(e) => Err(LoginError::Repository(e)),
        }
    }
    pub async fn find_active_by_identifier(&self, id: &str) -> Result<User, LoginError> {
        match self.repository.get_by_identifier(id).await {
            Ok(user) if user.status != UserStatus::Active => Err(LoginError::AccountDisabled),
            Ok(user) => Ok(user),
            Err(UserError::NotFound) => Err(LoginError::InvalidCredentials),
            Err(e) => Err(LoginError::Repository(e)),
        }
    }
}

#[cfg(test)]
mod default_display_name_tests {
    //! `generate_display_name` 的规格测试（随机生成的默认昵称）。
    //! 随机值没法断言确切内容，只能断言**性质**（property-based 思路）。

    use super::generate_display_name;
    use crate::user::model::DisplayName;
    use std::collections::HashSet;

    /// 生成的默认昵称必须**天然合法**：非空、能过 DisplayName 全部规则。
    /// 跑 100 次覆盖不同随机组合，避免某个词表项/数字恰好越界没被发现。
    #[test]
    fn generated_name_is_always_a_valid_display_name() {
        for _ in 0..100 {
            let name = generate_display_name();
            assert!(!name.is_empty(), "默认昵称不应为空");
            assert!(
                DisplayName::parse(&name).is_ok(),
                "生成的默认昵称应满足 DisplayName 规则：{name}"
            );
        }
    }

    /// 随机性：大量生成应基本互不相同（576 万组合，200 次碰撞概率极低）。
    /// 若生成器退化成常量或随机源坏了，这条会挂。
    #[test]
    fn generated_names_are_mostly_unique() {
        let set: HashSet<String> = (0..200).map(|_| generate_display_name()).collect();
        assert!(
            set.len() >= 190,
            "200 次生成应基本不重复，实际唯一数：{}",
            set.len()
        );
    }
}

#[cfg(test)]
mod display_name_tests {
    //! `DisplayName::parse` 的规格测试（纯函数，无需 DB）。
    //!
    //! 来源：user-service-test-checklist §A（display_name 校验）+ §0 第 9 条
    //! （display_name 防注入不变量）。对齐 go 的 rune 语义：**长度按 Unicode
    //! code point（Rust `char`）数，不是字节**。
    //!
    //! 只依赖 `DisplayName::parse` 与 `DisplayName::as_str`——错误分支用 `matches!`
    //! 比对，成功分支比 `as_str()`，不强制 `DisplayName` 实现 `PartialEq`。

    use crate::user::model::{DisplayName, DisplayNameError};

    // ——————————————— 放行路径 ———————————————

    /// trim 两端空白后校验，存进去的是 trim 后的值。
    #[test]
    fn accepts_and_trims_plain_name() {
        let d = DisplayName::parse("  Alice  ").expect("普通昵称应通过");
        assert_eq!(d.as_str(), "Alice", "两端空白应被 trim 掉");
    }

    /// `'` `"` `&` 在没有 `<` `>` 时构不成标签，是合法昵称的一部分
    /// （O'Brien、Tom&Jerry、a"b），不能误杀。
    #[test]
    fn accepts_apostrophe_quote_ampersand() {
        for name in ["O'Brien", "Tom&Jerry", "a\"b"] {
            let d =
                DisplayName::parse(name).unwrap_or_else(|e| panic!("{name:?} 应放行，却报 {e:?}"));
            assert_eq!(d.as_str(), name, "放行的名字应原样保留");
        }
    }

    /// 多语言昵称合法：非 ASCII 字母原样放行，校验不能是 ASCII-only。
    #[test]
    fn accepts_non_ascii_letters() {
        for name in ["新昵称", "他/她", "Björk"] {
            let d =
                DisplayName::parse(name).unwrap_or_else(|e| panic!("{name:?} 应放行，却报 {e:?}"));
            assert_eq!(d.as_str(), name);
        }
    }

    // ——————————————— Empty（trim 后为空）———————————————

    /// trim 后为空（含纯空白、纯制表/换行）→ Empty；长度下限 1 由这条覆盖。
    /// binding 层的 min=1 只能拦原始长度，拦不住 trim 后为空，故这里必须拦。
    #[test]
    fn blank_after_trim_is_rejected() {
        for raw in ["", "   ", "\t\n "] {
            assert!(
                matches!(DisplayName::parse(raw), Err(DisplayNameError::Empty)),
                "空 / 纯空白应判 Empty：{raw:?}"
            );
        }
    }

    // ——————————————— 长度：按 char 数，不是 byte ———————————————

    /// 50 字符是上界，恰好 50 应通过。
    #[test]
    fn accepts_exactly_50_chars() {
        let name = "a".repeat(50);
        let d = DisplayName::parse(&name).expect("恰好 50 字符应通过");
        assert_eq!(d.as_str().chars().count(), 50);
    }

    /// 超过 50 字符 → TooLong。
    #[test]
    fn rejects_more_than_50_chars() {
        let name = "a".repeat(51);
        assert!(
            matches!(DisplayName::parse(&name), Err(DisplayNameError::TooLong)),
            "51 字符应判 TooLong"
        );
    }

    /// 长度按 Unicode `char` 数、不是字节数：50 个中文（150 字节）应通过。
    /// 若误用 `raw.len()`（字节）会得到 150 > 50 而错判 TooLong——这条专门网住它。
    #[test]
    fn length_counts_chars_not_bytes() {
        let name = "字".repeat(50); // 50 char，150 byte
        assert_eq!(name.len(), 150, "前提：50 个中文是 150 字节");
        let d = DisplayName::parse(&name).expect("50 个中文字符应通过（按 char 数）");
        assert_eq!(d.as_str().chars().count(), 50);
    }

    /// 长度应在 **trim 之后**再数：两端各垫 3 个空格、中间恰好 50 字符 → 通过。
    #[test]
    fn length_measured_after_trim() {
        let name = format!("   {}   ", "a".repeat(50));
        let d = DisplayName::parse(&name).expect("trim 后恰好 50 字符应通过");
        assert_eq!(d.as_str().chars().count(), 50);
    }

    // ——————————————— 禁用字符：< > ———————————————

    /// 出现 `<` 或 `>` 于任意位置 → ForbiddenCharacters（防 XSS 标签）。
    #[test]
    fn rejects_angle_brackets() {
        for bad in ["<script>", "a<b", "a>b", "<", ">"] {
            assert!(
                matches!(
                    DisplayName::parse(bad),
                    Err(DisplayNameError::ForbiddenCharacters)
                ),
                "含尖括号应判 ForbiddenCharacters：{bad:?}"
            );
        }
    }

    // ——————————————— 禁用字符：控制符 ———————————————

    /// 控制符（NUL / 换行 / 制表 / 回车，`char::is_control`）→ ForbiddenCharacters。
    /// NUL 会让 Postgres 报编码错(500)，控制字符会破坏渲染，必须在此拦下。
    #[test]
    fn rejects_control_chars() {
        for bad in ["a\u{0}b", "a\nb", "a\tb", "a\rb"] {
            assert!(
                matches!(
                    DisplayName::parse(bad),
                    Err(DisplayNameError::ForbiddenCharacters)
                ),
                "含控制符应判 ForbiddenCharacters：{bad:?}"
            );
        }
    }

    // ——————————————— 禁用字符：Unicode Cf（格式类）———————————————

    /// Unicode General_Category = Cf 的字符（零宽空格、BOM、bidi 标记/override）
    /// → ForbiddenCharacters。这类字符能通过 trim，却造成视觉空白/错乱或 bidi 攻击。
    ///
    /// ⚠️ 实现要点：std **没有**直接的 Cf 判定（`char::is_control` 不含 Cf，
    /// 这批字符都不是 control，`is_ascii_control` 更不含它们）。需按 Unicode
    /// General_Category 判断——可引 `unicode-properties` 或
    /// `unicode-general-category` crate；别只硬编码下面这几个字符
    /// （测试只取代表样本，硬编码能过测试但会漏其它 Cf）。
    #[test]
    fn rejects_unicode_cf() {
        for bad in [
            "a\u{200b}b",  // ZERO WIDTH SPACE
            "\u{feff}foo", // BOM / ZERO WIDTH NO-BREAK SPACE
            "a\u{202e}b",  // RIGHT-TO-LEFT OVERRIDE（bidi 攻击）
            "a\u{200f}b",  // RIGHT-TO-LEFT MARK
        ] {
            assert!(
                matches!(
                    DisplayName::parse(bad),
                    Err(DisplayNameError::ForbiddenCharacters)
                ),
                "含 Unicode Cf 应判 ForbiddenCharacters：{bad:?}"
            );
        }
    }
}
