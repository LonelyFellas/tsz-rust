use crate::lexicon::{
    dto::Dialect,
    normalization::{
        HEADWORD_NORMALIZATION_VERSION, HeadwordNormalizationError, normalize_headword,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSurfaceScope {
    pub surface: String,
    pub normalized_surface: String,
    pub normalization_version: i16,
    pub dialect: &'static str,
    pub dialect_scope: &'static str,
}

/// Normalize one explicitly persisted headword/form surface and expand its
/// query scopes. This intentionally performs no morphology inference: callers
/// must supply every form that exists in authoritative lexicon content.
pub fn normalize_surface_scopes(
    value: &str,
    dialect: Dialect,
) -> Result<Vec<NormalizedSurfaceScope>, HeadwordNormalizationError> {
    let normalized = normalize_headword(value)?;
    let (dialect, scopes): (&str, &[&str]) = match dialect {
        Dialect::Common => ("common", &["uk", "us"]),
        Dialect::Uk => ("uk", &["uk"]),
        Dialect::Us => ("us", &["us"]),
    };

    Ok(scopes
        .iter()
        .map(|dialect_scope| NormalizedSurfaceScope {
            surface: normalized.display.clone(),
            normalized_surface: normalized.key.clone(),
            normalization_version: HEADWORD_NORMALIZATION_VERSION,
            dialect,
            dialect_scope,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_surface_expands_to_uk_and_us_without_changing_display() {
        let scopes = normalize_surface_scopes("  Workspaces  ", Dialect::Common).unwrap();

        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0].surface, "Workspaces");
        assert_eq!(scopes[0].normalized_surface, "workspaces");
        assert_eq!(scopes[0].dialect, "common");
        assert_eq!(scopes[0].dialect_scope, "uk");
        assert_eq!(scopes[1].dialect_scope, "us");
    }

    #[test]
    fn dialect_specific_surface_has_only_its_authoritative_scope() {
        let uk = normalize_surface_scopes("centre", Dialect::Uk).unwrap();
        let us = normalize_surface_scopes("center", Dialect::Us).unwrap();

        assert_eq!(uk.len(), 1);
        assert_eq!(uk[0].dialect_scope, "uk");
        assert_eq!(us.len(), 1);
        assert_eq!(us[0].dialect_scope, "us");
    }

    #[test]
    fn normalization_does_not_infer_unstored_english_forms() {
        let scopes = normalize_surface_scopes("workspace", Dialect::Common).unwrap();

        assert!(
            scopes
                .iter()
                .all(|scope| scope.normalized_surface == "workspace")
        );
        assert!(
            scopes
                .iter()
                .all(|scope| scope.normalized_surface != "workspaces")
        );
    }
}
