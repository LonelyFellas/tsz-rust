use axum::{Json, extract::State, http::header};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

pub mod errors;
pub mod notifications;
pub mod output;

use crate::{
    api::ApiQuery,
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum InterfaceLocaleV1 {
    #[serde(rename = "zh-Hans")]
    #[sqlx(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "zh-Hant")]
    #[sqlx(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "en-GB")]
    #[sqlx(rename = "en-GB")]
    EnGb,
    #[serde(rename = "en-US")]
    #[sqlx(rename = "en-US")]
    EnUs,
}

impl InterfaceLocaleV1 {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "zh-Hans" => Some(Self::ZhHans),
            "zh-Hant" => Some(Self::ZhHant),
            "en-GB" => Some(Self::EnGb),
            "en-US" => Some(Self::EnUs),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for InterfaceLocaleV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("invalid interface locale"))
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CapabilitiesQuery {
    pub surface: InterfaceSurface,
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceSurface {
    Web,
    Admin,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InterfaceCapabilities {
    #[schema(value_type = u8, minimum = 1, maximum = 1)]
    pub schema_version: u8,
    #[schema(value_type = u8, minimum = 1, maximum = 1)]
    pub preference_version: u8,
    pub default_locale: InterfaceLocaleV1,
    pub supported_locales: Vec<InterfaceLocaleV1>,
}

impl Default for InterfaceCapabilities {
    fn default() -> Self {
        Self {
            schema_version: 1,
            preference_version: 1,
            default_locale: InterfaceLocaleV1::ZhHans,
            supported_locales: vec![InterfaceLocaleV1::ZhHans],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InterfaceLanguages {
    pub web: InterfaceCapabilities,
    pub admin: InterfaceCapabilities,
}

impl InterfaceLanguages {
    pub fn for_surface(&self, surface: InterfaceSurface) -> &InterfaceCapabilities {
        match surface {
            InterfaceSurface::Web => &self.web,
            InterfaceSurface::Admin => &self.admin,
        }
    }

    pub fn from_pairs(pairs: &[(String, String)]) -> Result<Self, envy::Error> {
        let read = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        };
        let parse = |surface: &str| -> Result<InterfaceCapabilities, envy::Error> {
            let invalid =
                || envy::Error::Custom(format!("invalid I18N_{surface} language configuration"));
            let locales = read(&format!("I18N_{surface}_SUPPORTED_LOCALES")).unwrap_or("zh-Hans");
            let supported_locales = locales
                .split(',')
                .map(|s| InterfaceLocaleV1::parse(s.trim()).ok_or_else(invalid))
                .collect::<Result<Vec<_>, _>>()?;
            let default_locale = InterfaceLocaleV1::parse(
                read(&format!("I18N_{surface}_DEFAULT_LOCALE")).unwrap_or("zh-Hans"),
            )
            .ok_or_else(invalid)?;
            if !supported_locales.contains(&default_locale)
                || supported_locales
                    .iter()
                    .enumerate()
                    .any(|(i, l)| supported_locales[..i].contains(l))
            {
                return Err(invalid());
            }
            Ok(InterfaceCapabilities {
                default_locale,
                supported_locales,
                ..Default::default()
            })
        };
        Ok(Self {
            web: parse("WEB")?,
            admin: parse("ADMIN")?,
        })
    }

    pub fn require_supported(
        &self,
        surface: InterfaceSurface,
        locale: Option<InterfaceLocaleV1>,
    ) -> Result<(), AppError> {
        if locale.is_some_and(|value| !self.for_surface(surface).supported_locales.contains(&value))
        {
            return Err(AppError::unprocessable(
                ErrorCode::UnsupportedLanguage,
                "interface language is not enabled",
            ));
        }
        Ok(())
    }
}

/// 区分缺字段与显式 null，用于按字段更新。
pub fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[utoipa::path(get, path = "/api/v1/i18n/capabilities", tag = "i18n", params(CapabilitiesQuery),
    responses((status = 200, body = InterfaceCapabilities), (status = 400, description = "invalid_query")))]
pub async fn capabilities(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<CapabilitiesQuery>,
) -> impl axum::response::IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.interface_languages.for_surface(query.surface).clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::OpenApi;

    #[test]
    fn capabilities_schema_declares_resolvable_surface_query() {
        let spec = serde_json::to_value(crate::openapi::ApiDoc::openapi()).unwrap();
        let surface = spec["paths"]["/api/v1/i18n/capabilities"]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "surface")
            .unwrap();
        assert_eq!(surface["in"], "query");
        assert_eq!(surface["required"], true);
        assert_eq!(
            surface["schema"]["$ref"],
            "#/components/schemas/InterfaceSurface"
        );
        assert_eq!(
            spec["components"]["schemas"]["InterfaceSurface"]["enum"],
            serde_json::json!(["web", "admin"])
        );
    }
    #[test]
    fn configuration_is_strict_and_surfaces_are_independent() {
        for value in ["", "zh-CN", "en", "zh-Hans,zh-Hans", "ja"] {
            assert!(
                InterfaceLanguages::from_pairs(&[(
                    "I18N_WEB_SUPPORTED_LOCALES".into(),
                    value.into()
                )])
                .is_err()
            );
        }
        assert!(
            InterfaceLanguages::from_pairs(&[("I18N_WEB_DEFAULT_LOCALE".into(), "en-US".into())])
                .is_err()
        );
        let config = InterfaceLanguages::from_pairs(&[(
            "I18N_ADMIN_SUPPORTED_LOCALES".into(),
            "zh-Hans,en-US".into(),
        )])
        .unwrap();
        assert!(
            config
                .require_supported(InterfaceSurface::Web, Some(InterfaceLocaleV1::EnUs))
                .is_err()
        );
        assert!(
            config
                .require_supported(InterfaceSurface::Admin, Some(InterfaceLocaleV1::EnUs))
                .is_ok()
        );
        for value in [
            serde_json::json!({"en-US":null}),
            serde_json::json!("en-us"),
        ] {
            assert!(serde_json::from_value::<InterfaceLocaleV1>(value).is_err());
        }
    }
}
