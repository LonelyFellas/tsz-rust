use rand::RngExt;
use rand::seq::IndexedRandom;
use uuid::Uuid;

const ADJECTIVES: &[&str] = &[
    "勤奋", "阳光", "机智", "沉稳", "聪明", "勇敢", "善良", "美丽", "帅气", "优雅", "大方", "自信",
    "乐观", "开朗", "幽默", "风趣", "机灵", "灵活", "敏捷", "迅速", "果断", "坚定", "执着", "努力",
    "奋斗", "积极", "向上", "热情", "温柔", "细腻", "真诚", "厚道", "正直", "忠诚", "可靠", "踏实",
    "稳重", "冷静", "从容", "淡定", "睿智", "博学", "精明", "干练", "活力", "青春", "活泼", "可爱",
    "甜美", "清新", "自然", "朴实", "谦虚", "低调", "进取", "创新", "独特", "非凡", "卓越", "优秀",
    "杰出", "出色", "精彩", "耀眼", "闪亮", "明亮", "温暖", "贴心", "细心", "周到", "体贴", "浪漫",
    "自由", "随性", "洒脱", "文艺", "知性", "理性", "感性", "敏锐", "犀利", "精准", "专注", "认真",
    "严谨", "耐心", "恒心", "毅力", "坚强", "独立", "自强", "谦逊", "慷慨", "豪爽", "爽朗", "健谈",
    "有趣", "灵动", "豁达", "潇洒",
];
const NOUNS: &[&str] = &[
    "松鼠",
    "海豚",
    "考拉",
    "狐狸",
    "熊猫",
    "刺猬",
    "猫头鹰",
    "兔子",
    "猫咪",
    "小狗",
    "小熊",
    "小象",
    "企鹅",
    "河马",
    "斑马",
    "羚羊",
    "小鹿",
    "麋鹿",
    "犀牛",
    "水牛",
    "骆驼",
    "绵羊",
    "山羊",
    "小猪",
    "老虎",
    "狮子",
    "豹子",
    "雪豹",
    "猎豹",
    "灰狼",
    "浣熊",
    "水獭",
    "海獭",
    "海狮",
    "海豹",
    "鲸鱼",
    "虎鲸",
    "章鱼",
    "乌贼",
    "螃蟹",
    "龙虾",
    "海龟",
    "海星",
    "水母",
    "蝴蝶",
    "蜜蜂",
    "蜻蜓",
    "瓢虫",
    "蚂蚁",
    "萤火虫",
    "鹦鹉",
    "孔雀",
    "天鹅",
    "鸭子",
    "鸽子",
    "老鹰",
    "燕子",
    "麻雀",
    "啄木鸟",
    "翠鸟",
    "乌鸦",
    "喜鹊",
    "黄鹂",
    "鹌鹑",
    "火烈鸟",
    "袋鼠",
    "树懒",
    "水豚",
    "土拨鼠",
    "仓鼠",
    "龙猫",
    "鼹鼠",
    "蝙蝠",
    "海牛",
    "鸭嘴兽",
    "小熊猫",
    "金丝猴",
    "大猩猩",
    "狒狒",
    "猴子",
    "长颈鹿",
    "牦牛",
    "野牛",
    "驴子",
    "马儿",
    "豪猪",
    "白兔",
    "红狐",
    "银狐",
    "雪兔",
    "野兔",
    "野鸭",
    "野猫",
    "獾",
    "黄鼠狼",
    "林麝",
    "貂熊",
    "信天翁",
    "布谷鸟",
    "金丝雀",
];

fn generate_display_name() -> String {
    let mut rng = rand::rng();
    let adj = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    let num = rng.random_range(1..=999);
    format!("{adj}{noun}{num:04}")
}

use crate::user::{
    model::{Password, PasswordError, SubjectError, User, UserRole},
    repository::{NewUser, UserError, UserRepository},
};

fn normalize_phone(phone: &str) -> String {
    phone.trim().to_string()
}
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
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
        let password = Password::parse(&input.password)?;
        let password_hash = password.hash_password()?;

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
