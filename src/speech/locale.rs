use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

/// Native English synthesis locale. Accent grouping never changes this identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ToSchema)]
#[schema(value_type = String, pattern = "^en-[A-Z]{2}$")]
pub struct EnglishLocale([u8; 5]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccentGroup {
    Uk,
    Us,
    Other,
}

#[allow(non_upper_case_globals)]
impl EnglishLocale {
    pub const Uk: Self = Self(*b"en-GB");
    pub const Us: Self = Self(*b"en-US");
    pub const EnGb: Self = Self::Uk;
    pub const EnUs: Self = Self::Us;

    pub fn parse(value: &str) -> Option<Self> {
        let bytes: [u8; 5] = value.as_bytes().try_into().ok()?;
        (bytes[..3] == *b"en-" && bytes[3..].iter().all(u8::is_ascii_uppercase))
            .then_some(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("locale contains only validated ASCII")
    }

    pub fn accent_group(self) -> AccentGroup {
        match self.as_str() {
            "en-GB" | "en-AU" | "en-NZ" => AccentGroup::Uk,
            "en-US" | "en-CA" => AccentGroup::Us,
            _ => AccentGroup::Other,
        }
    }

    /// UK/US each select one version; supplemental regions select one per native locale.
    pub fn selection_key(self) -> Self {
        match self.accent_group() {
            AccentGroup::Uk => Self::Uk,
            AccentGroup::Us => Self::Us,
            AccentGroup::Other => self,
        }
    }

    pub fn is_legacy(self) -> bool {
        matches!(self, Self::Uk | Self::Us)
    }
}

impl Serialize for EnglishLocale {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EnglishLocale {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| serde::de::Error::custom("locale must match ^en-[A-Z]{2}$"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_english_identity_and_selection_groups_are_distinct() {
        for (locale, group, key) in [
            ("en-GB", AccentGroup::Uk, "en-GB"),
            ("en-AU", AccentGroup::Uk, "en-GB"),
            ("en-NZ", AccentGroup::Uk, "en-GB"),
            ("en-US", AccentGroup::Us, "en-US"),
            ("en-CA", AccentGroup::Us, "en-US"),
            ("en-IN", AccentGroup::Other, "en-IN"),
            ("en-IE", AccentGroup::Other, "en-IE"),
        ] {
            let parsed = EnglishLocale::parse(locale).unwrap();
            assert_eq!(parsed.accent_group(), group);
            assert_eq!(parsed.selection_key().as_str(), key);
            assert_eq!(serde_json::to_value(parsed).unwrap(), locale);
        }
        for invalid in ["en", "en-us", "en-USA", "en-US-extra", "zh-CN", "other"] {
            assert!(EnglishLocale::parse(invalid).is_none());
            assert!(serde_json::from_value::<EnglishLocale>(serde_json::json!(invalid)).is_err());
        }
    }
}
