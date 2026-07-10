use unicode_general_category::{GeneralCategory, get_general_category};

pub fn normalize_phone(phone: &str) -> String {
    phone.trim().to_string()
}
pub fn normalize_email(email: &str) -> String {
    email.trim().to_string()
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum DisplayNameError {
    #[error("display name cannot be empty")]
    Empty,
    #[error("display name cannot be longer than 50 characters")]
    TooLong,
    #[error("display name contains forbidden characters")]
    ForbiddenCharacters,
}

pub struct DisplayName(String);

impl DisplayName {
    pub fn parse(raw: &str) -> Result<Self, DisplayNameError> {
        // 1) trim
        let display_name = raw.trim();
        // 2) empty
        if display_name.is_empty() {
            return Err(DisplayNameError::Empty);
        }
        // 3) 长度按chars().count()数，大于50 -> TooLong (中文昵称是3不是9)
        let size = display_name.chars().count();

        if size > 50 {
            return Err(DisplayNameError::TooLong);
        }

        // 4) 禁用字符：< >
        if display_name.contains(['<', '>']) {
            return Err(DisplayNameError::ForbiddenCharacters);
        }
        // 5) 含 Unicode Cf（零宽 \u{200b}、BOM \u{feff}、bidi \u{202e}）→ Forbidden
        if display_name
            .chars()
            .any(|c| c.is_control() || get_general_category(c) == GeneralCategory::Format)
        {
            return Err(DisplayNameError::ForbiddenCharacters);
        }

        Ok(DisplayName(display_name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
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

    use super::{DisplayName, DisplayNameError};

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
