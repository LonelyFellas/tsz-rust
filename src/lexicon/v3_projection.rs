//! Pure Smart Lexicon V3 presentation and form-variant surface projection builders.
//!
//! Presentation uses base forms in wire order; every concrete form contributes searchable surfaces.
//! Draft variants with blank spellings are valid incomplete nodes, but do not produce presentation
//! or searchable surfaces until a spelling is supplied.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::lexicon::{
    dto::{
        Dialect, DialectModeV3, DraftFormsStepContentV3, EntryNameVariantV3, EntryPresentationV3,
        WordFormTypeV3, WordHeadwordsV2, WordRegionalVariantsV3,
    },
    normalization::{
        HEADWORD_NORMALIZATION_VERSION, HeadwordNormalizationError, normalize_headword,
    },
};

pub(crate) const NATIVE_PRESENTATION_STRATEGY_VERSION: &str = "surface_summary_v1";
pub(crate) const EMPTY_PRESENTATION_STRATEGY_VERSION: &str = "short_uuid_v1";
pub(crate) const V3_SURFACE_PROJECTION_VERSION: &str = "form_variant_surface_v1";

const UNNAMED_ENTRY_PREFIX: &str = "未命名词条 · ";
const SHORT_UUID_LENGTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum V3SurfaceDialect {
    Common,
    Uk,
    Us,
}

impl V3SurfaceDialect {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Uk => "uk",
            Self::Us => "us",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum V3SurfaceDialectScope {
    Uk,
    Us,
}

impl V3SurfaceDialectScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Uk => "uk",
            Self::Us => "us",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct V3FormVariantSurfaceSource {
    pub entry_id: Uuid,
    pub source_id: String,
    pub pos_id: Uuid,
    pub pos: String,
    pub group_ids: Vec<Uuid>,
    pub form_id: Uuid,
    pub variant_id: Uuid,
    pub form_type: WordFormTypeV3,
    pub dialect: V3SurfaceDialect,
    pub dialect_scope: V3SurfaceDialectScope,
    pub surface: String,
    pub normalized_surface: String,
    pub normalization_version: i16,
    pub projection_version: &'static str,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum V3ProjectionError {
    #[error("V3 form ID {form_id} is duplicated")]
    DuplicateForm { form_id: Uuid },
    #[error("V3 variant ID {variant_id} is duplicated")]
    DuplicateVariant { variant_id: Uuid },
    #[error("V3 group {group_id} in POS {pos_id} references unknown form {form_id}")]
    UnknownGroupMember {
        pos_id: Uuid,
        group_id: Uuid,
        form_id: Uuid,
    },
    #[error("V3 form {form_id} in POS {pos_id} has no active group membership")]
    OrphanForm { pos_id: Uuid, form_id: Uuid },
    #[error("V3 variant {variant_id} has an invalid surface")]
    InvalidSurface {
        variant_id: Uuid,
        #[source]
        source: HeadwordNormalizationError,
    },
}

pub(crate) fn presentation_from_native_forms(
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
) -> Result<EntryPresentationV3, V3ProjectionError> {
    let mut seen = HashSet::new();
    let mut matched_surfaces = Vec::new();
    for pos in &forms.pos {
        for form in &pos.forms {
            if form.form_type != "base" {
                continue;
            }
            for (variant_id, spelling) in regional_variant_spellings(&form.regional_variants) {
                let Some(normalized) = projectable_surface(variant_id, spelling)? else {
                    continue;
                };
                if seen.insert(normalized.key) {
                    matched_surfaces.push(normalized.display);
                }
            }
        }
    }
    let (label, strategy_version) = if matched_surfaces.is_empty() {
        (
            format!("{UNNAMED_ENTRY_PREFIX}{}", short_uuid(entry_id)),
            EMPTY_PRESENTATION_STRATEGY_VERSION,
        )
    } else {
        (
            matched_surfaces.join(" / "),
            NATIVE_PRESENTATION_STRATEGY_VERSION,
        )
    };
    Ok(EntryPresentationV3 {
        label,
        matched_surfaces,
        strategy_version: strategy_version.to_owned(),
    })
}

pub(crate) fn name_variants_from_native_forms(
    forms: &DraftFormsStepContentV3,
    initial_headwords: Option<&WordHeadwordsV2>,
) -> Result<Vec<EntryNameVariantV3>, V3ProjectionError> {
    let mut names = Vec::<EntryNameVariantV3>::new();
    let mut indices = HashMap::<String, usize>::new();
    let mut push =
        |variant_id, spelling: &str, dialect: Option<Dialect>| -> Result<(), V3ProjectionError> {
            let Some(normalized) = projectable_surface(variant_id, spelling)? else {
                return Ok(());
            };
            if let Some(&index) = indices.get(&normalized.key) {
                let dialects = &mut names[index].dialects;
                if let Some(dialect) = dialect
                    && !dialects.contains(&dialect)
                {
                    dialects.push(dialect);
                }
            } else {
                indices.insert(normalized.key, names.len());
                names.push(EntryNameVariantV3 {
                    text: normalized.display,
                    dialects: dialect.into_iter().collect(),
                });
            }
            Ok(())
        };
    let mut has_current_name = false;
    for pos in &forms.pos {
        for form in &pos.forms {
            if form.form_type != "base" {
                continue;
            }
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    has_current_name |= !common.spelling.trim().is_empty();
                    push(common.id, &common.spelling, Some(Dialect::Common))?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    let groups = pos
                        .form_groups
                        .iter()
                        .filter(|group| {
                            group.members.iter().any(|member| member.form_id == form.id)
                        })
                        .collect::<Vec<_>>();
                    let spelling_mode = forms
                        .content_rules(form.id)
                        .map(|rules| rules.spelling_mode)
                        .or_else(|| {
                            groups
                                .first()
                                .map(|group| group.dialect_rules.spelling_mode)
                        });
                    for (variant_id, spelling, dialect) in [
                        (uk.id, uk.spelling.as_str(), Dialect::Uk),
                        (us.id, us.spelling.as_str(), Dialect::Us),
                    ] {
                        has_current_name |= !spelling.trim().is_empty();
                        push(
                            variant_id,
                            spelling,
                            spelling_mode.map(|mode| match mode {
                                DialectModeV3::Distinguish => dialect,
                                DialectModeV3::Unified => Dialect::Common,
                            }),
                        )?;
                    }
                }
            }
        }
    }
    if !has_current_name && let Some(headwords) = initial_headwords {
        match headwords {
            WordHeadwordsV2::Unified { common } => {
                push(Uuid::nil(), common, Some(Dialect::Common))?
            }
            WordHeadwordsV2::Distinguish { uk, us, .. } => {
                push(Uuid::nil(), uk, Some(Dialect::Uk))?;
                push(Uuid::nil(), us, Some(Dialect::Us))?;
            }
        }
    }
    for name in &mut names {
        name.dialects.sort_by_key(|dialect| match dialect {
            Dialect::Common => 0,
            Dialect::Uk => 1,
            Dialect::Us => 2,
        });
    }
    names.sort_by_key(|name| {
        if name.dialects.contains(&Dialect::Common) {
            0
        } else if name.dialects.contains(&Dialect::Uk) {
            1
        } else {
            2
        }
    });
    Ok(names)
}

pub(crate) fn form_variant_sources(
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
) -> Result<Vec<V3FormVariantSurfaceSource>, V3ProjectionError> {
    let mut seen_form_ids = HashSet::new();
    let mut seen_variant_ids = HashSet::new();
    let mut sources = Vec::new();

    for pos in &forms.pos {
        let mut local_form_ids = HashSet::new();
        for form in &pos.forms {
            if !local_form_ids.insert(form.id) || !seen_form_ids.insert(form.id) {
                return Err(V3ProjectionError::DuplicateForm { form_id: form.id });
            }
        }

        let mut group_ids_by_form = HashMap::<Uuid, Vec<Uuid>>::new();
        for group in &pos.form_groups {
            for member in &group.members {
                if !local_form_ids.contains(&member.form_id) {
                    return Err(V3ProjectionError::UnknownGroupMember {
                        pos_id: pos.pos_id,
                        group_id: group.id,
                        form_id: member.form_id,
                    });
                }
                let group_ids = group_ids_by_form.entry(member.form_id).or_default();
                if !group_ids.contains(&group.id) {
                    group_ids.push(group.id);
                }
            }
        }

        for form in &pos.forms {
            let group_ids =
                group_ids_by_form
                    .get(&form.id)
                    .ok_or(V3ProjectionError::OrphanForm {
                        pos_id: pos.pos_id,
                        form_id: form.id,
                    })?;
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    ensure_unique_variant(&mut seen_variant_ids, common.id)?;
                    push_variant_sources(
                        &mut sources,
                        entry_id,
                        pos.pos_id,
                        &pos.pos,
                        group_ids,
                        form.id,
                        &form.form_type,
                        common.id,
                        V3SurfaceDialect::Common,
                        &common.spelling,
                        &[V3SurfaceDialectScope::Uk, V3SurfaceDialectScope::Us],
                    )?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    ensure_unique_variant(&mut seen_variant_ids, uk.id)?;
                    push_variant_sources(
                        &mut sources,
                        entry_id,
                        pos.pos_id,
                        &pos.pos,
                        group_ids,
                        form.id,
                        &form.form_type,
                        uk.id,
                        V3SurfaceDialect::Uk,
                        &uk.spelling,
                        &[V3SurfaceDialectScope::Uk],
                    )?;
                    ensure_unique_variant(&mut seen_variant_ids, us.id)?;
                    push_variant_sources(
                        &mut sources,
                        entry_id,
                        pos.pos_id,
                        &pos.pos,
                        group_ids,
                        form.id,
                        &form.form_type,
                        us.id,
                        V3SurfaceDialect::Us,
                        &us.spelling,
                        &[V3SurfaceDialectScope::Us],
                    )?;
                }
            }
        }
    }
    Ok(sources)
}

fn regional_variant_spellings(variants: &WordRegionalVariantsV3) -> Vec<(Uuid, &str)> {
    match variants {
        WordRegionalVariantsV3::Common { common } => vec![(common.id, &common.spelling)],
        WordRegionalVariantsV3::UkUs { uk, us } => {
            vec![(uk.id, &uk.spelling), (us.id, &us.spelling)]
        }
    }
}

fn ensure_unique_variant(
    seen_variant_ids: &mut HashSet<Uuid>,
    variant_id: Uuid,
) -> Result<(), V3ProjectionError> {
    if seen_variant_ids.insert(variant_id) {
        Ok(())
    } else {
        Err(V3ProjectionError::DuplicateVariant { variant_id })
    }
}

#[allow(clippy::too_many_arguments)]
fn push_variant_sources(
    sources: &mut Vec<V3FormVariantSurfaceSource>,
    entry_id: Uuid,
    pos_id: Uuid,
    pos: &str,
    group_ids: &[Uuid],
    form_id: Uuid,
    form_type: &str,
    variant_id: Uuid,
    dialect: V3SurfaceDialect,
    spelling: &str,
    dialect_scopes: &[V3SurfaceDialectScope],
) -> Result<(), V3ProjectionError> {
    let Some(normalized) = projectable_surface(variant_id, spelling)? else {
        return Ok(());
    };
    let source_id = format!("v3:form_variant:{variant_id}");
    for dialect_scope in dialect_scopes {
        sources.push(V3FormVariantSurfaceSource {
            entry_id,
            source_id: source_id.clone(),
            pos_id,
            pos: pos.to_owned(),
            group_ids: group_ids.to_vec(),
            form_id,
            variant_id,
            form_type: form_type.to_owned(),
            dialect,
            dialect_scope: *dialect_scope,
            surface: normalized.display.clone(),
            normalized_surface: normalized.key.clone(),
            normalization_version: HEADWORD_NORMALIZATION_VERSION,
            projection_version: V3_SURFACE_PROJECTION_VERSION,
        });
    }
    Ok(())
}

fn projectable_surface(
    variant_id: Uuid,
    spelling: &str,
) -> Result<Option<crate::lexicon::normalization::NormalizedHeadword>, V3ProjectionError> {
    if spelling.trim().is_empty() {
        return Ok(None);
    }
    normalize_headword(spelling)
        .map(Some)
        .map_err(|source| V3ProjectionError::InvalidSurface { variant_id, source })
}

fn short_uuid(entry_id: Uuid) -> String {
    entry_id
        .simple()
        .to_string()
        .chars()
        .take(SHORT_UUID_LENGTH)
        .collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::lexicon::dto::{
        CommonDialectV3, DialectRulesV3, DraftFormsStepContentV3, FormGroupScopeV3, TextOrigin,
        UkDialectV3, UsDialectV3, WordCommonFormVariantV3, WordConcreteFormV3,
        WordFormGroupMemberV3, WordFormGroupV3, WordPosFormsV3, WordRegionalVariantsV3,
        WordUkFormVariantV3, WordUsFormVariantV3,
    };

    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn common_form(form_id: Uuid, variant_id: Uuid, spelling: &str) -> WordConcreteFormV3 {
        WordConcreteFormV3 {
            id: form_id,
            form_type: "base".to_owned(),
            regional_variants: WordRegionalVariantsV3::Common {
                common: WordCommonFormVariantV3 {
                    spelling_rich: None,
                    spelling_speech: None,
                    is_regular: None,
                    id: variant_id,
                    dialect: CommonDialectV3::Common,
                    spelling: spelling.to_owned(),
                    origin: TextOrigin::Manual,
                    pronunciations: Vec::new(),
                    component_usages: Vec::new().into(),
                },
            },
        }
    }

    fn uk_us_form(
        form_id: Uuid,
        uk_variant_id: Uuid,
        uk: &str,
        us_variant_id: Uuid,
        us: &str,
    ) -> WordConcreteFormV3 {
        WordConcreteFormV3 {
            id: form_id,
            form_type: "base".to_owned(),
            regional_variants: WordRegionalVariantsV3::UkUs {
                uk: WordUkFormVariantV3 {
                    spelling_rich: None,
                    spelling_speech: None,
                    is_regular: None,
                    id: uk_variant_id,
                    dialect: UkDialectV3::Uk,
                    spelling: uk.to_owned(),
                    origin: TextOrigin::Manual,
                    pronunciations: Vec::new(),
                    component_usages: Vec::new().into(),
                },
                us: WordUsFormVariantV3 {
                    spelling_rich: None,
                    spelling_speech: None,
                    is_regular: None,
                    id: us_variant_id,
                    dialect: UsDialectV3::Us,
                    spelling: us.to_owned(),
                    origin: TextOrigin::Manual,
                    pronunciations: Vec::new(),
                    component_usages: Vec::new().into(),
                },
            },
        }
    }

    fn name_forms(
        forms: Vec<WordConcreteFormV3>,
        rules: DialectRulesV3,
    ) -> DraftFormsStepContentV3 {
        DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(900),
                pos: "noun".to_owned(),
                form_groups: vec![WordFormGroupV3 {
                    id: id(901),
                    is_regular: false,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: rules,
                    members: forms
                        .iter()
                        .enumerate()
                        .map(|(index, form)| WordFormGroupMemberV3 {
                            id: id(1000 + index as u128),
                            form_id: form.id,
                        })
                        .collect(),
                }],
                forms,
            }],
        }
    }

    #[test]
    fn list_names_merge_dialects_and_ignore_inflections() {
        let mut inflection = common_form(id(10), id(11), "colours");
        inflection.form_type = "plural".to_owned();
        let mut forms = name_forms(
            vec![
                uk_us_form(id(1), id(2), "colour", id(3), "color"),
                uk_us_form(id(4), id(5), "theatre", id(6), "theater"),
                uk_us_form(id(7), id(8), "same", id(9), "same"),
                inflection,
            ],
            DialectRulesV3::DISTINGUISH,
        );
        let mut duplicate = forms.pos[0].clone();
        duplicate.pos_id = id(999);
        forms.pos.push(duplicate);
        let names = name_variants_from_native_forms(&forms, None).unwrap();
        assert_eq!(
            names,
            vec![
                EntryNameVariantV3 {
                    text: "colour".into(),
                    dialects: vec![Dialect::Uk]
                },
                EntryNameVariantV3 {
                    text: "theatre".into(),
                    dialects: vec![Dialect::Uk]
                },
                EntryNameVariantV3 {
                    text: "same".into(),
                    dialects: vec![Dialect::Uk, Dialect::Us]
                },
                EntryNameVariantV3 {
                    text: "color".into(),
                    dialects: vec![Dialect::Us]
                },
                EntryNameVariantV3 {
                    text: "theater".into(),
                    dialects: vec![Dialect::Us]
                },
            ]
        );
    }

    #[test]
    fn list_names_use_spelling_rules_instead_of_pronunciation_sides() {
        let forms = name_forms(
            vec![uk_us_form(id(1), id(2), "cat", id(3), "cat")],
            DialectRulesV3::UNIFIED_DISTINGUISH,
        );
        assert_eq!(
            name_variants_from_native_forms(&forms, None).unwrap(),
            vec![EntryNameVariantV3 {
                text: "cat".into(),
                dialects: vec![Dialect::Common]
            },]
        );
        let mut mixed = forms.clone();
        mixed.pos.extend(
            name_forms(
                vec![uk_us_form(id(4), id(5), "cat", id(6), "cat")],
                DialectRulesV3::DISTINGUISH,
            )
            .pos,
        );
        assert_eq!(
            name_variants_from_native_forms(&mixed, None).unwrap()[0].dialects,
            [Dialect::Common, Dialect::Uk, Dialect::Us]
        );
    }

    #[test]
    fn list_names_use_shared_content_rules_and_keep_unbound_draft_names_unmarked() {
        let mut forms = name_forms(
            vec![uk_us_form(id(1), id(2), "cat", id(3), "cat")],
            DialectRulesV3::UNIFIED_DISTINGUISH,
        );
        forms.enable_shared().unwrap();
        forms.pos[0].form_groups[0].dialect_rules = DialectRulesV3::DISTINGUISH;
        assert_eq!(
            name_variants_from_native_forms(&forms, None).unwrap()[0].dialects,
            [Dialect::Common]
        );
        let mut unbound = name_forms(
            vec![uk_us_form(id(1), id(2), "cat", id(3), "cat")],
            DialectRulesV3::DISTINGUISH,
        );
        unbound.pos[0].form_groups.clear();
        assert_eq!(
            name_variants_from_native_forms(&unbound, None).unwrap(),
            vec![EntryNameVariantV3 {
                text: "cat".into(),
                dialects: vec![]
            },]
        );
    }

    #[test]
    fn list_names_only_fall_back_to_initial_names_when_all_base_spellings_are_blank() {
        let initial = WordHeadwordsV2::Distinguish {
            uk: "cosy".into(),
            us: "cozy".into(),
            source_dialect: crate::lexicon::dto::SourceDialect::Us,
        };
        let mut forms = name_forms(
            vec![uk_us_form(id(1), id(2), "", id(3), "")],
            DialectRulesV3::DISTINGUISH,
        );
        assert_eq!(
            name_variants_from_native_forms(&forms, Some(&initial))
                .unwrap()
                .iter()
                .map(|name| name.text.as_str())
                .collect::<Vec<_>>(),
            ["cosy", "cozy"]
        );
        let WordRegionalVariantsV3::UkUs { us, .. } = &mut forms.pos[0].forms[0].regional_variants
        else {
            unreachable!()
        };
        us.spelling = "current".into();
        assert_eq!(
            name_variants_from_native_forms(&forms, Some(&initial)).unwrap(),
            vec![EntryNameVariantV3 {
                text: "current".into(),
                dialects: vec![Dialect::Us]
            },]
        );
        assert!(
            name_variants_from_native_forms(&DraftFormsStepContentV3::default(), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn native_presentation_uses_only_base_forms_in_wire_order() {
        let first_form_id = id(1);
        let second_form_id = id(2);
        let comparative_form_id = id(3);
        let mut comparative = common_form(comparative_form_id, id(33), "more colourful");
        comparative.form_type = "comparative".to_owned();
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(10),
                pos: "adjective".to_owned(),
                forms: vec![
                    comparative,
                    common_form(first_form_id, id(11), "Color"),
                    uk_us_form(second_form_id, id(21), "colour", id(22), "color"),
                ],
                form_groups: vec![WordFormGroupV3 {
                    id: id(40),
                    is_regular: false,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::UNIFIED,
                    members: vec![
                        WordFormGroupMemberV3 {
                            id: id(41),
                            form_id: first_form_id,
                        },
                        WordFormGroupMemberV3 {
                            id: id(42),
                            form_id: second_form_id,
                        },
                        WordFormGroupMemberV3 {
                            id: id(43),
                            form_id: comparative_form_id,
                        },
                    ],
                }],
            }],
        };

        let presentation = presentation_from_native_forms(id(0xfeed), &forms).unwrap();

        assert_eq!(presentation.matched_surfaces, ["Color", "colour"]);
        assert_eq!(presentation.label, "Color / colour");
        assert_eq!(
            presentation.strategy_version,
            NATIVE_PRESENTATION_STRATEGY_VERSION
        );
    }

    #[test]
    fn native_presentation_uses_short_uuid_only_when_no_surface_exists() {
        let entry_id = Uuid::parse_str("12345678-9abc-def0-1234-56789abcdef0").unwrap();

        let presentation =
            presentation_from_native_forms(entry_id, &DraftFormsStepContentV3::default()).unwrap();

        assert_eq!(presentation.label, "未命名词条 · 12345678");
        assert!(presentation.matched_surfaces.is_empty());
        assert_eq!(
            presentation.strategy_version,
            EMPTY_PRESENTATION_STRATEGY_VERSION
        );
    }

    #[test]
    fn draft_blank_spellings_are_ignored_by_presentation_and_surface_projection() {
        let entry_id = Uuid::parse_str("12345678-9abc-def0-1234-56789abcdef0").unwrap();
        let blank_common_form_id = id(501);
        let regional_form_id = id(502);
        let blank_common_variant_id = id(503);
        let blank_uk_variant_id = id(504);
        let us_variant_id = id(505);
        let group_id = id(506);
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(500),
                pos: "noun".to_owned(),
                forms: vec![
                    common_form(blank_common_form_id, blank_common_variant_id, "   "),
                    uk_us_form(
                        regional_form_id,
                        blank_uk_variant_id,
                        "",
                        us_variant_id,
                        "center",
                    ),
                ],
                form_groups: vec![WordFormGroupV3 {
                    id: group_id,
                    is_regular: false,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::UNIFIED,
                    members: vec![
                        WordFormGroupMemberV3 {
                            id: id(507),
                            form_id: blank_common_form_id,
                        },
                        WordFormGroupMemberV3 {
                            id: id(508),
                            form_id: regional_form_id,
                        },
                    ],
                }],
            }],
        };

        let presentation = presentation_from_native_forms(entry_id, &forms).unwrap();
        assert_eq!(presentation.label, "center");
        assert_eq!(presentation.matched_surfaces, ["center"]);
        assert_eq!(
            presentation.strategy_version,
            NATIVE_PRESENTATION_STRATEGY_VERSION
        );

        let sources = form_variant_sources(entry_id, &forms).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].variant_id, us_variant_id);
        assert_eq!(sources[0].dialect, V3SurfaceDialect::Us);
        assert_eq!(sources[0].dialect_scope, V3SurfaceDialectScope::Us);
    }

    #[test]
    fn only_blank_draft_spellings_use_the_short_uuid_fallback_and_emit_no_sources() {
        let entry_id = Uuid::parse_str("87654321-9abc-def0-1234-56789abcdef0").unwrap();
        let form_id = id(601);
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(600),
                pos: "verb".to_owned(),
                forms: vec![common_form(form_id, id(602), "\t\n")],
                form_groups: vec![WordFormGroupV3 {
                    id: id(603),
                    is_regular: true,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::UNIFIED,
                    members: vec![WordFormGroupMemberV3 {
                        id: id(604),
                        form_id,
                    }],
                }],
            }],
        };

        let presentation = presentation_from_native_forms(entry_id, &forms).unwrap();
        assert_eq!(presentation.label, "未命名词条 · 87654321");
        assert!(presentation.matched_surfaces.is_empty());
        assert_eq!(
            presentation.strategy_version,
            EMPTY_PRESENTATION_STRATEGY_VERSION
        );
        assert!(form_variant_sources(entry_id, &forms).unwrap().is_empty());
    }

    #[test]
    fn nonblank_invalid_spellings_still_fail_closed() {
        let form_id = id(701);
        let variant_id = id(702);
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(700),
                pos: "noun".to_owned(),
                forms: vec![common_form(form_id, variant_id, "bad\0surface")],
                form_groups: vec![WordFormGroupV3 {
                    id: id(703),
                    is_regular: false,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::UNIFIED,
                    members: vec![WordFormGroupMemberV3 {
                        id: id(704),
                        form_id,
                    }],
                }],
            }],
        };
        let expected = V3ProjectionError::InvalidSurface {
            variant_id,
            source: HeadwordNormalizationError::ControlCharacter,
        };

        assert_eq!(
            presentation_from_native_forms(id(705), &forms),
            Err(expected)
        );
        assert_eq!(
            form_variant_sources(id(705), &forms),
            Err(V3ProjectionError::InvalidSurface {
                variant_id,
                source: HeadwordNormalizationError::ControlCharacter,
            })
        );
    }

    #[test]
    fn variant_sources_expand_common_and_preserve_all_group_memberships() {
        let entry_id = id(100);
        let pos_id = id(101);
        let form_id = id(102);
        let variant_id = id(103);
        let first_group_id = id(104);
        let second_group_id = id(105);
        let mut form = common_form(form_id, variant_id, "Workspaces");
        form.form_type = "custom_plural".to_owned();
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id,
                pos: "noun".to_owned(),
                forms: vec![form],
                form_groups: vec![
                    WordFormGroupV3 {
                        id: first_group_id,
                        is_regular: true,
                        scope: FormGroupScopeV3::General,
                        dialect_rules: DialectRulesV3::UNIFIED,
                        members: vec![WordFormGroupMemberV3 {
                            id: id(106),
                            form_id,
                        }],
                    },
                    WordFormGroupV3 {
                        id: second_group_id,
                        is_regular: false,
                        scope: FormGroupScopeV3::General,
                        dialect_rules: DialectRulesV3::UNIFIED,
                        members: vec![WordFormGroupMemberV3 {
                            id: id(107),
                            form_id,
                        }],
                    },
                ],
            }],
        };

        let sources = form_variant_sources(entry_id, &forms).unwrap();

        assert_eq!(
            sources.len(),
            2,
            "common must address both UK and US scopes"
        );
        assert_eq!(sources[0].dialect, V3SurfaceDialect::Common);
        assert_eq!(sources[0].dialect_scope, V3SurfaceDialectScope::Uk);
        assert_eq!(sources[1].dialect_scope, V3SurfaceDialectScope::Us);
        assert_eq!(sources[0].normalized_surface, "workspaces");
        assert_eq!(sources[0].form_id, form_id);
        assert!(
            sources
                .iter()
                .all(|source| source.form_type == "custom_plural")
        );
        assert_eq!(sources[0].variant_id, variant_id);
        assert_eq!(sources[0].group_ids, [first_group_id, second_group_id]);
        assert_eq!(sources[1].group_ids, [first_group_id, second_group_id]);
        assert_eq!(sources[0].source_id, sources[1].source_id);
        assert_eq!(
            sources[0].source_id,
            format!("v3:form_variant:{variant_id}")
        );
        assert_eq!(sources[0].projection_version, V3_SURFACE_PROJECTION_VERSION);
        assert_eq!(sources, form_variant_sources(entry_id, &forms).unwrap());
    }

    #[test]
    fn variant_sources_keep_uk_and_us_as_two_variants_of_one_form() {
        let form_id = id(202);
        let uk_variant_id = id(203);
        let us_variant_id = id(204);
        let group_id = id(205);
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(201),
                pos: "noun".to_owned(),
                forms: vec![uk_us_form(
                    form_id,
                    uk_variant_id,
                    "centre",
                    us_variant_id,
                    "center",
                )],
                form_groups: vec![WordFormGroupV3 {
                    id: group_id,
                    is_regular: true,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::DISTINGUISH,
                    members: vec![WordFormGroupMemberV3 {
                        id: id(206),
                        form_id,
                    }],
                }],
            }],
        };

        let sources = form_variant_sources(id(200), &forms).unwrap();

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].variant_id, uk_variant_id);
        assert_eq!(sources[0].dialect, V3SurfaceDialect::Uk);
        assert_eq!(sources[0].dialect_scope, V3SurfaceDialectScope::Uk);
        assert_eq!(sources[1].variant_id, us_variant_id);
        assert_eq!(sources[1].dialect, V3SurfaceDialect::Us);
        assert_eq!(sources[1].dialect_scope, V3SurfaceDialectScope::Us);
        assert!(sources.iter().all(|source| source.form_id == form_id));
        assert!(sources.iter().all(|source| source.group_ids == [group_id]));
    }

    #[test]
    fn variant_sources_fail_closed_for_orphan_and_cross_pos_memberships() {
        let orphan_form_id = id(301);
        let orphan_forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(300),
                pos: "noun".to_owned(),
                forms: vec![common_form(orphan_form_id, id(303), "orphan")],
                form_groups: Vec::new(),
            }],
        };
        assert_eq!(
            form_variant_sources(id(306), &orphan_forms),
            Err(V3ProjectionError::OrphanForm {
                pos_id: id(300),
                form_id: orphan_form_id,
            })
        );

        let cross_pos_form_id = id(302);
        let forms = DraftFormsStepContentV3 {
            shared: None,
            pos: vec![WordPosFormsV3 {
                pos_id: id(300),
                pos: "noun".to_owned(),
                forms: vec![common_form(orphan_form_id, id(303), "orphan")],
                form_groups: vec![WordFormGroupV3 {
                    id: id(304),
                    is_regular: false,
                    scope: FormGroupScopeV3::General,
                    dialect_rules: DialectRulesV3::UNIFIED,
                    members: vec![WordFormGroupMemberV3 {
                        id: id(305),
                        form_id: cross_pos_form_id,
                    }],
                }],
            }],
        };

        assert_eq!(
            form_variant_sources(id(306), &forms),
            Err(V3ProjectionError::UnknownGroupMember {
                pos_id: id(300),
                group_id: id(304),
                form_id: cross_pos_form_id,
            })
        );
    }
}
