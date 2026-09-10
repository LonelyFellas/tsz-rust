use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::dictionary_suggestions::{
    allowed_explicit_spelling_tag, apply_base_dialect_pair, apply_content_base_dialect_pairs,
    build_dictionary_suggestions, content_base_dialect_pairs, dictionary_alternative_terms,
    hard_disallowed_regional_relation_tag, regional_spelling_pair,
};
use super::*;
use crate::lexicon::audio_assets::dto::{AudioAsset, AudioAssetGender, AudioAssetLocale};
use crate::lexicon::dto::DraftMeaningsStepContent;
use crate::lexicon::dto::{
    AdminWordAny, AdminWordAnyEnvelope, AdminWordDraftAnyEnvelope, AdminWordDraftV3Envelope,
    AdminWordV3, AdminWordV3Capabilities, AdminWordV3Compatibility, BuiltinDictionaryEvidenceV3,
    CommonDialectV3, CreateAdminWordV3Input, DetectLexiconSurfaceResponseV3,
    DetectLexiconSurfaceV3Input, DetectionSurfaceRequestEchoV3, DialectRulesV3,
    DialectVariantRichTextSlotV3, DictionaryCoverageV3, DictionaryPronunciationEvidenceV3,
    DictionaryProvenanceV3, DictionaryProviderEvidenceV3, DraftFormsStepContentV3,
    DraftMeaningsStepContentV3, DraftNodeLocation, DraftValidationResponseV3, EnglishLanguageV3,
    EnglishTextV3, EntryPresentationV3, FormsImpactItemV3, FormsImpactNodeTypeV3,
    FormsImpactResponseV3, LegacyHeadwordsCompatibilityV3, PhraseComponentUsageV3,
    PresenceAwareVec, PreviewFormsImpactInputV3, PronunciationNormalizationVersionV3,
    PronunciationStyle, RetiredStableNodeV3, RichTextVariantV3, SaveFormsStepInputV3,
    SaveMeaningsStepInputV3, SuggestedConcreteFormV3, SuggestedRegionalVariantsV3, TextOrigin,
    UkDialectV3, UsDialectV3, V3PublicationBlockCode, V3PublicationCapability, V3RetiredNodeRole,
    V3ValidationIssueCode, ValidateAdminWordV3Input, WordCommonFormVariantV3, WordConcreteFormV3,
    WordDefinitionV3, WordEntryKindV3, WordFormGroupMemberV3, WordFormGroupV3, WordFormTypeV3,
    WordHeadwordsV2, WordPosFormsV3, WordPosMeaningsV3, WordPronunciationV3,
    WordRegionalVariantsV3, WordUkFormVariantV3, WordUsFormVariantV3,
};
use crate::lexicon::model::{ComponentTargetDraftRecord, NodeIdentityRecord, RegionEvidenceRecord};

const V3_CREATE_SCOPE: &str = "lexicon.entry.create.v3";
const V3_DETECTION_TTL: StdDuration = StdDuration::from_secs(5 * 60);
const V3_DETECTION_RETENTION_TTL: StdDuration = StdDuration::from_secs(65 * 60);

fn is_regional_spelling_relation(
    source: &RegionSurfaceRecord,
    candidate: &RegionSurfaceRecord,
    evidence: &RegionEvidenceRecord,
) -> bool {
    let evidence_target = if evidence.normalized_term == source.normalized_term {
        &candidate.normalized_term
    } else if evidence.normalized_term == candidate.normalized_term {
        &source.normalized_term
    } else {
        return false;
    };
    let targets_counterpart = evidence.targets.iter().any(|target| {
        normalize_headword(target).is_ok_and(|normalized| normalized.key == *evidence_target)
    });
    let same_part_of_speech =
        source.pos.contains(&evidence.pos) && candidate.pos.contains(&evidence.pos);
    let tags = evidence
        .raw_tags
        .iter()
        .map(|tag| tag.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let general_region = tags.iter().any(|tag| {
        matches!(
            tag.as_str(),
            "uk" | "british" | "received-pronunciation" | "us" | "american" | "general-american"
        )
    });
    let spelling_family =
        regional_spelling_pair(&source.normalized_term, &candidate.normalized_term).is_some();
    let explicit_spelling = evidence.evidence_type == "spelling"
        && general_region
        && tags.iter().all(|tag| allowed_explicit_spelling_tag(tag));
    let family_spelling = spelling_family
        && !tags
            .iter()
            .any(|tag| hard_disallowed_regional_relation_tag(tag));
    same_part_of_speech
        && targets_counterpart
        && (source.is_headword || candidate.is_headword)
        && (explicit_spelling || family_spelling)
}

#[derive(Debug, sqlx::FromRow)]
struct V3EntryStateRecord {
    origin: String,
    publication_canary_enabled: bool,
    initial_headwords: Option<Value>,
}

#[derive(Debug, sqlx::FromRow)]
struct V3PresentationRecord {
    label: String,
    matched_surfaces: Vec<String>,
    strategy_version: String,
}

#[derive(Debug, sqlx::FromRow)]
struct V3RelationTargetStatusRecord {
    id: Uuid,
    is_archived: bool,
    is_published: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct RetiredV3NodeRecord {
    id: Uuid,
    parent_node_id: Option<Uuid>,
    node_role: String,
    retired_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
struct V3AuditNodeDelta {
    generated_node_ids: Vec<Uuid>,
    changed_node_ids: Vec<Uuid>,
    retired_node_ids: Vec<Uuid>,
}

fn blank_v3_pronunciation() -> WordPronunciationV3 {
    WordPronunciationV3 {
        dict_phonetic_rich: None,
        actual_pron_rich: None,
        voice_profile: None,
        audio_assets: Vec::new(),
        id: Uuid::now_v7(),
        dict_phonetic: String::new(),
        actual_pron: String::new(),
        style: Some(PronunciationStyle::Normal),
    }
}

fn suggested_v3_pronunciations(
    values: &[DictionaryPronunciationEvidenceV3],
) -> Vec<WordPronunciationV3> {
    if values.is_empty() {
        return vec![blank_v3_pronunciation()];
    }
    values
        .iter()
        .map(|value| WordPronunciationV3 {
            dict_phonetic_rich: None,
            actual_pron_rich: None,
            voice_profile: None,
            audio_assets: Vec::new(),
            id: Uuid::now_v7(),
            dict_phonetic: value.dict_phonetic.clone(),
            actual_pron: value.actual_pron.clone().unwrap_or_default(),
            style: Some(value.style.unwrap_or(PronunciationStyle::Normal)),
        })
        .collect()
}

fn suggested_v3_dialect_rules(forms: &[&SuggestedConcreteFormV3]) -> DialectRulesV3 {
    let mut saw_uk_us = false;
    for form in forms {
        if let SuggestedRegionalVariantsV3::UkUs { uk, us } = &form.regional_variants {
            saw_uk_us = true;
            if uk.spelling != us.spelling {
                return DialectRulesV3::DISTINGUISH;
            }
        }
    }
    if saw_uk_us {
        DialectRulesV3::UNIFIED_DISTINGUISH
    } else {
        DialectRulesV3::UNIFIED
    }
}

fn materialize_suggested_v3_form(
    form: &SuggestedConcreteFormV3,
    rules: DialectRulesV3,
) -> WordConcreteFormV3 {
    let regional_variants = match (&form.regional_variants, rules) {
        (SuggestedRegionalVariantsV3::Common { common }, DialectRulesV3::UNIFIED) => {
            WordRegionalVariantsV3::Common {
                common: WordCommonFormVariantV3 {
                    id: Uuid::now_v7(),
                    dialect: CommonDialectV3::Common,
                    spelling: common.spelling.clone(),
                    origin: TextOrigin::Dictionary,
                    pronunciations: suggested_v3_pronunciations(&common.pronunciations),
                    component_usages: Vec::new().into(),
                },
            }
        }
        (SuggestedRegionalVariantsV3::Common { common }, _) => WordRegionalVariantsV3::UkUs {
            uk: WordUkFormVariantV3 {
                id: Uuid::now_v7(),
                dialect: UkDialectV3::Uk,
                spelling: common.spelling.clone(),
                origin: TextOrigin::Dictionary,
                pronunciations: suggested_v3_pronunciations(&common.pronunciations),
                component_usages: Vec::new().into(),
            },
            us: WordUsFormVariantV3 {
                id: Uuid::now_v7(),
                dialect: UsDialectV3::Us,
                spelling: common.spelling.clone(),
                origin: TextOrigin::Dictionary,
                pronunciations: suggested_v3_pronunciations(&common.pronunciations),
                component_usages: Vec::new().into(),
            },
        },
        (SuggestedRegionalVariantsV3::UkUs { uk, us }, _) => WordRegionalVariantsV3::UkUs {
            uk: WordUkFormVariantV3 {
                id: Uuid::now_v7(),
                dialect: UkDialectV3::Uk,
                spelling: uk.spelling.clone(),
                origin: TextOrigin::Dictionary,
                pronunciations: suggested_v3_pronunciations(&uk.pronunciations),
                component_usages: Vec::new().into(),
            },
            us: WordUsFormVariantV3 {
                id: Uuid::now_v7(),
                dialect: UsDialectV3::Us,
                spelling: us.spelling.clone(),
                origin: TextOrigin::Dictionary,
                pronunciations: suggested_v3_pronunciations(&us.pronunciations),
                component_usages: Vec::new().into(),
            },
        },
    };
    WordConcreteFormV3 {
        id: Uuid::now_v7(),
        form_type: form.form_type.clone(),
        regional_variants,
    }
}

fn blank_v3_base_form() -> WordConcreteFormV3 {
    WordConcreteFormV3 {
        id: Uuid::now_v7(),
        form_type: "base".to_owned(),
        regional_variants: WordRegionalVariantsV3::Common {
            common: WordCommonFormVariantV3 {
                id: Uuid::now_v7(),
                dialect: CommonDialectV3::Common,
                spelling: String::new(),
                origin: TextOrigin::Manual,
                pronunciations: vec![blank_v3_pronunciation()],
                component_usages: Vec::new().into(),
            },
        },
    }
}

fn materialize_v3_detection_forms(
    detection: &DetectLexiconSurfaceResponseV3,
) -> DraftFormsStepContentV3 {
    let builtin_forms = match &detection.builtin_dictionary {
        BuiltinDictionaryEvidenceV3::Matched {
            suggested_forms, ..
        } => suggested_forms.as_slice(),
        BuiltinDictionaryEvidenceV3::NotFound | BuiltinDictionaryEvidenceV3::Unavailable { .. } => {
            &[]
        }
    };
    let pos = detection
        .suggested_pos
        .iter()
        .map(|pos_code| {
            let suggestions = builtin_forms
                .iter()
                .filter(|form| &form.pos == pos_code)
                .collect::<Vec<_>>();
            let dialect_rules = suggested_v3_dialect_rules(&suggestions);
            let mut forms = suggestions
                .iter()
                .map(|form| materialize_suggested_v3_form(form, dialect_rules))
                .collect::<Vec<_>>();
            if !forms.iter().any(|form| form.form_type == "base") {
                forms.insert(0, blank_v3_base_form());
            }
            let members = forms
                .iter()
                .map(|form| WordFormGroupMemberV3 {
                    id: Uuid::now_v7(),
                    form_id: form.id,
                })
                .collect();
            WordPosFormsV3 {
                pos_id: Uuid::now_v7(),
                pos: pos_code.clone(),
                dialect_rules,
                forms,
                form_groups: vec![WordFormGroupV3 {
                    id: Uuid::now_v7(),
                    is_regular: true,
                    members,
                }],
            }
        })
        .collect();
    DraftFormsStepContentV3 { pos }
}

fn cloned_v3_pronunciations(values: &[WordPronunciationV3]) -> Vec<WordPronunciationV3> {
    values
        .iter()
        .map(|value| WordPronunciationV3 {
            dict_phonetic_rich: value.dict_phonetic_rich.clone(),
            actual_pron_rich: value.actual_pron_rich.clone(),
            voice_profile: value.voice_profile.clone(),
            audio_assets: value.audio_assets.clone(),
            id: Uuid::now_v7(),
            dict_phonetic: value.dict_phonetic.clone(),
            actual_pron: value.actual_pron.clone(),
            style: value.style,
        })
        .collect()
}

fn cloned_v3_component_usages(values: &[PhraseComponentUsageV3]) -> Vec<PhraseComponentUsageV3> {
    values
        .iter()
        .cloned()
        .map(|value| match value {
            PhraseComponentUsageV3::Unresolved { literal, .. } => {
                PhraseComponentUsageV3::Unresolved {
                    id: Uuid::now_v7(),
                    literal,
                }
            }
            PhraseComponentUsageV3::Resolved {
                literal,
                target_word_id,
                target_publication_id,
                target_pos_id,
                target_base_form_id,
                target_sense_id,
                target_form_id,
                target_variant_id,
                target_dialect,
                target_form_type,
                target_headword,
                target_gloss,
                ..
            } => PhraseComponentUsageV3::Resolved {
                id: Uuid::now_v7(),
                literal,
                target_word_id,
                target_publication_id,
                target_pos_id,
                target_base_form_id,
                target_sense_id,
                target_form_id,
                target_variant_id,
                target_dialect,
                target_form_type,
                target_headword,
                target_gloss,
            },
        })
        .collect()
}

fn apply_confirmed_v3_headwords(forms: &mut DraftFormsStepContentV3, headwords: &WordHeadwordsV2) {
    for pos in &mut forms.pos {
        let has_suggested_base = pos.forms.iter().any(|form| {
            form.form_type == "base"
                && match &form.regional_variants {
                    WordRegionalVariantsV3::Common { common } => !common.spelling.is_empty(),
                    WordRegionalVariantsV3::UkUs { uk, us } => {
                        !uk.spelling.is_empty() || !us.spelling.is_empty()
                    }
                }
        });
        if !has_suggested_base {
            continue;
        }
        match headwords {
            WordHeadwordsV2::Distinguish { .. } => {
                pos.dialect_rules = DialectRulesV3::DISTINGUISH;
                for form in &mut pos.forms {
                    if let WordRegionalVariantsV3::Common { common } = &form.regional_variants {
                        form.regional_variants = WordRegionalVariantsV3::UkUs {
                            uk: WordUkFormVariantV3 {
                                id: common.id,
                                dialect: UkDialectV3::Uk,
                                spelling: common.spelling.clone(),
                                origin: common.origin,
                                pronunciations: common.pronunciations.clone(),
                                component_usages: common.component_usages.clone(),
                            },
                            us: WordUsFormVariantV3 {
                                id: Uuid::now_v7(),
                                dialect: UsDialectV3::Us,
                                spelling: common.spelling.clone(),
                                origin: common.origin,
                                pronunciations: cloned_v3_pronunciations(&common.pronunciations),
                                component_usages: cloned_v3_component_usages(
                                    &common.component_usages,
                                )
                                .into(),
                            },
                        };
                    }
                }
            }
            WordHeadwordsV2::Unified { .. } => {
                pos.dialect_rules = if pos.dialect_rules.phonetic_mode
                    == crate::lexicon::dto::DialectModeV3::Distinguish
                {
                    DialectRulesV3::UNIFIED_DISTINGUISH
                } else {
                    DialectRulesV3::UNIFIED
                };
                for form in &mut pos.forms {
                    match (&mut form.regional_variants, pos.dialect_rules) {
                        (
                            WordRegionalVariantsV3::UkUs { uk, us },
                            DialectRulesV3::UNIFIED_DISTINGUISH,
                        ) => us.spelling.clone_from(&uk.spelling),
                        (WordRegionalVariantsV3::UkUs { uk, .. }, DialectRulesV3::UNIFIED) => {
                            form.regional_variants = WordRegionalVariantsV3::Common {
                                common: WordCommonFormVariantV3 {
                                    id: uk.id,
                                    dialect: CommonDialectV3::Common,
                                    spelling: uk.spelling.clone(),
                                    origin: uk.origin,
                                    pronunciations: uk.pronunciations.clone(),
                                    component_usages: uk.component_usages.clone(),
                                },
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
        for form in &mut pos.forms {
            if form.form_type != "base" {
                continue;
            }
            match (headwords, &mut form.regional_variants) {
                (
                    WordHeadwordsV2::Unified { common },
                    WordRegionalVariantsV3::Common { common: variant },
                ) => {
                    if variant.spelling != *common {
                        variant.origin = TextOrigin::Manual;
                    }
                    variant.spelling.clone_from(common);
                }
                (WordHeadwordsV2::Unified { common }, WordRegionalVariantsV3::UkUs { uk, us }) => {
                    if uk.spelling != *common {
                        uk.origin = TextOrigin::Manual;
                    }
                    if us.spelling != *common {
                        us.origin = TextOrigin::Manual;
                    }
                    uk.spelling.clone_from(common);
                    us.spelling.clone_from(common);
                }
                (
                    WordHeadwordsV2::Distinguish {
                        uk: confirmed_uk,
                        us: confirmed_us,
                        ..
                    },
                    WordRegionalVariantsV3::UkUs { uk, us },
                ) => {
                    if uk.spelling != *confirmed_uk {
                        uk.origin = TextOrigin::Manual;
                    }
                    if us.spelling != *confirmed_us {
                        us.origin = TextOrigin::Manual;
                    }
                    uk.spelling.clone_from(confirmed_uk);
                    us.spelling.clone_from(confirmed_us);
                }
                (WordHeadwordsV2::Distinguish { .. }, WordRegionalVariantsV3::Common { .. }) => {
                    unreachable!("distinguish headwords convert every V3 form to uk_us")
                }
            }
        }
    }
}

fn compatibility_v3_headwords(
    detection: &DetectLexiconSurfaceResponseV3,
    forms: &DraftFormsStepContentV3,
) -> Result<WordHeadwordsV2, LexiconServiceError> {
    let mut candidates = std::collections::BTreeSet::new();
    for form in forms
        .pos
        .iter()
        .flat_map(|pos| &pos.forms)
        .filter(|form| form.form_type == "base")
    {
        match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } if !common.spelling.trim().is_empty() => {
                candidates.insert(("common", common.spelling.clone(), String::new()));
            }
            WordRegionalVariantsV3::UkUs { uk, us }
                if !uk.spelling.trim().is_empty() && !us.spelling.trim().is_empty() =>
            {
                candidates.insert(("uk_us", uk.spelling.clone(), us.spelling.clone()));
            }
            _ => {}
        }
    }
    if candidates.len() == 1 {
        let (mode, first, second) = candidates.into_iter().next().expect("one candidate");
        if mode == "common" {
            return Ok(WordHeadwordsV2::Unified { common: first });
        }
        let normalized_uk = normalize_headword(&first).map_err(map_headword_error)?.key;
        let normalized_us = normalize_headword(&second).map_err(map_headword_error)?.key;
        let source_dialect = if detection.normalized_surface == normalized_us
            && detection.normalized_surface != normalized_uk
        {
            SourceDialect::Us
        } else {
            SourceDialect::Uk
        };
        return Ok(WordHeadwordsV2::Distinguish {
            uk: first,
            us: second,
            source_dialect,
        });
    }
    Ok(WordHeadwordsV2::Unified {
        common: NormalizedHeadword::parse(&detection.request.surface)
            .map_err(map_surface_error)?
            .display,
    })
}

fn confirmed_v3_headwords_presentation(headwords: &WordHeadwordsV2) -> EntryPresentationV3 {
    let matched_surfaces = ordered_headword_sides(headwords)
        .into_iter()
        .map(|(_, spelling)| spelling.to_owned())
        .collect::<Vec<_>>();
    EntryPresentationV3 {
        label: matched_surfaces.join(" / "),
        matched_surfaces,
        strategy_version: crate::lexicon::v3_projection::NATIVE_PRESENTATION_STRATEGY_VERSION
            .to_owned(),
    }
}

fn detection_basis_dialect_for_headwords(
    normalized_surface: &str,
    headwords: &WordHeadwordsV2,
) -> Result<Option<SourceDialect>, LexiconServiceError> {
    let WordHeadwordsV2::Distinguish { uk, us, .. } = headwords else {
        return Ok(None);
    };
    let matches_uk = normalize_headword(uk).map_err(map_headword_error)?.key == normalized_surface;
    let matches_us = normalize_headword(us).map_err(map_headword_error)?.key == normalized_surface;
    Ok(match (matches_uk, matches_us) {
        (true, false) => Some(SourceDialect::Uk),
        (false, true) => Some(SourceDialect::Us),
        _ => None,
    })
}

/// 拿归一化后的检测 surface 与当时的主词逐侧比对。
///
/// 外层 `None` 表示材料不全、算不出来，和内层「算过了但没有唯一命中侧」是两回事——
/// 前者才允许退回近似值，后者是明确结论，不该被近似值盖掉。
fn surface_detection_basis(
    normalized_surface: Option<&Value>,
    headwords: Option<&Value>,
) -> Option<Option<SourceDialect>> {
    let normalized_surface = normalized_surface?.as_str()?;
    let headwords: WordHeadwordsV2 = serde_json::from_value(headwords?.clone()).ok()?;
    detection_basis_dialect_for_headwords(normalized_surface, &headwords).ok()
}

/// 按词条来源分派 detection_snapshot 的两种形状，而不是嗅探 JSON 键：`migrated_v2` 存的是
/// `WordDetectionSnapshotV2`（`normalized_headword` + `headwords`，另有词典命中方言
/// `matched_dialect`），原生 v3 存的是 `DetectLexiconSurfaceResponseV3`（`normalized_surface`，
/// 主词另存在 `v3_entry_state.initial_headwords`）。
///
/// 两条路径都优先做逐侧比对，口径与建条时一致；只有 v2 快照缺了比对材料，才退回
/// `matched_dialect` 这个近似值——它是词典主条的方言，与「原始 surface 命中哪一侧」并不等价。
///
/// 这个字段纯属展示，取不到就不显示——任何一步都不得让读词条本身失败。
fn stored_detection_basis_dialect(
    origin: &str,
    detection_snapshot: &Value,
    initial_headwords: Option<&Value>,
) -> Option<SourceDialect> {
    if origin == "migrated_v2" {
        if let Some(basis) = surface_detection_basis(
            detection_snapshot.get("normalized_headword"),
            detection_snapshot.get("headwords"),
        ) {
            return basis;
        }
        return match detection_snapshot
            .get("matched_dialect")
            .and_then(Value::as_str)
        {
            Some("uk") => Some(SourceDialect::Uk),
            Some("us") => Some(SourceDialect::Us),
            _ => None,
        };
    }
    surface_detection_basis(
        detection_snapshot.get("normalized_surface"),
        initial_headwords,
    )
    .flatten()
}

fn initial_v3_headword_keys(
    headwords: &WordHeadwordsV2,
) -> Result<Vec<String>, LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            let key = normalize_headword(common).map_err(map_headword_error)?.key;
            Ok(vec![format!("uk:{key}"), format!("us:{key}")])
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => Ok(vec![
            format!(
                "uk:{}",
                normalize_headword(uk).map_err(map_headword_error)?.key
            ),
            format!(
                "us:{}",
                normalize_headword(us).map_err(map_headword_error)?.key
            ),
        ]),
    }
}

fn preserve_missing_component_usages(
    proposed: &mut DraftFormsStepContentV3,
    current: &DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let mut current_by_variant = HashMap::<Uuid, (Dialect, Vec<PhraseComponentUsageV3>)>::new();
    for form in current.pos.iter().flat_map(|pos| &pos.forms) {
        match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                current_by_variant.insert(
                    common.id,
                    (Dialect::Common, common.component_usages.to_vec()),
                );
            }
            WordRegionalVariantsV3::UkUs { uk, us } => {
                current_by_variant.insert(uk.id, (Dialect::Uk, uk.component_usages.to_vec()));
                current_by_variant.insert(us.id, (Dialect::Us, us.component_usages.to_vec()));
            }
        }
    }
    let mut proposed_by_variant = HashMap::<Uuid, (Dialect, bool)>::new();
    for form in proposed.pos.iter_mut().flat_map(|pos| &mut pos.forms) {
        match &mut form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                let was_present = common.component_usages.was_present();
                proposed_by_variant.insert(common.id, (Dialect::Common, was_present));
                common.component_usages.preserve_missing_from(
                    current_by_variant
                        .get(&common.id)
                        .map_or(&[], |(_, values)| values.as_slice()),
                );
            }
            WordRegionalVariantsV3::UkUs { uk, us } => {
                let uk_was_present = uk.component_usages.was_present();
                let us_was_present = us.component_usages.was_present();
                proposed_by_variant.insert(uk.id, (Dialect::Uk, uk_was_present));
                proposed_by_variant.insert(us.id, (Dialect::Us, us_was_present));
                uk.component_usages.preserve_missing_from(
                    current_by_variant
                        .get(&uk.id)
                        .map_or(&[], |(_, values)| values.as_slice()),
                );
                us.component_usages.preserve_missing_from(
                    current_by_variant
                        .get(&us.id)
                        .map_or(&[], |(_, values)| values.as_slice()),
                );
            }
        }
    }
    for (variant_id, (current_dialect, components)) in current_by_variant {
        if components.is_empty() {
            continue;
        }
        match proposed_by_variant.get(&variant_id) {
            Some((proposed_dialect, _)) if proposed_dialect == &current_dialect => {}
            Some((_, true)) => {}
            Some((_, false)) | None => return Err(LexiconServiceError::ReferenceConflict),
        }
    }
    Ok(())
}

impl LexiconService {
    pub async fn get_draft_any(
        &self,
        id: Uuid,
    ) -> Result<AdminWordDraftAnyEnvelope, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        if record.content_schema_version == 2 {
            let mut word = entry_from_record(record)?;
            self.hydrate_sentence_associations(&mut word).await?;
            let retired_stable_slots = self
                .repository
                .retired_stable_slots(id)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| RetiredStableSlotV2 {
                    id: record.id,
                    parent_node_id: record.parent_node_id,
                    node_role: record.node_role,
                })
                .collect();
            return Ok(AdminWordDraftAnyEnvelope::V2(Box::new(
                AdminWordDraftV2Envelope {
                    word,
                    retired_stable_slots,
                },
            )));
        }
        if record.content_schema_version != 3 {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(
                record.content_schema_version,
            ));
        }
        let word = self.entry_v3_from_record(record).await?;
        let retired_stable_nodes = self.retired_v3_nodes(id).await?;
        Ok(AdminWordDraftAnyEnvelope::V3(Box::new(
            AdminWordDraftV3Envelope {
                word,
                retired_stable_nodes,
            },
        )))
    }

    pub async fn get_v3(&self, id: Uuid) -> Result<AdminWordV3, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        self.entry_v3_from_record(record).await
    }

    async fn hydrate_v3_relation_target_statuses(
        &self,
        meanings: &mut DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconServiceError> {
        let mut target_ids = meanings
            .pos
            .iter()
            .flat_map(|pos| &pos.senses)
            .flat_map(|sense| &sense.relations)
            .filter_map(|relation| relation.target_word_id)
            .collect::<Vec<_>>();
        target_ids.sort_unstable();
        target_ids.dedup();
        if target_ids.is_empty() {
            return Ok(());
        }
        let statuses = sqlx::query_as::<_, V3RelationTargetStatusRecord>(
            r#"
            SELECT entry.id,
                   entry.archived_at IS NOT NULL AS is_archived,
                   entry.current_publication_id IS NOT NULL AS is_published
            FROM lexicon.entries entry
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = entry.id
             AND presentation.content_schema_version = 3
             AND presentation.source_revision = entry.revision
            WHERE entry.id = ANY($1)
            "#,
        )
        .bind(&target_ids)
        .fetch_all(self.repository.pool())
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|status| (status.id, status))
        .collect::<HashMap<_, _>>();
        for relation in meanings
            .pos
            .iter_mut()
            .flat_map(|pos| &mut pos.senses)
            .flat_map(|sense| &mut sense.relations)
        {
            let Some(target_id) = relation.target_word_id else {
                relation.target_status = None;
                continue;
            };
            let status = statuses.get(&target_id).ok_or_else(invariant_record)?;
            relation.target_status = Some(if status.is_archived {
                AdminWordStatus::Archived
            } else if status.is_published {
                AdminWordStatus::Published
            } else {
                AdminWordStatus::Draft
            });
        }
        Ok(())
    }

    async fn entry_v3_from_record(
        &self,
        record: EntryRecord,
    ) -> Result<AdminWordV3, LexiconServiceError> {
        if record.content_schema_version != 3 {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(
                record.content_schema_version,
            ));
        }
        if record.language != "en" {
            return Err(invariant_record());
        }
        let kind = parse_v3_kind(&record.kind).ok_or_else(invariant_record)?;
        let forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let mut meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(record.meanings.clone()).map_err(serialization_error)?;
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut meanings);
        self.hydrate_v3_sentence_associations(record.id, &mut meanings)
            .await?;
        self.hydrate_v3_relation_target_statuses(&mut meanings)
            .await?;
        let state = sqlx::query_as::<_, V3EntryStateRecord>(
            r#"
            SELECT origin, publication_canary_enabled, initial_headwords
            FROM lexicon.v3_entry_state
            WHERE entry_id = $1
            "#,
        )
        .bind(record.id)
        .fetch_optional(self.repository.pool())
        .await
        .map_err(database_error)?
        .ok_or_else(invariant_record)?;
        let detection_basis_dialect = stored_detection_basis_dialect(
            &state.origin,
            &record.detection_snapshot,
            state.initial_headwords.as_ref(),
        );
        let compatibility = if state.origin == "migrated_v2" {
            Some(AdminWordV3Compatibility {
                legacy_headwords: legacy_headwords_from_record(&record)?,
            })
        } else {
            None
        };
        let presentation = self
            .v3_presentation(record.id, record.revision, &forms, compatibility.as_ref())
            .await?;
        let completed_steps = record
            .completed_steps
            .iter()
            .filter_map(|step| match step.as_str() {
                "basics" => Some(PersistedWordStep::Basics),
                "forms" => Some(PersistedWordStep::Forms),
                "meanings" => Some(PersistedWordStep::Meanings),
                _ => None,
            })
            .collect();
        Ok(AdminWordV3 {
            schema_version: 3,
            id: record.id,
            language: EnglishLanguageV3::En,
            kind,
            status: if record.archived_at.is_some() {
                AdminWordStatus::Archived
            } else if record.current_publication_id.is_some() {
                AdminWordStatus::Published
            } else {
                AdminWordStatus::Draft
            },
            revision: record.revision,
            lifecycle_revision: record.lifecycle_revision,
            annotation: record.annotation,
            annotation_revision: record.annotation_revision,
            published_revision: record.current_publication_source_revision,
            has_unpublished_changes: record
                .current_publication_source_revision
                .is_some_and(|revision| revision != record.revision),
            presentation,
            capabilities: AdminWordV3Capabilities {
                text_links: Some(true),
                publication: match state.origin.as_str() {
                    "native" => V3PublicationCapability::Native,
                    "migrated_v2" => V3PublicationCapability::MigrationCanary {
                        whitelisted: state.publication_canary_enabled,
                        blocked_code: (!state.publication_canary_enabled)
                            .then_some(V3PublicationBlockCode::MigrationCanaryNotWhitelisted),
                    },
                    _ => return Err(invariant_record()),
                },
                pronunciation_normalization_version:
                    PronunciationNormalizationVersionV3::NfkcTrimLowerV1,
                sentence_associations: None,
                sentence_target_discovery: None,
                draft_relation_prebinding: None,
                sense_component_usages: None,
            },
            detection_basis_dialect,
            forms,
            meanings,
            compatibility,
            completed_steps,
            max_reachable_step: max_reachable_step(&record.completed_steps),
            created_by: record.created_by_admin_id,
            created_at: record.created_at,
            updated_at: record.updated_at,
            archived_at: record.archived_at,
            archived_by: record.archived_by_admin_id,
            published_at: record.current_published_at,
        })
    }

    async fn v3_presentation(
        &self,
        entry_id: Uuid,
        source_revision: i64,
        forms: &DraftFormsStepContentV3,
        compatibility: Option<&AdminWordV3Compatibility>,
    ) -> Result<EntryPresentationV3, LexiconServiceError> {
        let projected = sqlx::query_as::<_, V3PresentationRecord>(
            r#"
            SELECT label, matched_surfaces, strategy_version
            FROM lexicon.entry_presentation_projection
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND source_revision = $2
            "#,
        )
        .bind(entry_id)
        .bind(source_revision)
        .fetch_optional(self.repository.pool())
        .await
        .map_err(database_error)?;
        if let Some(projected) = projected {
            return Ok(EntryPresentationV3 {
                label: projected.label,
                matched_surfaces: projected.matched_surfaces,
                strategy_version: projected.strategy_version,
            });
        }
        if let Some(compatibility) = compatibility {
            return Ok(
                crate::lexicon::v3_projection::presentation_from_legacy_bridge(
                    entry_id,
                    &compatibility.legacy_headwords,
                ),
            );
        }
        crate::lexicon::v3_projection::presentation_from_native_forms(entry_id, forms).map_err(
            |_| {
                LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                    "validated V3 forms could not be presented",
                ))
            },
        )
    }

    async fn retired_v3_nodes(
        &self,
        entry_id: Uuid,
    ) -> Result<Vec<RetiredStableNodeV3>, LexiconServiceError> {
        let records = sqlx::query_as::<_, RetiredV3NodeRecord>(
            r#"
            SELECT id, parent_node_id, node_role,
                   removed_from_draft_at AS retired_at
            FROM lexicon.nodes
            WHERE entry_id = $1
              AND removed_from_draft_at IS NOT NULL
              AND node_role = ANY($2)
            ORDER BY removed_from_draft_at, id
            "#,
        )
        .bind(entry_id)
        .bind([
            "forms.pos",
            "forms.form_group",
            "forms.group_membership",
            "forms.concrete_form",
            "forms.form_variant:common",
            "forms.form_variant:uk",
            "forms.form_variant:us",
            "forms.pronunciation",
        ])
        .fetch_all(self.repository.pool())
        .await
        .map_err(database_error)?;
        records
            .into_iter()
            .map(|record| {
                Ok(RetiredStableNodeV3 {
                    id: record.id,
                    node_role: retired_role(&record.node_role)?,
                    parent_node_id: record.parent_node_id,
                    retired_at: record.retired_at,
                })
            })
            .collect()
    }

    async fn v3_regional_spelling_surface(
        &self,
        mut source: Option<RegionSurfaceRecord>,
    ) -> Result<Option<RegionSurfaceRecord>, LexiconServiceError> {
        let Some(source_value) = source.as_mut() else {
            return Ok(None);
        };
        let direct_keys = source_value
            .targets
            .iter()
            .filter_map(|target| normalize_headword(target).ok().map(|value| value.key))
            .collect::<Vec<_>>();
        let mut candidates = self
            .repository
            .region_surfaces(&direct_keys)
            .await
            .map_err(repository_error)?;
        candidates.extend(
            self.repository
                .region_surfaces_targeting(&source_value.normalized_term)
                .await
                .map_err(repository_error)?,
        );
        candidates.sort_by(|left, right| {
            direct_keys
                .iter()
                .position(|key| key == &left.normalized_term)
                .unwrap_or(usize::MAX)
                .cmp(
                    &direct_keys
                        .iter()
                        .position(|key| key == &right.normalized_term)
                        .unwrap_or(usize::MAX),
                )
                .then_with(|| left.normalized_term.cmp(&right.normalized_term))
        });
        candidates.dedup_by(|left, right| left.normalized_term == right.normalized_term);
        let mut evidence_keys = candidates
            .iter()
            .map(|candidate| candidate.normalized_term.clone())
            .collect::<Vec<_>>();
        evidence_keys.push(source_value.normalized_term.clone());
        evidence_keys.sort();
        evidence_keys.dedup();
        let evidence = self
            .repository
            .region_evidence(&evidence_keys)
            .await
            .map_err(repository_error)?;
        let targets = candidates
            .into_iter()
            .filter(|candidate| {
                evidence
                    .iter()
                    .any(|item| is_regional_spelling_relation(source_value, candidate, item))
            })
            .map(|candidate| candidate.term)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Ok(None);
        }
        source_value.targets = targets;
        Ok(source)
    }

    pub async fn detect_v3(
        &self,
        actor_id: Uuid,
        input: DetectLexiconSurfaceV3Input,
    ) -> Result<DetectLexiconSurfaceResponseV3, LexiconServiceError> {
        let normalized = NormalizedHeadword::parse(&input.surface).map_err(map_surface_error)?;
        let active_term = self
            .repository
            .dictionary_term(&normalized.key)
            .await
            .map_err(repository_error)?;
        let direct_surface = self
            .repository
            .region_surface(&normalized.key)
            .await
            .map_err(repository_error)?;
        let source_surface = direct_surface.or_else(|| {
            active_term.as_ref().map(|term| RegionSurfaceRecord {
                normalized_term: normalized.key.clone(),
                term: term.term.clone(),
                region_family: term.region_family.clone(),
                pos: term.pos.clone(),
                targets: Vec::new(),
                is_headword: true,
            })
        });
        let surface = self.v3_regional_spelling_surface(source_surface).await?;
        let term = match active_term {
            Some(term) => Some(term),
            None if surface.is_some() => self
                .repository
                .dictionary_term_from_region_surface(&normalized.key)
                .await
                .map_err(repository_error)?,
            None => None,
        };
        let (builtin_dictionary, suggested_pos) = if let Some(term) = term {
            // 内置词典的词性映射可能落在目录里已不存在的编码（如 2026-09-06 下线的介词等六个
            // 非基础种子），与 V2 路径一样只保留 catalog 现存的基本词性，避免建议出无法保存的 pos；
            // 但保持词典给出的词性顺序（首项是词典主词性），不按目录排序重排。
            let mapped_pos = map_dictionary_pos(&term.pos);
            let existing_codes = self
                .repository
                .catalog_parts(&mapped_pos)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|part| part.code)
                .collect::<std::collections::HashSet<_>>();
            let builtin_suggested_pos = mapped_pos
                .into_iter()
                .filter(|code| existing_codes.contains(code))
                .collect::<Vec<_>>();
            let mut base_content_keys = vec![normalized.key.clone()];
            if let Some(surface) = &surface {
                base_content_keys.extend(
                    surface.targets.iter().filter_map(|target| {
                        normalize_headword(target).ok().map(|value| value.key)
                    }),
                );
            }
            base_content_keys.sort();
            base_content_keys.dedup();
            let base_content = self
                .repository
                .dictionary_contents_for_terms(&base_content_keys)
                .await
                .map_err(repository_error)?;
            let (headwords, _) = self
                .detected_headwords_v3(&term.term, &term.region_family, surface)
                .await?;
            let regional_pair = match headwords {
                WordHeadwordsV2::Distinguish { uk, us, .. } => Some((uk, us)),
                WordHeadwordsV2::Unified { .. } => None,
            };
            let mut content_pairs = Vec::new();
            let content = if regional_pair.is_some() {
                base_content
            } else {
                let mut discovery_keys = base_content_keys.clone();
                discovery_keys.extend(dictionary_alternative_terms(&normalized.key, &base_content));
                discovery_keys.sort();
                discovery_keys.dedup();
                let discovery_content = self
                    .repository
                    .dictionary_contents_for_terms(&discovery_keys)
                    .await
                    .map_err(repository_error)?;
                content_pairs = content_base_dialect_pairs(&normalized.key, &discovery_content);
                // 方言配对同样只保留目录现存的词性，否则孤儿 pair 会把仍在目录里的词性的派生词形剪掉。
                content_pairs.retain(|pair| existing_codes.contains(&pair.pos));
                discovery_content
                    .into_iter()
                    .filter(|record| {
                        base_content_keys.contains(&record.normalized_term)
                            || content_pairs.iter().any(|pair| {
                                (record.normalized_term == pair.uk
                                    || record.normalized_term == pair.us)
                                    && map_dictionary_pos(std::slice::from_ref(&record.pos))
                                        .contains(&pair.pos)
                            })
                    })
                    .collect()
            };
            let mut suggestions =
                build_dictionary_suggestions(&term.term, &builtin_suggested_pos, &content);
            let configured_types =
                sqlx::query_scalar::<_, String>("SELECT code FROM catalog.form_types")
                    .fetch_all(self.repository.pool())
                    .await
                    .map_err(database_error)?;
            suggestions
                .forms
                .retain(|f| configured_types.contains(&f.form_type));
            let has_dialect_pair = regional_pair.is_some() || !content_pairs.is_empty();
            if let Some((uk, us)) = regional_pair {
                apply_base_dialect_pair(&mut suggestions.forms, &uk, &us);
            } else if !content_pairs.is_empty() {
                apply_content_base_dialect_pairs(&mut suggestions.forms, &content_pairs);
            }
            if has_dialect_pair {
                suggestions.retain_base_only(!content_pairs.is_empty());
            }
            let provider = DictionaryProviderEvidenceV3 {
                name: term.provider_name,
                version: term.provider_version,
            };
            let content_provider = content.first().map(|record| DictionaryProviderEvidenceV3 {
                name: record.provider_name.clone(),
                version: record.provider_version.clone(),
            });
            let pronunciation_coverage = if suggestions.has_pronunciations {
                DictionaryCoverageStateV2::Partial
            } else {
                DictionaryCoverageStateV2::Missing
            };
            (
                BuiltinDictionaryEvidenceV3::Matched {
                    provider: provider.clone(),
                    suggested_pos: builtin_suggested_pos.clone(),
                    suggested_forms: suggestions.forms,
                    coverage: DictionaryCoverageV3 {
                        forms: DictionaryCoverageStateV2::Partial,
                        pronunciations: pronunciation_coverage,
                        meanings: DictionaryCoverageStateV2::Missing,
                        examples: DictionaryCoverageStateV2::Missing,
                        frequency: DictionaryCoverageStateV2::Missing,
                    },
                    provenance: DictionaryProvenanceV3 {
                        forms: if suggestions.has_form_evidence {
                            content_provider.clone()
                        } else {
                            Some(provider.clone())
                        },
                        pronunciations: suggestions
                            .has_pronunciations
                            .then_some(content_provider)
                            .flatten(),
                        meanings: None,
                        examples: None,
                        frequency: None,
                    },
                },
                builtin_suggested_pos,
            )
        } else {
            (BuiltinDictionaryEvidenceV3::NotFound, Vec::new())
        };
        let detection_id = Uuid::now_v7();
        let now = Utc::now();
        let mut detection = DetectLexiconSurfaceResponseV3 {
            existing_draft_id: None,
            schema_version: 3,
            detection_id,
            expires_at: now + Duration::from_std(V3_DETECTION_TTL).expect("five minutes is valid"),
            request: DetectionSurfaceRequestEchoV3 {
                language: input.language,
                kind: input.kind,
                surface: normalized.display,
            },
            normalized_surface: normalized.key,
            requires_acknowledgement: false,
            matches: Vec::new(),
            surface_match_page: None,
            builtin_dictionary,
            suggested_pos,
        };
        // Confirm the complete default creation surface, including dictionary variants.
        let mut forms = materialize_v3_detection_forms(&detection);
        let headwords = compatibility_v3_headwords(&detection, &forms)?;
        apply_confirmed_v3_headwords(&mut forms, &headwords);
        let keys = initial_v3_headword_keys(&headwords)?;
        let (matches, surface_match_page, existing_suggested_pos) = self
            .detect_v3_surface_warning(
                actor_id,
                detection_id,
                &detection.normalized_surface,
                &forms,
                &keys,
            )
            .await?;
        for pos in existing_suggested_pos {
            if !detection.suggested_pos.contains(&pos) {
                detection.suggested_pos.push(pos);
            }
        }
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        // Empty drafts are visible across creators, just like the surface matches.
        detection.existing_draft_id = self
            .v3_empty_draft_conflict_in(&mut tx, input.kind, &keys, None)
            .await?;
        tx.commit().await.map_err(database_error)?;
        detection.requires_acknowledgement = !matches.is_empty();
        detection.matches = matches;
        detection.surface_match_page = surface_match_page;
        self.detections
            .save_v3(actor_id, &detection, V3_DETECTION_RETENTION_TTL)
            .await
            .map_err(LexiconServiceError::DetectionStore)?;
        Ok(detection)
    }

    pub async fn create_v3(
        &self,
        actor_id: Uuid,
        is_super_admin: bool,
        request_id: Uuid,
        idempotency_key: Uuid,
        mut input: CreateAdminWordV3Input,
        write_projection: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        super::annotations::normalize_annotation(&mut input.annotation)?;
        super::annotations::normalize_updates(&mut input.annotation_updates)?;
        let explicit_headwords = input.headwords.is_some();
        if let Some(headwords) = &mut input.headwords {
            normalize_submitted_headwords(headwords)?;
        }
        let request_hash = sha256_json(&input).map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{V3_CREATE_SCOPE}:{actor_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            V3_CREATE_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }
        super::annotations::lock_annotation_commands(&mut transaction).await?;
        let detection = self
            .detections
            .load_v3(actor_id, input.detection_id)
            .await
            .map_err(LexiconServiceError::DetectionStore)?
            .ok_or(LexiconServiceError::DetectionMismatch)?;
        let normalized_detection =
            NormalizedHeadword::parse(&detection.request.surface).map_err(map_surface_error)?;
        if normalized_detection.key != detection.normalized_surface {
            return Err(LexiconServiceError::InvalidField {
                field: "surface",
                message: "surface normalization does not match detection",
            });
        }
        if detection.expires_at <= Utc::now() {
            return Err(LexiconServiceError::DetectionExpired);
        }
        if detection.request.kind != input.kind {
            return Err(LexiconServiceError::DetectionMismatch);
        }
        let entry_id = Uuid::now_v7();
        let now = Utc::now();
        let mut forms = materialize_v3_detection_forms(&detection);
        let suggested_headwords = compatibility_v3_headwords(&detection, &forms)?;
        let confirmed_headwords = if let Some(headwords) = &input.headwords {
            apply_confirmed_v3_headwords(&mut forms, headwords);
            headwords.clone()
        } else {
            tracing::info!(
                actor_id = %actor_id,
                detection_id = %detection.detection_id,
                "accepted legacy V3 create body without explicit headwords"
            );
            compatibility_v3_headwords(&detection, &forms)?
        };
        let detection_basis_dialect = detection_basis_dialect_for_headwords(
            &detection.normalized_surface,
            &confirmed_headwords,
        )?;
        let initial_headword_keys = initial_v3_headword_keys(&confirmed_headwords)?;
        let verified_surface = if write_projection {
            if explicit_headwords {
                self.verify_v3_final_surface_for_create(
                    &mut transaction,
                    actor_id,
                    detection.detection_id,
                    entry_id,
                    input.kind,
                    &forms,
                    &confirmed_headwords,
                    (confirmed_headwords == suggested_headwords)
                        .then_some(detection.normalized_surface.as_str()),
                    &initial_headword_keys,
                    input.confirmed_surface_match_token.as_deref(),
                )
                .await?
            } else {
                self.verify_v3_detection_surface_for_create(
                    &mut transaction,
                    actor_id,
                    detection.detection_id,
                    input.kind,
                    &detection.normalized_surface,
                    &forms,
                    &initial_headword_keys,
                    input.confirmed_surface_match_token.as_deref(),
                )
                .await?
            }
        } else {
            None
        };
        self.apply_create_annotations(
            &mut transaction,
            actor_id,
            is_super_admin,
            request_id,
            v3_kind_string(input.kind),
            &initial_headword_keys,
            &input.annotation,
            &input.annotation_updates,
        )
        .await?;
        let meanings = DraftMeaningsStepContentV3::default();
        let catalog_parts = resolve_v3_catalog_parts(&mut transaction, &forms).await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision, annotation,
                headword_mode, source_dialect, detection_snapshot,
                created_by_admin_id, updated_by_admin_id, created_at, updated_at
            ) VALUES ($1, 3, 'en', $2, 1, $6, NULL, NULL, $3, $4, $4, $5, $5)
            "#,
        )
        .bind(entry_id)
        .bind(v3_kind_string(input.kind))
        .bind(serde_json::to_value(&detection).map_err(serialization_error)?)
        .bind(actor_id)
        .bind(now)
        .bind(&input.annotation)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_entry_state (
                entry_id, content_schema_version, origin,
                publication_canary_enabled, initial_headwords, initial_headword_keys
            ) VALUES ($1, 3, 'native', FALSE, $2, $3)
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&confirmed_headwords).map_err(serialization_error)?)
        .bind(&initial_headword_keys)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        replace_v3_forms(&mut transaction, entry_id, &forms, &catalog_parts).await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_editor_projection (
                entry_id, forms, meanings, rebuilt_revision, updated_at
            ) VALUES ($1, $2, $3, 1, $4)
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&forms).map_err(serialization_error)?)
        .bind(serde_json::to_value(&meanings).map_err(serialization_error)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_step_progress (
                entry_id, step, completed_revision, content_hash, completed_at
            ) VALUES ($1, 'basics', 1, $2, $3)
            "#,
        )
        .bind(entry_id)
        .bind(
            sha256_json(&serde_json::json!({
                "detection": detection,
                "headwords": confirmed_headwords,
            }))
            .map_err(serialization_error)?,
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let presentation = if explicit_headwords {
            confirmed_v3_headwords_presentation(&confirmed_headwords)
        } else {
            crate::lexicon::v3_projection::presentation_from_native_forms(entry_id, &forms)
                .map_err(|_| invariant_record())?
        };
        if write_projection {
            upsert_presentation(&mut transaction, entry_id, 1, &presentation).await?;
            replace_v3_surface_projection(&mut transaction, entry_id, input.kind, 1, &forms)
                .await?;
        }
        let word = AdminWordV3 {
            schema_version: 3,
            id: entry_id,
            language: EnglishLanguageV3::En,
            kind: input.kind,
            status: AdminWordStatus::Draft,
            revision: 1,
            lifecycle_revision: 1,
            annotation: input.annotation.clone(),
            annotation_revision: 1,
            published_revision: None,
            has_unpublished_changes: false,
            presentation,
            capabilities: AdminWordV3Capabilities {
                text_links: Some(true),
                publication: V3PublicationCapability::Native,
                pronunciation_normalization_version:
                    PronunciationNormalizationVersionV3::NfkcTrimLowerV1,
                sentence_associations: None,
                sentence_target_discovery: None,
                draft_relation_prebinding: None,
                sense_component_usages: None,
            },
            detection_basis_dialect,
            forms,
            meanings,
            compatibility: None,
            completed_steps: vec![PersistedWordStep::Basics],
            max_reachable_step: WordCreationStep::Forms,
            created_by: actor_id,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
            published_at: None,
        };
        let envelope = AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        };
        insert_v3_idempotency(
            &mut transaction,
            V3_CREATE_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            entry_id,
            201,
            serde_json::to_value(&envelope).map_err(serialization_error)?,
        )
        .await?;
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.create.v3",
            entry_id,
            1,
            serde_json::json!({
                "schema_version": 3,
                "explicit_headwords": explicit_headwords,
                "surface_snapshot_id": verified_surface.as_ref().map(|value| value.snapshot_id),
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = &verified_surface
            && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "created V3 entry but failed to remove surface confirmation");
        }
        Ok(envelope)
    }

    pub async fn preview_forms_impact_v3(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        mut input: PreviewFormsImpactInputV3,
        read_projection: bool,
    ) -> Result<FormsImpactResponseV3, LexiconServiceError> {
        canonicalize_v3_forms(&mut input.content)?;
        let current = self.get_v3(entry_id).await?;
        ensure_v3_active(&current)?;
        ensure_v3_revision(&current, input.base_revision)?;
        preserve_missing_component_usages(&mut input.content, &current.forms)?;
        ensure_phrase_component_ownership(current.kind, &input.content)?;
        let mut validation_tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        validate_phrase_components(
            &mut validation_tx,
            entry_id,
            current.kind,
            &mut input.content,
        )
        .await?;
        validation_tx.commit().await.map_err(database_error)?;
        let issues =
            crate::lexicon::v3_contract::validate_forms(&input.content, StepSaveIntent::Save);
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        let affected = forms_impact_v3(&current.forms, &input.content, &current.meanings)?;
        if read_projection
            && let Some(surface_match_page) = self
                .preview_v3_forms_surface_warning(
                    actor_id,
                    entry_id,
                    current.revision,
                    &input.content,
                    &affected,
                )
                .await?
        {
            return Ok(FormsImpactResponseV3 {
                schema_version: 3,
                base_revision: current.revision,
                requires_confirmation: !affected.is_empty(),
                affected,
                confirmation_token: None,
                surface_match_page: Some(surface_match_page),
            });
        }
        if affected.is_empty() {
            return Ok(FormsImpactResponseV3 {
                schema_version: 3,
                base_revision: current.revision,
                requires_confirmation: false,
                affected,
                confirmation_token: None,
                surface_match_page: None,
            });
        }
        let token = Uuid::now_v7();
        self.impacts
            .save(
                actor_id,
                token,
                &ImpactConfirmation {
                    entry_id,
                    base_revision: current.revision,
                    content_hash: sha256_json(&input.content).map_err(serialization_error)?,
                },
                IMPACT_TTL,
            )
            .await
            .map_err(LexiconServiceError::ImpactStore)?;
        Ok(FormsImpactResponseV3 {
            schema_version: 3,
            base_revision: current.revision,
            requires_confirmation: true,
            affected,
            confirmation_token: Some(token),
            surface_match_page: None,
        })
    }

    pub async fn save_forms_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        mut input: SaveFormsStepInputV3,
        write_projection: bool,
        is_super_admin: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        canonicalize_v3_forms(&mut input.content)?;
        let compatibility_source = self.get_v3(entry_id).await?;
        ensure_v3_active(&compatibility_source)?;
        ensure_v3_revision(&compatibility_source, input.base_revision)?;
        preserve_missing_component_usages(&mut input.content, &compatibility_source.forms)?;
        let issues = crate::lexicon::v3_contract::validate_forms(&input.content, input.intent);
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        if write_projection {
            LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
                .await
                .map_err(repository_error)?;
        }
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_draft_writable(&record, actor_id, is_super_admin)?;
        ensure_v3_record_active(&record)?;
        let entry_kind = parse_v3_kind(&record.kind).ok_or_else(invariant_record)?;
        ensure_phrase_component_ownership(entry_kind, &input.content)?;
        validate_phrase_components(&mut transaction, entry_id, entry_kind, &mut input.content)
            .await?;
        if record.revision != input.base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        normalize_pronunciation_audio_assets(&mut transaction, entry_id, &mut input.content)
            .await?;
        let current_forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let current_form_pos_ids = current_forms
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        let next_form_pos_ids = input
            .content
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        let pos_ownership_unchanged = current_form_pos_ids == next_form_pos_ids;
        let mut meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(record.meanings).map_err(serialization_error)?;
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut meanings);
        let forms_was_complete = record.completed_steps.iter().any(|step| step == "forms");
        let meanings_was_complete = record.completed_steps.iter().any(|step| step == "meanings");
        let affected = forms_impact_v3(&current_forms, &input.content, &meanings)?;
        if !write_projection && !affected.is_empty() {
            let Some(token) = input.confirmed_impact_token else {
                return Err(LexiconServiceError::DownstreamConfirmationRequired(
                    affected.iter().map(|item| item.node_id).collect(),
                ));
            };
            let confirmation = self
                .impacts
                .load(actor_id, token)
                .await
                .map_err(LexiconServiceError::ImpactStore)?;
            let expected_hash = sha256_json(&input.content).map_err(serialization_error)?;
            if confirmation.as_ref().is_none_or(|confirmation| {
                confirmation.entry_id != entry_id
                    || confirmation.base_revision != record.revision
                    || confirmation.content_hash != expected_hash
            }) {
                return Err(LexiconServiceError::DownstreamConfirmationRequired(
                    affected.iter().map(|item| item.node_id).collect(),
                ));
            }
        }
        let catalog_parts = resolve_v3_catalog_parts(&mut transaction, &input.content).await?;
        let next_revision = record.revision + 1;
        let surface_confirmation = if write_projection {
            Some(
                self.verify_v3_forms_surface_for_save(
                    &mut transaction,
                    actor_id,
                    entry_id,
                    parse_v3_kind(&record.kind).ok_or_else(invariant_record)?,
                    record.revision,
                    next_revision,
                    &current_forms,
                    &input.content,
                    &affected,
                    input.confirmed_surface_match_token.as_deref(),
                    input.confirmed_impact_token,
                )
                .await?,
            )
        } else {
            None
        };
        reconcile_v3_meanings_after_forms(&mut meanings, &input.content);
        let aggregate_issues =
            crate::lexicon::v3_contract::validate_aggregate_node_limit(&input.content, &meanings);
        if !aggregate_issues.is_empty() {
            return Err(v3_validation_failed(aggregate_issues));
        }
        let relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(&meanings).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        let retained_sense_ids = relational_meanings
            .pos
            .iter()
            .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
            .collect::<Vec<_>>();
        if !LexiconRepository::current_inbound_sense_refs(
            &mut transaction,
            entry_id,
            &retained_sense_ids,
        )
        .await
        .map_err(repository_error)?
        .is_empty()
        {
            return Err(LexiconServiceError::FormReferenceConflict);
        }
        let audit_node_delta = preflight_v3_form_node_identities(
            &mut transaction,
            entry_id,
            &current_forms,
            &input.content,
        )
        .await?;
        let sub_parts = LexiconRepository::catalog_sub_parts_for_reference(&mut transaction)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|part| (part.code, part.id))
            .collect::<HashMap<_, _>>();
        LexiconRepository::prepare_v3_sentence_translation_aliases(
            &mut transaction,
            entry_id,
            &meanings,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_meanings_content(
            &mut transaction,
            entry_id,
            &relational_meanings,
            &sub_parts,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_v3_sentence_translations(&mut transaction, entry_id, &meanings)
            .await
            .map_err(repository_error)?;
        replace_v3_sense_component_usages(&mut transaction, entry_id, &meanings).await?;
        // 词形保存会按新的词性集合裁剪词义内容（`reconcile_v3_meanings_after_forms`），
        // 被删掉的词性连同其变体上的音频一起从草稿里消失，引用行必须跟着重建。
        // 漏了这一步，引用行会永久指向已不存在的内容：资产既不会被回收，
        // 又会被这条词条永久「占用」，别的词条再也挂不上。
        replace_v3_audio_asset_references(&mut transaction, entry_id, &input.content, &meanings)
            .await?;
        replace_v3_forms(&mut transaction, entry_id, &input.content, &catalog_parts).await?;
        let now = Utc::now();
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET revision = $2, updated_by_admin_id = $3, updated_at = $4
            WHERE id = $1 AND content_schema_version = 3 AND revision = $5
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(actor_id)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        sqlx::query(
            r#"
            UPDATE lexicon.entry_editor_projection
            SET forms = $2, meanings = $3, rebuilt_revision = $4, updated_at = $5
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&input.content).map_err(serialization_error)?)
        .bind(serde_json::to_value(&meanings).map_err(serialization_error)?)
        .bind(next_revision)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let forms_complete = input.intent == StepSaveIntent::Complete
            || (forms_was_complete
                && crate::lexicon::v3_contract::validate_forms(
                    &input.content,
                    StepSaveIntent::Complete,
                )
                .is_empty());
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "forms",
            next_revision,
            &input.content,
            forms_complete,
        )
        .await?;
        let meaning_pos_ids = meanings
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "meanings",
            next_revision,
            &meanings,
            meanings_was_complete
                && forms_complete
                && pos_ownership_unchanged
                && next_form_pos_ids == meaning_pos_ids,
        )
        .await?;
        sqlx::query(
            r#"
            UPDATE lexicon.v3_entry_state
            SET first_v3_write_revision = COALESCE(first_v3_write_revision, $2)
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let migration_batch_id = v3_migration_batch_id(&mut transaction, entry_id).await?;
        if write_projection {
            let presentation = crate::lexicon::v3_projection::presentation_from_native_forms(
                entry_id,
                &input.content,
            )
            .map_err(|_| invariant_record())?;
            upsert_presentation(&mut transaction, entry_id, next_revision, &presentation).await?;
            replace_v3_surface_projection(
                &mut transaction,
                entry_id,
                parse_v3_kind(&record.kind).ok_or_else(invariant_record)?,
                next_revision,
                &input.content,
            )
            .await?;
            if let Some(evidence) = surface_confirmation
                .as_ref()
                .and_then(|confirmation| confirmation.evidence.as_ref())
            {
                LexiconRepository::upsert_forms_surface_acknowledgement(&mut transaction, evidence)
                    .await
                    .map_err(repository_error)?;
            } else {
                LexiconRepository::delete_forms_surface_acknowledgement(&mut transaction, entry_id)
                    .await
                    .map_err(repository_error)?;
            }
        }
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.forms.save.v3",
            entry_id,
            next_revision,
            v3_save_audit_metadata(input.intent, migration_batch_id, audit_node_delta),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = surface_confirmation.as_ref().and_then(|confirmation| {
            confirmation
                .verified_surface
                .as_ref()
                .or(confirmation.verified_impact.as_ref())
        }) && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "saved V3 forms but failed to remove surface confirmation");
        }
        let word = self.get_v3(entry_id).await?;
        Ok(AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        })
    }

    pub async fn save_meanings_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: SaveMeaningsStepInputV3,
        is_super_admin: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let SaveMeaningsStepInputV3 {
            base_revision,
            intent,
            mut content,
            ..
        } = input;
        let compatibility_source = self.get_v3(entry_id).await?;
        ensure_v3_active(&compatibility_source)?;
        ensure_v3_revision(&compatibility_source, base_revision)?;
        preserve_missing_sentence_translations(&mut content, &compatibility_source.meanings);
        preserve_missing_sense_component_usages(&mut content, &compatibility_source.meanings);
        super::text_links::preserve_missing(&mut content, &compatibility_source.meanings)?;
        let mut issues = crate::lexicon::v3_contract::validate_meanings(&content, intent);
        if intent == StepSaveIntent::Complete {
            issues.extend(
                crate::lexicon::v3_contract::validate_complete_definition_grammar(&content),
            );
        }
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut content);
        if !crate::lexicon::v3_contract::canonicalize_sentence_translations(&mut content) {
            return Err(v3_validation_failed(
                crate::lexicon::v3_contract::validate_meanings(&content, intent),
            ));
        }
        let mut translation_content = content.clone();
        let mut relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(content).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        crate::lexicon::sentence_association::clear_sentence_associations(&mut relational_meanings);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_draft_writable(&record, actor_id, is_super_admin)?;
        ensure_v3_record_active(&record)?;
        if record.revision != base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        if intent == StepSaveIntent::Complete
            && !record.completed_steps.iter().any(|step| step == "forms")
        {
            return Err(LexiconServiceError::StepNotReachable);
        }
        let forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let component_issues = validate_sense_phrase_components(
            &mut transaction,
            entry_id,
            compatibility_source.kind,
            &mut translation_content,
        )
        .await?;
        if !component_issues.is_empty() {
            return Err(v3_validation_failed(component_issues));
        }

        let audio_issues =
            validate_audio_assets(&mut transaction, entry_id, &translation_content).await?;
        if !audio_issues.is_empty() {
            return Err(v3_validation_failed(audio_issues));
        }
        super::text_links::validate_targets(&mut transaction, entry_id, &mut translation_content)
            .await?;
        let meanings_was_complete = record.completed_steps.iter().any(|step| step == "meanings");
        let mut current_v3_meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(record.meanings.clone()).map_err(serialization_error)?;
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut current_v3_meanings);
        let current_relational_meanings: DraftMeaningsStepContent = serde_json::from_value(
            serde_json::to_value(&current_v3_meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        let form_pos = forms
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        if relational_meanings
            .pos
            .iter()
            .any(|pos| !form_pos.contains(&pos.pos_id))
        {
            return Err(LexiconServiceError::UnprocessableField {
                field: "pos_id",
                message: "meanings must belong to a POS in the current V3 forms",
            });
        }
        let validation_forms = v3_meaning_validation_forms(&forms);
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &validation_forms)
            .await?;
        let rich_text_is_safe = canonicalize_meanings(&mut relational_meanings);
        let mut affected_contexts = relation_target_entry_ids(&current_relational_meanings);
        affected_contexts.extend(relation_target_entry_ids(&relational_meanings));
        affected_contexts.sort_unstable();
        affected_contexts.dedup();
        LexiconRepository::lock_surface_contexts(&mut transaction, &affected_contexts)
            .await
            .map_err(repository_error)?;
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut relational_meanings,
            ReferenceResolutionMode::Canonicalize,
            false,
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(v3_validation_failed(reference_resolution.issues));
        }
        let semantic_issues = validate_meanings(
            entry_id,
            &validation_forms,
            &relational_meanings,
            &v3_meaning_validation_headwords(),
            &catalog.sub_part_parents,
        );
        if !rich_text_is_safe
            || !meaning_storage_is_safe(
                entry_id,
                &validation_forms,
                &relational_meanings,
                &catalog.sub_part_parents,
            )
        {
            return Err(v3_validation_failed(meanings_storage_issues(
                entry_id,
                semantic_issues,
            )));
        }
        if intent == StepSaveIntent::Complete && !semantic_issues.is_empty() {
            return Err(v3_validation_failed(semantic_issues));
        }
        let mut canonical_content: DraftMeaningsStepContentV3 = serde_json::from_value(
            serde_json::to_value(&relational_meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        copy_sentence_translations(&translation_content, &mut canonical_content)?;
        restore_sense_component_usages(&translation_content, &mut canonical_content);
        restore_voice_profiles(&translation_content, &mut canonical_content);
        restore_sense_group_voice(&translation_content, &mut canonical_content);

        restore_audio_assets(
            &mut transaction,
            &translation_content,
            &mut canonical_content,
        )
        .await?;
        super::text_links::restore(&translation_content, &mut canonical_content);
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut canonical_content);
        let aggregate_issues =
            crate::lexicon::v3_contract::validate_aggregate_node_limit(&forms, &canonical_content);
        if !aggregate_issues.is_empty() {
            return Err(v3_validation_failed(aggregate_issues));
        }
        let mut proposed = proposed_nodes(&DraftFormsStepContent::default(), &relational_meanings);
        let translation_nodes = v3_translation_proposed_nodes(&canonical_content);
        let translation_ids = translation_nodes
            .iter()
            .map(|node| node.id)
            .collect::<HashSet<_>>();
        proposed.retain(|node| !translation_ids.contains(&node.id));
        proposed.extend(translation_nodes);
        // 成分节点不做 retain：V2 形状根本不产它们，撞 id 就该走 node_id_reused 报出来。
        proposed.extend(v3_component_proposed_nodes(&canonical_content));
        let proposed_ids = sorted_unique_node_ids(proposed.iter().map(|node| node.id));
        LexiconRepository::lock_node_ids(&mut transaction, &proposed_ids)
            .await
            .map_err(repository_error)?;
        let existing =
            LexiconRepository::node_identities(&mut transaction, entry_id, &proposed_ids)
                .await
                .map_err(repository_error)?;
        let node_issues =
            validate_node_identities(entry_id, &validation_forms, &proposed, &existing);
        if node_issues
            .iter()
            .any(|issue| issue.code == "stable_node_id_changed")
        {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
        if !node_issues.is_empty() {
            return Err(v3_validation_failed(node_issues));
        }
        let audit_node_delta = v3_audit_node_delta(
            entry_id,
            &v3_meaning_node_ids_with_v3_only_nodes(
                &current_relational_meanings,
                &current_v3_meanings,
            ),
            &proposed_ids,
            &existing,
        );
        let current_sense_ids = current_relational_meanings
            .pos
            .iter()
            .flat_map(|pos| &pos.senses)
            .map(|sense| sense.id)
            .collect::<HashSet<_>>();
        let next_sense_ids = relational_meanings
            .pos
            .iter()
            .flat_map(|pos| &pos.senses)
            .map(|sense| sense.id)
            .collect::<HashSet<_>>();
        let removed_sense_ids = current_sense_ids
            .difference(&next_sense_ids)
            .copied()
            .collect::<Vec<_>>();
        // An explicit relation must be removed or changed before its target sense is deleted.
        let inbound = sqlx::query_scalar::<_, Uuid>(
            "SELECT DISTINCT target_sense_id FROM lexicon.relations WHERE target_entry_id = $1 AND target_sense_id = ANY($2) AND entry_id <> $1 ORDER BY target_sense_id LIMIT 1",
        )
        .bind(entry_id)
        .bind(&removed_sense_ids)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if let Some(referenced_sense_id) = inbound {
            return Err(v3_validation_failed(vec![reference_issue(
                referenced_sense_id,
                "senses",
                "relation_target_unavailable",
                "该词义仍被其他词条关联，请先删除或修改关联再删除词义",
            )]));
        }
        LexiconRepository::prepare_v3_sentence_translation_aliases(
            &mut transaction,
            entry_id,
            &canonical_content,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_meanings_content(
            &mut transaction,
            entry_id,
            &relational_meanings,
            &catalog.sub_part_ids,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_v3_sentence_translations(
            &mut transaction,
            entry_id,
            &canonical_content,
        )
        .await
        .map_err(repository_error)?;
        replace_v3_sense_component_usages(&mut transaction, entry_id, &canonical_content).await?;
        replace_v3_audio_asset_references(&mut transaction, entry_id, &forms, &canonical_content)
            .await?;
        let next_revision = record.revision + 1;
        let now = Utc::now();
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET revision = $2, updated_by_admin_id = $3, updated_at = $4
            WHERE id = $1 AND content_schema_version = 3 AND revision = $5
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(actor_id)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        sqlx::query(
            r#"
            UPDATE lexicon.entry_editor_projection
            SET meanings = $2, rebuilt_revision = $3, updated_at = $4
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&canonical_content).map_err(serialization_error)?)
        .bind(next_revision)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let presentation_updated = sqlx::query(
            r#"
            UPDATE lexicon.entry_presentation_projection
            SET source_revision = $2, updated_at = $3
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND source_revision = $4
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if presentation_updated.rows_affected() != 1 {
            return Err(invariant_record());
        }
        sqlx::query(
            r#"
            UPDATE lexicon.surface_sources
            SET source_revision = $2,
                updated_at = $3
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND content_scope = 'draft'
              AND is_deleted = FALSE
              AND source_revision = $4
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "meanings",
            next_revision,
            &canonical_content,
            semantic_issues.is_empty()
                && (intent == StepSaveIntent::Complete || meanings_was_complete),
        )
        .await?;
        sqlx::query(
            r#"
            UPDATE lexicon.v3_entry_state
            SET first_v3_write_revision = COALESCE(first_v3_write_revision, $2)
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES (
                $1, 'lexicon.entry', $2, $3,
                'lexicon.entry.draft_meanings_saved', $4, now(), now()
            )
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(entry_id)
        .bind(next_revision)
        .bind(serde_json::json!({
            "entry_id": entry_id,
            "source_revision": next_revision,
        }))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let migration_batch_id = v3_migration_batch_id(&mut transaction, entry_id).await?;
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.meanings.save.v3",
            entry_id,
            next_revision,
            v3_save_audit_metadata(intent, migration_batch_id, audit_node_delta),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        let word = self.get_v3(entry_id).await?;
        Ok(AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        })
    }

    pub async fn validate_v3(
        &self,
        entry_id: Uuid,
        input: ValidateAdminWordV3Input,
    ) -> Result<DraftValidationResponseV3, LexiconServiceError> {
        let word = self.get_v3(entry_id).await?;
        ensure_v3_active(&word)?;
        ensure_v3_revision(&word, input.base_revision)?;
        let mut issues =
            crate::lexicon::v3_contract::validate_forms(&word.forms, StepSaveIntent::Complete);
        issues.extend(crate::lexicon::v3_contract::validate_meanings(
            &word.meanings,
            StepSaveIntent::Complete,
        ));
        issues.extend(
            crate::lexicon::v3_contract::validate_complete_definition_grammar(&word.meanings),
        );
        issues.extend(crate::lexicon::v3_contract::validate_aggregate_node_limit(
            &word.forms,
            &word.meanings,
        ));
        let validation_forms = v3_meaning_validation_forms(&word.forms);
        let mut relational_meanings: DraftMeaningsStepContent = serde_json::from_value(
            serde_json::to_value(&word.meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        crate::lexicon::sentence_association::clear_sentence_associations(&mut relational_meanings);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &validation_forms)
            .await?;
        issues.extend(validate_meanings(
            entry_id,
            &validation_forms,
            &relational_meanings,
            &v3_meaning_validation_headwords(),
            &catalog.sub_part_parents,
        ));
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut relational_meanings,
            ReferenceResolutionMode::Verify,
            false,
        )
        .await?;
        issues.extend(reference_resolution.issues);
        transaction.commit().await.map_err(database_error)?;
        Ok(DraftValidationResponseV3 {
            schema_version: 3,
            validated_revision: word.revision,
            valid: issues.is_empty(),
            issues: crate::lexicon::v3_contract::v3_issues(&issues),
        })
    }
}

fn legacy_headwords_from_record(
    record: &EntryRecord,
) -> Result<LegacyHeadwordsCompatibilityV3, LexiconServiceError> {
    match record.headword_mode.as_deref() {
        Some("unified") => Ok(LegacyHeadwordsCompatibilityV3::Unified {
            common: record
                .common_headword
                .clone()
                .ok_or_else(invariant_record)?,
        }),
        Some("distinguish") => Ok(LegacyHeadwordsCompatibilityV3::Distinguish {
            uk: record.uk_headword.clone().ok_or_else(invariant_record)?,
            us: record.us_headword.clone().ok_or_else(invariant_record)?,
            source_dialect: match record.source_dialect.as_deref() {
                Some("uk") => SourceDialect::Uk,
                Some("us") => SourceDialect::Us,
                _ => return Err(invariant_record()),
            },
        }),
        _ => Err(invariant_record()),
    }
}

fn retired_role(role: &str) -> Result<V3RetiredNodeRole, LexiconServiceError> {
    Ok(match role {
        "forms.pos" => V3RetiredNodeRole::Pos,
        "forms.form_group" => V3RetiredNodeRole::FormGroup,
        "forms.group_membership" => V3RetiredNodeRole::GroupMembership,
        "forms.concrete_form" => V3RetiredNodeRole::ConcreteForm,
        "forms.form_variant:common" => V3RetiredNodeRole::CommonVariant,
        "forms.form_variant:uk" => V3RetiredNodeRole::UkVariant,
        "forms.form_variant:us" => V3RetiredNodeRole::UsVariant,
        "forms.pronunciation" => V3RetiredNodeRole::Pronunciation,
        "forms.phrase_component_usage" => V3RetiredNodeRole::PhraseComponentUsage,
        _ => return Err(invariant_record()),
    })
}

fn ensure_v3_active(word: &AdminWordV3) -> Result<(), LexiconServiceError> {
    if word.archived_at.is_some() {
        Err(LexiconServiceError::EntryArchived)
    } else {
        Ok(())
    }
}

fn ensure_v3_record_active(record: &EntryRecord) -> Result<(), LexiconServiceError> {
    if record.content_schema_version != 3 {
        return Err(LexiconServiceError::UnsupportedSchemaVersion(
            record.content_schema_version,
        ));
    }
    if record.archived_at.is_some() {
        return Err(LexiconServiceError::EntryArchived);
    }
    Ok(())
}

fn ensure_phrase_component_ownership(
    kind: WordEntryKindV3,
    content: &DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    if kind == WordEntryKindV3::Phrase {
        return Ok(());
    }
    let has_component_usages =
        content
            .pos
            .iter()
            .flat_map(|pos| &pos.forms)
            .any(|form| match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => !common.component_usages.is_empty(),
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    !uk.component_usages.is_empty() || !us.component_usages.is_empty()
                }
            });
    if has_component_usages {
        Err(LexiconServiceError::InvalidField {
            field: "component_usages",
            message: "component usages are only available for phrase entries",
        })
    } else {
        Ok(())
    }
}

/// 词形变体级成分用词校验（保存词形步与发布共用）。目标可以是发布快照，也可以是从未发布的
/// 草稿；草稿目标在目标发布之后会在这里升级并回填 `target_publication_id`。
pub(super) async fn validate_phrase_components(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    kind: WordEntryKindV3,
    content: &mut DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    ensure_phrase_component_ownership(kind, content)?;
    if kind != WordEntryKindV3::Phrase {
        return Ok(());
    }

    for form in content.pos.iter().flat_map(|pos| &pos.forms) {
        let within_limit = match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => common.component_usages.len() <= 100,
            WordRegionalVariantsV3::UkUs { uk, us } => {
                uk.component_usages.len() <= 100 && us.component_usages.len() <= 100
            }
        };
        if !within_limit {
            return Err(invalid_phrase_component());
        }
    }

    let components = form_component_usages(content).cloned().collect::<Vec<_>>();
    let mut groups = BTreeMap::<(Uuid, Option<Uuid>), Vec<PhraseComponentUsageV3>>::new();
    for component in &components {
        if !phrase_component_literal_is_valid(phrase_component_literal(component)) {
            return Err(invalid_phrase_component());
        }
        let Some((target_word_id, target_publication_id)) = resolved_component_key(component)
        else {
            continue;
        };
        if target_word_id == entry_id {
            return Err(LexiconServiceError::InvalidField {
                field: "component_usages",
                message: "phrase component must not target the phrase itself",
            });
        }
        groups
            .entry((target_word_id, target_publication_id))
            .or_default()
            .push(component.clone());
    }
    let mut targets = HashMap::<(Uuid, Option<Uuid>), ComponentTargetWord>::new();
    for (key, members) in &groups {
        let target = resolve_component_target(tx, key.0, key.1, |candidate| {
            members
                .iter()
                .all(|member| resolved_component_matches(candidate, member))
        })
        .await?
        .ok_or_else(invalid_phrase_component)?;
        targets.insert(*key, target);
    }

    // 短语套短语只放一层：目标短语自己的成分不得再出现短语目标。发布快照不可变，草稿目标
    // 则可能事后再加短语成分，所以发布路径会再跑一遍本函数。
    let mut nested_target_ids = targets
        .values()
        .filter(|target| target.kind == WordEntryKindV3::Phrase)
        .flat_map(phrase_component_resolved_target_ids)
        .collect::<Vec<_>>();
    nested_target_ids.sort_unstable();
    nested_target_ids.dedup();
    if !nested_target_ids.is_empty() {
        let nested_phrase_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM lexicon.entries WHERE id = ANY($1) AND kind = 'phrase')",
        )
        .bind(&nested_target_ids)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if nested_phrase_exists {
            return Err(LexiconServiceError::InvalidField {
                field: "component_usages",
                message: "phrase component target must not itself reference another phrase",
            });
        }
    }

    for component in &components {
        let Some(key) = resolved_component_key(component) else {
            continue;
        };
        let target = targets.get(&key).ok_or_else(invalid_phrase_component)?;
        if !resolved_component_matches(target, component) {
            return Err(invalid_phrase_component());
        }
    }
    for usages in form_component_usage_slots(content) {
        refresh_component_targets(usages, |word_id| targets.get(&(word_id, None)));
    }
    Ok(())
}

fn form_component_usages(
    content: &DraftFormsStepContentV3,
) -> impl Iterator<Item = &PhraseComponentUsageV3> {
    content
        .pos
        .iter()
        .flat_map(|pos| &pos.forms)
        .flat_map(|form| match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => vec![&common.component_usages],
            WordRegionalVariantsV3::UkUs { uk, us } => {
                vec![&uk.component_usages, &us.component_usages]
            }
        })
        .flat_map(|usages| usages.iter())
}

fn form_component_usage_slots(
    content: &mut DraftFormsStepContentV3,
) -> impl Iterator<Item = &mut PresenceAwareVec<PhraseComponentUsageV3>> {
    content
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.forms)
        .flat_map(|form| match &mut form.regional_variants {
            WordRegionalVariantsV3::Common { common } => vec![&mut common.component_usages],
            WordRegionalVariantsV3::UkUs { uk, us } => {
                vec![&mut uk.component_usages, &mut us.component_usages]
            }
        })
}

/// 已解析成分的目标键：`(词条, 发布版本)`；发布版本缺省即草稿目标。
fn resolved_component_key(component: &PhraseComponentUsageV3) -> Option<(Uuid, Option<Uuid>)> {
    match component {
        PhraseComponentUsageV3::Resolved {
            target_word_id,
            target_publication_id,
            ..
        } => Some((*target_word_id, *target_publication_id)),
        PhraseComponentUsageV3::Unresolved { .. } => None,
    }
}

/// 已解析成分是否与目标内容一致（词性 / 词形 / 变体 / 原形同组 / 词义）。
/// 词面 / 释义文案只对已钉住发布版本的成分比对（快照不可变，等值就是客户端没篡改）；
/// 草稿目标是活的，文案由服务端在写回时刷新，不拿旧文案拒掉宿主。
fn resolved_component_matches(
    target: &ComponentTargetWord,
    component: &PhraseComponentUsageV3,
) -> bool {
    let PhraseComponentUsageV3::Resolved {
        target_publication_id,
        target_pos_id,
        target_base_form_id,
        target_sense_id,
        target_form_id,
        target_variant_id,
        target_dialect,
        target_form_type,
        target_headword,
        target_gloss,
        ..
    } = component
    else {
        return false;
    };
    let pinned_text = target_publication_id.is_some();
    phrase_component_matches_target(
        target,
        *target_pos_id,
        *target_base_form_id,
        *target_sense_id,
        *target_form_id,
        *target_variant_id,
        *target_dialect,
        target_form_type.clone(),
        pinned_text.then_some((target_headword.as_str(), target_gloss.as_str())),
    )
}

/// 草稿目标的成分写回：目标已发布就钉上当前发布版本；词面 / 释义文案按解析到的目标内容刷新
/// （草稿是活的，客户端带来的是选中当刻的文案）。只在确有变化时才取可变引用，
/// 免得 `PresenceAwareVec` 的 `DerefMut` 把「客户端没发这个字段」误标成显式提交。
fn refresh_component_targets<'a>(
    usages: &mut PresenceAwareVec<PhraseComponentUsageV3>,
    resolved_target: impl Fn(Uuid) -> Option<&'a ComponentTargetWord>,
) {
    let refreshed = |usage: &PhraseComponentUsageV3| -> Option<(Option<Uuid>, String, String)> {
        let PhraseComponentUsageV3::Resolved {
            target_word_id,
            target_publication_id: None,
            target_pos_id,
            target_sense_id,
            ..
        } = usage
        else {
            return None;
        };
        let target = resolved_target(*target_word_id)?;
        Some((
            target.publication_id(),
            target.label.clone(),
            component_target_gloss(target, *target_pos_id, *target_sense_id).unwrap_or_default(),
        ))
    };
    let changed = usages.iter().any(|usage| {
        refreshed(usage).is_some_and(|(publication_id, headword, gloss)| match usage {
            PhraseComponentUsageV3::Resolved {
                target_publication_id,
                target_headword,
                target_gloss,
                ..
            } => {
                publication_id != *target_publication_id
                    || headword != *target_headword
                    || gloss != *target_gloss
            }
            PhraseComponentUsageV3::Unresolved { .. } => false,
        })
    });
    if !changed {
        return;
    }
    for usage in usages.iter_mut() {
        let Some((publication_id, headword, gloss)) = refreshed(usage) else {
            continue;
        };
        if let PhraseComponentUsageV3::Resolved {
            target_publication_id,
            target_headword,
            target_gloss,
            ..
        } = usage
        {
            *target_publication_id = publication_id;
            *target_headword = headword;
            *target_gloss = gloss;
        }
    }
}

/// 目标短语自己的成分指向了谁。**必须同时扫 forms 与 meanings**：
/// 发布快照不可变，B1 之前存量短语的成分还挂在 forms 上，只看 meanings 会漏掉套娃检测。
fn phrase_component_resolved_target_ids(word: &ComponentTargetWord) -> Vec<Uuid> {
    let from_forms = word
        .forms
        .pos
        .iter()
        .flat_map(|pos| &pos.forms)
        .flat_map(|form| match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                common.component_usages.iter().collect::<Vec<_>>()
            }
            WordRegionalVariantsV3::UkUs { uk, us } => uk
                .component_usages
                .iter()
                .chain(&us.component_usages)
                .collect(),
        });
    let from_meanings = word
        .meanings
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| &sense.component_usages);
    from_forms
        .chain(from_meanings)
        .filter_map(|component| match component {
            PhraseComponentUsageV3::Resolved { target_word_id, .. } => Some(*target_word_id),
            PhraseComponentUsageV3::Unresolved { .. } => None,
        })
        .collect()
}

/// 目标词义在成分 / 关联里展示的释义：首条中文定义或中文例句释义，没有就空串。
fn component_target_gloss(
    target: &ComponentTargetWord,
    target_pos_id: Uuid,
    target_sense_id: Uuid,
) -> Option<String> {
    let sense = target
        .meanings
        .pos
        .iter()
        .find(|meaning_pos| meaning_pos.pos_id == target_pos_id)?
        .senses
        .iter()
        .find(|sense| sense.id == target_sense_id)?;
    Some(
        sense
            .definitions
            .iter()
            .find_map(|definition| match definition {
                WordDefinitionV3::ZhDefinition { content, .. }
                | WordDefinitionV3::ZhSentence { content, .. } => Some(content.text().to_owned()),
                WordDefinitionV3::EnDefinition { .. } | WordDefinitionV3::EnSentence { .. } => None,
            })
            .unwrap_or_default(),
    )
}

/// `expected_text` = Some((词面, 释义)) 时还要求文案等值（钉住发布版本的成分）；None 只查结构。
#[allow(clippy::too_many_arguments)]
fn phrase_component_matches_target(
    target: &ComponentTargetWord,
    target_pos_id: Uuid,
    target_base_form_id: Uuid,
    target_sense_id: Uuid,
    target_form_id: Uuid,
    target_variant_id: Uuid,
    target_dialect: Dialect,
    target_form_type: WordFormTypeV3,
    expected_text: Option<(&str, &str)>,
) -> bool {
    if expected_text.is_some_and(|(headword, _)| target.label != headword) {
        return false;
    }
    let Some(pos) = target
        .forms
        .pos
        .iter()
        .find(|pos| pos.pos_id == target_pos_id)
    else {
        return false;
    };
    let Some(form) = pos.forms.iter().find(|form| form.id == target_form_id) else {
        return false;
    };
    if form.form_type != target_form_type {
        return false;
    }
    let variant_matches = match (&form.regional_variants, target_dialect) {
        (WordRegionalVariantsV3::Common { common }, Dialect::Common) => {
            common.id == target_variant_id
        }
        (WordRegionalVariantsV3::UkUs { uk, .. }, Dialect::Uk) => uk.id == target_variant_id,
        (WordRegionalVariantsV3::UkUs { us, .. }, Dialect::Us) => us.id == target_variant_id,
        _ => false,
    };
    if !variant_matches {
        return false;
    }
    let base_is_valid = pos
        .forms
        .iter()
        .any(|candidate| candidate.id == target_base_form_id && candidate.form_type == "base")
        && (target_form_id == target_base_form_id
            || pos.form_groups.iter().any(|group| {
                let ids = group
                    .members
                    .iter()
                    .map(|member| member.form_id)
                    .collect::<HashSet<_>>();
                ids.contains(&target_form_id) && ids.contains(&target_base_form_id)
            }));
    if !base_is_valid {
        return false;
    }
    let Some(gloss) = component_target_gloss(target, target_pos_id, target_sense_id) else {
        return false;
    };
    expected_text.is_none_or(|(_, expected_gloss)| gloss == expected_gloss)
}

const fn phrase_component_id(component: &PhraseComponentUsageV3) -> Uuid {
    match component {
        PhraseComponentUsageV3::Unresolved { id, .. }
        | PhraseComponentUsageV3::Resolved { id, .. } => *id,
    }
}

fn phrase_component_literal(component: &PhraseComponentUsageV3) -> &str {
    match component {
        PhraseComponentUsageV3::Unresolved { literal, .. }
        | PhraseComponentUsageV3::Resolved { literal, .. } => literal,
    }
}

fn phrase_component_literal_is_valid(literal: &str) -> bool {
    literal.trim() == literal && !literal.is_empty() && literal.chars().count() <= 200
}

/// 成分用词 / 正文关联目标的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComponentTargetScope {
    /// 某一版发布快照；`revision` 是该发布的 `source_revision`。
    Publication { publication_id: Uuid, revision: i64 },
    /// 从未发布的草稿当前内容；`revision` 是目标 `entries.revision`。
    Draft { revision: i64 },
}

/// 校验成分用词 / 正文关联所需的目标内容，发布快照与草稿投影都能组装成它。
#[derive(Debug, Clone)]
pub(super) struct ComponentTargetWord {
    pub(super) id: Uuid,
    pub(super) kind: WordEntryKindV3,
    pub(super) label: String,
    pub(super) forms: DraftFormsStepContentV3,
    pub(super) meanings: DraftMeaningsStepContentV3,
    pub(super) scope: ComponentTargetScope,
}

impl ComponentTargetWord {
    pub(super) fn publication_id(&self) -> Option<Uuid> {
        match self.scope {
            ComponentTargetScope::Publication { publication_id, .. } => Some(publication_id),
            ComponentTargetScope::Draft { .. } => None,
        }
    }

    fn from_snapshot(snapshot: Value, publication_id: Uuid, revision: i64) -> Option<Self> {
        let word = serde_json::from_value::<AdminWordV3>(snapshot).ok()?;
        Some(Self {
            id: word.id,
            kind: word.kind,
            label: word.presentation.label,
            forms: word.forms,
            meanings: word.meanings,
            scope: ComponentTargetScope::Publication {
                publication_id,
                revision,
            },
        })
    }

    fn from_draft_row(row: ComponentTargetDraftRecord) -> Result<Self, LexiconServiceError> {
        let kind = parse_v3_kind(&row.kind).ok_or_else(invariant_record)?;
        let forms = serde_json::from_value(row.forms).map_err(serialization_error)?;
        let mut meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(row.meanings).map_err(serialization_error)?;
        crate::lexicon::v3_contract::normalize_sentence_translations(&mut meanings);
        Ok(Self {
            id: row.id,
            kind,
            label: row.label,
            forms,
            meanings,
            scope: ComponentTargetScope::Draft {
                revision: row.revision,
            },
        })
    }
}

/// 取成分用词 / 正文关联的目标（未归档 + V3 + word/phrase）。
///
/// `target_publication_id` 给定时只认那一版发布快照。未给定即「草稿目标」：目标若已有当前发布
/// 且 `probe` 认可该快照（引用的节点都在里面），就升级成发布目标——调用方据此回填
/// `target_publication_id`；否则按当前草稿内容返回。行不存在、已归档、非 V3 都返回 `None`。
pub(super) async fn resolve_component_target(
    tx: &mut Transaction<'_, Postgres>,
    target_word_id: Uuid,
    target_publication_id: Option<Uuid>,
    probe: impl Fn(&ComponentTargetWord) -> bool,
) -> Result<Option<ComponentTargetWord>, LexiconServiceError> {
    if let Some(publication_id) = target_publication_id {
        let row = sqlx::query_as::<_, (Value, i64)>(
            r#"
            SELECT publication.snapshot, publication.source_revision
            FROM lexicon.entries entry
            JOIN lexicon.entry_publications publication
              ON publication.id = $2
             AND publication.entry_id = entry.id
            WHERE entry.id = $1
              AND publication.content_schema_version = 3
              AND entry.kind IN ('word', 'phrase')
              AND entry.archived_at IS NULL
            "#,
        )
        .bind(target_word_id)
        .bind(publication_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        return Ok(row.and_then(|(snapshot, revision)| {
            ComponentTargetWord::from_snapshot(snapshot, publication_id, revision)
        }));
    }
    // 目标词条行加共享锁（与发布时锁目标同款）：读到的草稿内容与随后记入引用的 revision
    // 是同一版；目标正在保存时立即 409，而不是等锁或读到半新半旧。
    let row = LexiconRepository::component_target_draft_for_share(tx, target_word_id)
        .await
        .map_err(repository_error)?;
    let Some(mut row) = row else {
        return Ok(None);
    };
    if let (Some(publication_id), Some(snapshot), Some(revision)) = (
        row.current_publication_id,
        row.current_snapshot.take(),
        row.current_revision,
    ) && let Some(published) =
        ComponentTargetWord::from_snapshot(snapshot, publication_id, revision)
        && probe(&published)
    {
        return Ok(Some(published));
    }
    ComponentTargetWord::from_draft_row(row).map(Some)
}

/// 关键字检索用：批量取从未发布的 V3 草稿目标，不按创建者过滤。
pub(super) async fn load_draft_component_targets(
    tx: &mut Transaction<'_, Postgres>,
    entry_ids: &[Uuid],
) -> Result<Vec<ComponentTargetWord>, LexiconServiceError> {
    // 一份坏投影只该少一条候选，不该让整个检索 500：记日志后跳过。
    Ok(LexiconRepository::component_target_drafts(tx, entry_ids)
        .await
        .map_err(repository_error)?
        .into_iter()
        .filter_map(|row| {
            let entry_id = row.id;
            match ComponentTargetWord::from_draft_row(row) {
                Ok(target) => Some(target),
                Err(error) => {
                    tracing::warn!(%entry_id, %error, "skipping unreadable draft component target");
                    None
                }
            }
        })
        .collect())
}

/// 短语套短语只放一层：目标短语自身的成分不得再指向另一个短语。
async fn phrase_component_target_is_nested(
    tx: &mut Transaction<'_, Postgres>,
    target: &ComponentTargetWord,
) -> Result<bool, LexiconServiceError> {
    if target.kind != WordEntryKindV3::Phrase {
        return Ok(false);
    }
    let mut nested_target_ids = phrase_component_resolved_target_ids(target);
    nested_target_ids.sort_unstable();
    nested_target_ids.dedup();
    if nested_target_ids.is_empty() {
        return Ok(false);
    }
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM lexicon.entries WHERE id = ANY($1) AND kind = 'phrase')",
    )
    .bind(&nested_target_ids)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)
}

fn sense_component_issue(
    code: V3ValidationIssueCode,
    node_id: Uuid,
    field: &str,
    message: &str,
    node_role: &str,
    pos_id: Uuid,
    ancestor_node_ids: Vec<Uuid>,
) -> DraftValidationIssue {
    DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id,
        field: field.to_owned(),
        code: code.as_str().to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: Some(DraftNodeLocation {
            node_role: node_role.to_owned(),
            pos: None,
            pos_id: Some(pos_id),
            form_group_index: None,
            form_group_id: None,
            membership_id: None,
            form_id: None,
            variant_id: None,
            pronunciation_id: None,
            form_type: None,
            dialect: None,
            ancestor_node_ids,
        }),
    }
}

/// 释义级成分用词校验。与变体级的 400 `InvalidField` 不同，这里一律落成
/// node 级 issue（`step = meanings`），词义步才能把错误定位到具体成分。
/// 目标可以是发布快照或从未发布的草稿；草稿目标发布后在这里升级并回填 `target_publication_id`。
pub(super) async fn validate_sense_phrase_components(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    kind: WordEntryKindV3,
    content: &mut DraftMeaningsStepContentV3,
) -> Result<Vec<DraftValidationIssue>, LexiconServiceError> {
    let mut issues = Vec::new();
    if kind != WordEntryKindV3::Phrase {
        for pos in &content.pos {
            for sense in &pos.senses {
                if !sense.component_usages.is_empty() {
                    issues.push(sense_component_issue(
                        V3ValidationIssueCode::PhraseComponentNotAllowed,
                        sense.id,
                        "component_usages",
                        "component usages are only available for phrase entries",
                        crate::lexicon::node_identity::SENSE_ROLE,
                        pos.pos_id,
                        vec![pos.pos_id],
                    ));
                }
            }
        }
        return Ok(issues);
    }

    // 目标先按 (词条, 发布版本) 去重取回，让下面的 issue 收集是纯计算、不再穿插 await。
    let mut groups = BTreeMap::<(Uuid, Option<Uuid>), Vec<PhraseComponentUsageV3>>::new();
    for component in content
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| sense.component_usages.iter())
    {
        if let Some((target_word_id, target_publication_id)) = resolved_component_key(component)
            && target_word_id != entry_id
        {
            groups
                .entry((target_word_id, target_publication_id))
                .or_default()
                .push(component.clone());
        }
    }
    let mut targets = HashMap::<(Uuid, Option<Uuid>), Option<ComponentTargetWord>>::new();
    let mut nested = HashMap::<(Uuid, Option<Uuid>), bool>::new();
    for (key, members) in &groups {
        let target = resolve_component_target(tx, key.0, key.1, |candidate| {
            members
                .iter()
                .all(|member| resolved_component_matches(candidate, member))
        })
        .await?;
        if let Some(target) = target.as_ref() {
            nested.insert(*key, phrase_component_target_is_nested(tx, target).await?);
        }
        targets.insert(*key, target);
    }

    for pos in &content.pos {
        for sense in &pos.senses {
            if sense.component_usages.len() > 100 {
                issues.push(sense_component_issue(
                    V3ValidationIssueCode::PhraseComponentLimitExceeded,
                    sense.id,
                    "component_usages",
                    "a sense may carry at most 100 phrase component usages",
                    crate::lexicon::node_identity::SENSE_ROLE,
                    pos.pos_id,
                    vec![pos.pos_id],
                ));
            }
            for component in &sense.component_usages {
                let component_id = phrase_component_id(component);
                let ancestors = vec![pos.pos_id, sense.id];
                let component_issue = |code, field, message| {
                    sense_component_issue(
                        code,
                        component_id,
                        field,
                        message,
                        crate::lexicon::node_identity::PHRASE_COMPONENT_USAGE_ROLE,
                        pos.pos_id,
                        ancestors.clone(),
                    )
                };
                if !phrase_component_literal_is_valid(phrase_component_literal(component)) {
                    issues.push(component_issue(
                        V3ValidationIssueCode::PhraseComponentLiteralInvalid,
                        "literal",
                        "component literal must be trimmed and 1-200 characters long",
                    ));
                }
                let Some(key) = resolved_component_key(component) else {
                    continue;
                };
                if key.0 == entry_id {
                    issues.push(component_issue(
                        V3ValidationIssueCode::PhraseComponentSelfTarget,
                        "target",
                        "phrase component must not target the phrase itself",
                    ));
                    continue;
                }
                let Some(target) = targets.get(&key).and_then(Option::as_ref) else {
                    issues.push(component_issue(
                        V3ValidationIssueCode::PhraseComponentTargetUnavailable,
                        "target",
                        "phrase component target is missing, archived or not a V3 word",
                    ));
                    continue;
                };
                if nested.get(&key).copied().unwrap_or_default() {
                    issues.push(component_issue(
                        V3ValidationIssueCode::PhraseComponentTargetNested,
                        "target",
                        "phrase component target must not itself reference another phrase",
                    ));
                    continue;
                }
                if !resolved_component_matches(target, component) {
                    issues.push(component_issue(
                        V3ValidationIssueCode::PhraseComponentTargetStale,
                        "target",
                        "resolved component must match the target word or phrase form and sense",
                    ));
                }
            }
        }
    }
    for sense in content.pos.iter_mut().flat_map(|pos| &mut pos.senses) {
        refresh_component_targets(&mut sense.component_usages, |word_id| {
            targets.get(&(word_id, None)).and_then(Option::as_ref)
        });
    }
    Ok(issues)
}

/// 缺键 = 保持不变（旧客户端不发就别清空），显式 `[]` = 清空。按 sense id 对齐。
fn preserve_missing_sense_component_usages(
    proposed: &mut DraftMeaningsStepContentV3,
    current: &DraftMeaningsStepContentV3,
) {
    let current_by_sense = current
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .map(|sense| (sense.id, sense.component_usages.to_vec()))
        .collect::<HashMap<_, _>>();
    for sense in proposed.pos.iter_mut().flat_map(|pos| &mut pos.senses) {
        sense
            .component_usages
            .preserve_missing_from(current_by_sense.get(&sense.id).map_or(&[], Vec::as_slice));
    }
}

/// V3 → V2 → V3 往返会静默吞掉 `component_usages`（`WordSenseV2` 没这个字段且不 deny），
/// 每个落库点都要从往返前的 V3 内容回填。
pub(super) fn restore_sense_component_usages(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) {
    let by_sense = source
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .map(|sense| (sense.id, sense.component_usages.to_vec()))
        .collect::<HashMap<_, _>>();
    for sense in target.pos.iter_mut().flat_map(|pos| &mut pos.senses) {
        sense.component_usages = by_sense.get(&sense.id).cloned().unwrap_or_default().into();
    }
}

/// V3 → V2 → V3 往返会静默吞掉 `voice_profile`（`GrammarVariantV2` / `TextVariantV2` 没这个
/// 字段且不 deny），和 `component_usages` 一样，每个落库点都要从往返前的 V3 内容按节点 id 回填。
///
/// 语法结构变体与英文文本变体共用一张 id → profile 表：两者的 id 都是 `text_variants.id`，
/// 全库唯一，不会互相串。
pub(super) fn restore_voice_profiles(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) {
    let mut by_node = HashMap::new();
    for pos in &source.pos {
        for variant in pos
            .grammar_structures
            .iter()
            .flat_map(|grammar| &grammar.variants)
        {
            by_node.insert(variant.id, variant.voice_profile.clone());
        }
        for text in source_english_texts(pos) {
            for variant in crate::lexicon::v3_contract::english_text_variants(text) {
                by_node.insert(variant.id, variant.voice_profile.clone());
            }
        }
    }
    for pos in &mut target.pos {
        for variant in pos
            .grammar_structures
            .iter_mut()
            .flat_map(|grammar| &mut grammar.variants)
        {
            variant.voice_profile = by_node.get(&variant.id).cloned().flatten();
        }
        for text in target_english_texts(pos) {
            for variant in english_text_variants_mut(text) {
                variant.voice_profile = by_node.get(&variant.id).cloned().flatten();
            }
        }
    }
}

/// 单个语法结构变体最多挂几条录音。与 `GrammarVariantV3.audio_assets` 的 `max_items` 同一个数。
const MAX_VARIANT_AUDIO_ASSETS: usize = 8;

/// 按草稿内容重建音频资产的引用行。整条删掉再插，与 `replace_v3_sense_component_usages` 同款：
/// 词义保存本来就是整块替换，增量对账只会多一份出错的可能。
///
/// 这里**不删任何对象或资产行**。失去引用的资产由 `audio_assets` 的回收 worker 处理，
/// 因为删对象需要对象存储句柄，而把它塞进词库服务只为了这一处补偿并不划算；
/// 分工也与 speech 试听一致：写路径只管数据库，删除集中在一处。
pub(super) async fn replace_v3_audio_asset_references(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
    content: &DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        "DELETE FROM lexicon.v3_audio_asset_references WHERE entry_id = $1 AND scope = 'draft'",
    )
    .bind(entry_id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    // 同一段录音可以挂在多个变体上，但草稿引用每个资产只记一行（回收只问「还有没有人引用」），
    // 所以取首次出现的变体作为排障线索。
    let mut inserted = HashSet::new();
    for (variant_id, asset_ids) in requested_audio_assets(content)
        .into_iter()
        .chain(requested_pronunciation_audio_assets(forms))
    {
        for asset_id in asset_ids {
            if !inserted.insert(asset_id) {
                continue;
            }
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_audio_asset_references
                    (asset_id, entry_id, scope, publication_id, variant_id)
                VALUES ($1, $2, 'draft', NULL, $3)
                "#,
            )
            .bind(asset_id)
            .bind(entry_id)
            .bind(variant_id)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        }
    }
    Ok(())
}

/// 按变体收集草稿引用的音频资产 id，保持出现顺序。
fn requested_audio_assets(content: &DraftMeaningsStepContentV3) -> Vec<(Uuid, Vec<Uuid>)> {
    content
        .pos
        .iter()
        .flat_map(|pos| &pos.grammar_structures)
        .flat_map(|grammar| &grammar.variants)
        .filter(|variant| !variant.audio_assets.is_empty())
        .map(|variant| {
            (
                variant.id,
                variant.audio_assets.iter().map(|asset| asset.id).collect(),
            )
        })
        .collect()
}

pub(super) fn pronunciation_rows(forms: &DraftFormsStepContentV3) -> Vec<&WordPronunciationV3> {
    forms
        .pos
        .iter()
        .flat_map(|pos| &pos.forms)
        .flat_map(|form| match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                common.pronunciations.iter().collect::<Vec<_>>()
            }
            WordRegionalVariantsV3::UkUs { uk, us } => {
                uk.pronunciations.iter().chain(&us.pronunciations).collect()
            }
        })
        .collect()
}

fn pronunciation_rows_mut(forms: &mut DraftFormsStepContentV3) -> Vec<&mut WordPronunciationV3> {
    forms
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.forms)
        .flat_map(|form| match &mut form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                common.pronunciations.iter_mut().collect::<Vec<_>>()
            }
            WordRegionalVariantsV3::UkUs { uk, us } => uk
                .pronunciations
                .iter_mut()
                .chain(&mut us.pronunciations)
                .collect(),
        })
        .collect()
}

pub(super) fn requested_pronunciation_audio_assets(
    forms: &DraftFormsStepContentV3,
) -> Vec<(Uuid, Vec<Uuid>)> {
    pronunciation_rows(forms)
        .into_iter()
        .filter(|row| !row.audio_assets.is_empty())
        .map(|row| {
            (
                row.id,
                row.audio_assets.iter().map(|asset| asset.id).collect(),
            )
        })
        .collect()
}

async fn normalize_pronunciation_audio_assets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    forms: &mut DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let requested = requested_pronunciation_audio_assets(forms);
    let mut issues = validate_requested_audio_assets(tx, entry_id, &requested).await?;
    for problem in &mut issues {
        problem.step = PersistedWordStep::Forms;
        problem.field = "dict_phonetic".into();
        if let Some(location) = &mut problem.node_location {
            location.node_role = "forms.pronunciation".into();
            location.pronunciation_id = Some(problem.node_id);
        }
    }
    if !issues.is_empty() {
        return Err(v3_validation_failed(issues));
    }
    let canonical = load_audio_asset_metadata(tx, &requested).await?;
    for row in pronunciation_rows_mut(forms) {
        row.audio_assets = row
            .audio_assets
            .iter()
            .map(|asset| {
                canonical
                    .get(&asset.id)
                    .cloned()
                    .ok_or_else(invariant_record)
            })
            .collect::<Result<_, _>>()?;
    }
    Ok(())
}

/// 校验草稿引用的音频资产：条数、变体内不重复、资产存在、且没有被别的词条占用。
/// 跨词条引用必须拦住——回收是按「还有没有人引用」判定的，允许共享会让一次删除
/// 波及另一条词条的历史发布。
async fn validate_audio_assets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &DraftMeaningsStepContentV3,
) -> Result<Vec<DraftValidationIssue>, LexiconServiceError> {
    validate_requested_audio_assets(tx, entry_id, &requested_audio_assets(content)).await
}

async fn validate_requested_audio_assets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    requested: &[(Uuid, Vec<Uuid>)],
) -> Result<Vec<DraftValidationIssue>, LexiconServiceError> {
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    let mut issues = Vec::new();
    for (variant_id, asset_ids) in requested {
        if asset_ids.len() > MAX_VARIANT_AUDIO_ASSETS {
            issues.push(audio_asset_issue(
                *variant_id,
                &format!(
                    "a pronunciation or grammar variant may reference at most {MAX_VARIANT_AUDIO_ASSETS} audio assets"
                ),
            ));
        }
        let mut seen = HashSet::new();
        if asset_ids.iter().any(|id| !seen.insert(*id)) {
            issues.push(audio_asset_issue(
                *variant_id,
                "a pronunciation or grammar variant must not reference the same audio asset twice",
            ));
        }
    }

    // 一次查回「存在」与「被别人占用」，让下面的 issue 收集是纯计算、不再穿插 await。
    let unique_ids = requested
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        SELECT asset.id,
               EXISTS (
                   SELECT 1 FROM lexicon.v3_audio_asset_references reference
                   WHERE reference.asset_id = asset.id AND reference.entry_id <> $2
               ) AS taken
        FROM lexicon.audio_assets asset
        WHERE asset.id = ANY($1)
        -- 与回收 worker 的 FOR UPDATE 串行：不加锁的话，校验通过之后、引用行写入之前，
        -- worker 可能刚好把这条资产的对象删掉，留下引用完好但播不出声的资产。
        FOR SHARE OF asset
        "#,
    )
    .bind(&unique_ids)
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    let mut known = HashSet::new();
    let mut taken = HashSet::new();
    for row in rows {
        let id: Uuid = row.get("id");
        if row.get::<bool, _>("taken") {
            taken.insert(id);
        }
        known.insert(id);
    }

    for (variant_id, asset_ids) in requested {
        for asset_id in asset_ids {
            if !known.contains(asset_id) {
                issues.push(audio_asset_issue(*variant_id, "audio asset does not exist"));
            } else if taken.contains(asset_id) {
                issues.push(audio_asset_issue(
                    *variant_id,
                    "audio asset is already used by another entry",
                ));
            }
        }
    }
    Ok(issues)
}

/// 直接复用 `meanings_issue`，与 `voice_profile_invalid` 逐字同形。
/// 自己拼 `DraftValidationIssue` 而把 `node_location` 留成 `None` 的话，上 wire 的
/// `node_role` 会变成 `entry`——那是整词条级错误的角色，前端按它分发会走到另一条分支，
/// 跳不到出问题的那个变体。
fn audio_asset_issue(variant_id: Uuid, message: &str) -> DraftValidationIssue {
    crate::lexicon::v3_contract::meanings_issue(
        V3ValidationIssueCode::AudioAssetInvalid,
        "audio_assets",
        variant_id,
        message,
    )
}

/// `audio_assets` 和 `voice_profile` 一样活不过 V2 往返，必须按节点 id 回填。
/// 但这里不是「把请求里的值搬回来」——除 id 外的字段一律以数据库为准重新灌入，
/// 客户端回传的展示元数据不作数，省掉一整套「改了就 422」的比对。
pub(super) async fn restore_audio_assets(
    tx: &mut Transaction<'_, Postgres>,
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let requested = requested_audio_assets(source);
    if requested.is_empty() {
        return Ok(());
    }
    let canonical = load_audio_asset_metadata(tx, &requested).await?;

    let by_variant = requested.into_iter().collect::<HashMap<_, _>>();
    for variant in target
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.grammar_structures)
        .flat_map(|grammar| &mut grammar.variants)
    {
        variant.audio_assets = by_variant
            .get(&variant.id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| canonical.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default();
    }
    Ok(())
}

async fn load_audio_asset_metadata(
    tx: &mut Transaction<'_, Postgres>,
    requested: &[(Uuid, Vec<Uuid>)],
) -> Result<HashMap<Uuid, AudioAsset>, LexiconServiceError> {
    if requested.is_empty() {
        return Ok(HashMap::new());
    }
    let unique_ids = requested
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        SELECT id, locale, gender, content_type, size_bytes, duration_ms, original_name, created_at
        FROM lexicon.audio_assets WHERE id = ANY($1)
        "#,
    )
    .bind(&unique_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    let mut canonical = HashMap::new();
    for row in rows {
        let id: Uuid = row.get("id");
        let locale: String = row.get("locale");
        let gender: String = row.get("gender");
        canonical.insert(
            id,
            AudioAsset {
                id,
                locale: AudioAssetLocale::parse(&locale).ok_or_else(invariant_record)?,
                gender: AudioAssetGender::parse(&gender).ok_or_else(invariant_record)?,
                content_type: row.get("content_type"),
                size_bytes: row.get("size_bytes"),
                duration_ms: row.get("duration_ms"),
                original_name: row.get("original_name"),
                created_at: row.get("created_at"),
            },
        );
    }

    Ok(canonical)
}

pub(super) fn source_english_texts(pos: &WordPosMeaningsV3) -> Vec<&EnglishTextV3> {
    pos.senses
        .iter()
        .flat_map(|sense| {
            sense
                .definitions
                .iter()
                .filter_map(|definition| match definition {
                    WordDefinitionV3::EnDefinition { content, .. }
                    | WordDefinitionV3::EnSentence { content, .. } => Some(content),
                    _ => None,
                })
                .chain(sense.sentences.iter().map(|sentence| &sentence.en_text))
        })
        .collect()
}

pub(super) fn target_english_texts(pos: &mut WordPosMeaningsV3) -> Vec<&mut EnglishTextV3> {
    pos.senses
        .iter_mut()
        .flat_map(|sense| {
            sense
                .definitions
                .iter_mut()
                .filter_map(|definition| match definition {
                    WordDefinitionV3::EnDefinition { content, .. }
                    | WordDefinitionV3::EnSentence { content, .. } => Some(content),
                    _ => None,
                })
                .chain(
                    sense
                        .sentences
                        .iter_mut()
                        .map(|sentence| &mut sentence.en_text),
                )
        })
        .collect()
}

pub(super) fn english_text_variants_mut(
    content: &mut EnglishTextV3,
) -> Vec<&mut RichTextVariantV3> {
    match content {
        EnglishTextV3::Unified { common } => vec![common],
        EnglishTextV3::Distinguish { uk, us, .. } => [uk, us]
            .into_iter()
            .filter_map(|slot| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => Some(variant),
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}

/// V3 → V2 → V3 往返会把每句的多档 `zh_translations` 塌成 1 档（`WordSentenceV2` 只有单条
/// `zh_text`，回程 `normalize_sentence_translations` 只按别名补 1 档），发布路径的每个往返点都要
/// 从往返前的 V3 内容按 sentence id 回填。源里没有的句子（正常不会发生）保留 normalize 的结果，
/// 不塞空档——空 `zh_translations` 是非法态。
pub(super) fn restore_sentence_zh_translations(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) {
    let by_sentence = source
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| &sense.sentences)
        .map(|sentence| (sentence.id, sentence.zh_translations.to_vec()))
        .collect::<HashMap<_, _>>();
    for sentence in target
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.senses)
        .flat_map(|sense| &mut sense.sentences)
    {
        if let Some(translations) = by_sentence.get(&sentence.id) {
            sentence.zh_translations = translations.clone().into();
        }
    }
}

fn v3_component_proposed_nodes(content: &DraftMeaningsStepContentV3) -> Vec<ProposedNode> {
    content
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| {
            sense
                .component_usages
                .iter()
                .map(move |component| ProposedNode {
                    id: phrase_component_id(component),
                    node_type: "phrase_component_usage",
                    step: PersistedWordStep::Meanings,
                    parent_node_id: Some(sense.id),
                    node_role: crate::lexicon::node_identity::PHRASE_COMPONENT_USAGE_ROLE
                        .to_owned(),
                    stable_slot: false,
                })
        })
        .collect()
}

/// 释义级成分行整表 delete-then-insert：新表与 `lexicon.senses` 之间没有外键，
/// `replace_meanings_content` 也不硬删 `lexicon.nodes`，清理只能由写入侧自己负责。
/// **`replace_meanings_content` 的每个 V3 可达调用点都必须跟一次本函数**，
/// 包括发布路径上 `sync_canonical_meanings` 内部那次。
pub(super) async fn replace_v3_sense_component_usages(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    sqlx::query("DELETE FROM lexicon.v3_phrase_sense_component_usages WHERE entry_id = $1")
        .bind(entry_id)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    for sense in content.pos.iter().flat_map(|pos| &pos.senses) {
        for (ordinal, component) in sense.component_usages.iter().enumerate() {
            upsert_v3_node(
                tx,
                phrase_component_id(component),
                entry_id,
                "phrase_component_usage",
                Some(sense.id),
                crate::lexicon::node_identity::PHRASE_COMPONENT_USAGE_ROLE,
            )
            .await?;
            match component {
                PhraseComponentUsageV3::Unresolved { id, literal } => {
                    sqlx::query(
                        r#"
                        INSERT INTO lexicon.v3_phrase_sense_component_usages (
                            id, entry_id, sense_id, ordinal, state, literal
                        ) VALUES ($1, $2, $3, $4, 'unresolved', $5)
                        "#,
                    )
                    .bind(id)
                    .bind(entry_id)
                    .bind(sense.id)
                    .bind(ordinal as i16)
                    .bind(literal)
                    .execute(&mut **tx)
                    .await
                    .map_err(database_error)?;
                }
                PhraseComponentUsageV3::Resolved {
                    id,
                    literal,
                    target_word_id,
                    target_publication_id,
                    target_pos_id,
                    target_base_form_id,
                    target_sense_id,
                    target_form_id,
                    target_variant_id,
                    target_dialect,
                    target_form_type,
                    target_headword,
                    target_gloss,
                } => {
                    sqlx::query(
                        r#"
                        INSERT INTO lexicon.v3_phrase_sense_component_usages (
                            id, entry_id, sense_id, ordinal, state, literal,
                            target_entry_id, target_publication_id, target_pos_id,
                            target_base_form_id, target_sense_id, target_form_id,
                            target_variant_id, target_dialect, target_form_type,
                            target_headword_snapshot, target_gloss_snapshot
                        ) VALUES (
                            $1, $2, $3, $4, 'resolved', $5, $6, $7, $8, $9,
                            $10, $11, $12, $13, $14, $15, $16
                        )
                        "#,
                    )
                    .bind(id)
                    .bind(entry_id)
                    .bind(sense.id)
                    .bind(ordinal as i16)
                    .bind(literal)
                    .bind(target_word_id)
                    .bind(target_publication_id)
                    .bind(target_pos_id)
                    .bind(target_base_form_id)
                    .bind(target_sense_id)
                    .bind(target_form_id)
                    .bind(target_variant_id)
                    .bind(crate::lexicon::node_identity::dialect_name(*target_dialect))
                    .bind(v3_form_type_name(target_form_type))
                    .bind(target_headword)
                    .bind(target_gloss)
                    .execute(&mut **tx)
                    .await
                    .map_err(database_error)?;
                }
            }
        }
    }
    Ok(())
}

fn invalid_phrase_component() -> LexiconServiceError {
    LexiconServiceError::InvalidField {
        field: "component_usages",
        message: "resolved component must match a published word or phrase form and sense",
    }
}

fn ensure_v3_revision(word: &AdminWordV3, revision: i64) -> Result<(), LexiconServiceError> {
    if word.revision == revision {
        Ok(())
    } else {
        Err(LexiconServiceError::RevisionConflict {
            current_revision: word.revision,
        })
    }
}

fn forms_impact_v3(
    current: &DraftFormsStepContentV3,
    proposed: &DraftFormsStepContentV3,
    current_meanings: &DraftMeaningsStepContentV3,
) -> Result<Vec<FormsImpactItemV3>, LexiconServiceError> {
    let proposed_ids = v3_form_node_types(proposed);
    let mut affected = v3_form_node_types(current)
        .into_iter()
        .filter_map(|(id, node_type)| {
            (!proposed_ids.contains_key(&id)).then_some(FormsImpactItemV3 {
                node_id: id,
                node_type,
                reason: "node_removed_from_draft".to_owned(),
            })
        })
        .collect::<Vec<_>>();
    let mut proposed_meanings = current_meanings.clone();
    reconcile_v3_meanings_after_forms(&mut proposed_meanings, proposed);
    let proposed_meaning_ids = v3_meaning_node_types(&proposed_meanings)?;
    affected.extend(
        v3_meaning_node_types(current_meanings)?
            .into_iter()
            .filter_map(|(id, node_type)| {
                (!proposed_meaning_ids.contains_key(&id)).then_some(FormsImpactItemV3 {
                    node_id: id,
                    node_type,
                    reason: "downstream_node_removed_with_pos".to_owned(),
                })
            }),
    );
    affected.sort_by_key(|item| item.node_id);
    Ok(affected)
}

fn reconcile_v3_meanings_after_forms(
    meanings: &mut DraftMeaningsStepContentV3,
    forms: &DraftFormsStepContentV3,
) {
    let active_pos = forms
        .pos
        .iter()
        .map(|pos| pos.pos_id)
        .collect::<HashSet<_>>();
    meanings.pos.retain(|pos| active_pos.contains(&pos.pos_id));
    meanings.pos.sort_by_key(|meaning_pos| {
        forms
            .pos
            .iter()
            .position(|form_pos| form_pos.pos_id == meaning_pos.pos_id)
            .unwrap_or(usize::MAX)
    });
}

fn v3_meaning_node_types(
    content: &DraftMeaningsStepContentV3,
) -> Result<HashMap<Uuid, FormsImpactNodeTypeV3>, LexiconServiceError> {
    let relational: DraftMeaningsStepContent =
        serde_json::from_value(serde_json::to_value(content).map_err(serialization_error)?)
            .map_err(serialization_error)?;
    let mut types = proposed_nodes(&DraftFormsStepContent::default(), &relational)
        .into_iter()
        .filter(|node| node.step == PersistedWordStep::Meanings)
        .filter_map(|node| {
            let node_type = match node.node_type {
                "grammar_structure" => FormsImpactNodeTypeV3::GrammarStructure,
                "text_variant" => FormsImpactNodeTypeV3::TextVariant,
                "sense" => FormsImpactNodeTypeV3::Sense,
                "definition" => FormsImpactNodeTypeV3::Definition,
                "sentence" => FormsImpactNodeTypeV3::Sentence,
                "relation" => FormsImpactNodeTypeV3::Relation,
                // sense groups are top-level and are retained by forms saves.
                "sense_group" => return None,
                _ => return None,
            };
            Some((node.id, node_type))
        })
        .collect::<HashMap<_, _>>();
    // 释义级成分在 V2 形状里不存在，词性被删时同样要出现在影响清单上。
    types.extend(
        v3_component_proposed_nodes(content)
            .into_iter()
            .map(|node| (node.id, FormsImpactNodeTypeV3::PhraseComponentUsage)),
    );
    Ok(types)
}

fn v3_form_node_ids(content: &DraftFormsStepContentV3) -> Vec<Uuid> {
    let mut ids = v3_form_node_types(content).into_keys().collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn v3_meaning_node_ids(content: &DraftMeaningsStepContent) -> Vec<Uuid> {
    sorted_unique_node_ids(
        proposed_nodes(&DraftFormsStepContent::default(), content)
            .into_iter()
            .map(|node| node.id),
    )
}

fn v3_translation_proposed_nodes(content: &DraftMeaningsStepContentV3) -> Vec<ProposedNode> {
    content
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| &sense.sentences)
        .flat_map(|sentence| {
            sentence
                .zh_translations
                .iter()
                .map(|translation| ProposedNode {
                    id: translation.id,
                    node_type: "text_variant",
                    step: PersistedWordStep::Meanings,
                    parent_node_id: Some(sentence.id),
                    node_role: crate::lexicon::node_identity::SENTENCE_TRANSLATION_ROLE.to_owned(),
                    stable_slot: false,
                })
        })
        .collect()
}

/// V2 形状看不见的 V3-only 节点（多档翻译、释义级成分）要一并算进节点增量，
/// 否则审计会把它们当成凭空出现。
fn v3_meaning_node_ids_with_v3_only_nodes(
    relational: &DraftMeaningsStepContent,
    v3: &DraftMeaningsStepContentV3,
) -> Vec<Uuid> {
    sorted_unique_node_ids(
        v3_meaning_node_ids(relational)
            .into_iter()
            .chain(
                v3_translation_proposed_nodes(v3)
                    .into_iter()
                    .map(|node| node.id),
            )
            .chain(
                v3_component_proposed_nodes(v3)
                    .into_iter()
                    .map(|node| node.id),
            ),
    )
}

fn copy_sentence_translations(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let translations = source
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| &sense.sentences)
        .map(|sentence| (sentence.id, sentence.zh_translations.clone()))
        .collect::<HashMap<_, _>>();
    for sentence in target
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.senses)
        .flat_map(|sense| &mut sense.sentences)
    {
        sentence.zh_translations = translations
            .get(&sentence.id)
            .cloned()
            .ok_or_else(invariant_record)?;
    }
    Ok(())
}

fn preserve_missing_sentence_translations(
    proposed: &mut DraftMeaningsStepContentV3,
    current: &DraftMeaningsStepContentV3,
) {
    let current_by_sentence = current
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .flat_map(|sense| &sense.sentences)
        .map(|sentence| (sentence.id, sentence.zh_translations.to_vec()))
        .collect::<HashMap<_, _>>();
    for sentence in proposed
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.senses)
        .flat_map(|sense| &mut sense.sentences)
    {
        sentence.zh_translations.preserve_missing_from(
            current_by_sentence
                .get(&sentence.id)
                .map_or(&[], Vec::as_slice),
        );
    }
}

fn sorted_unique_node_ids(ids: impl IntoIterator<Item = Uuid>) -> Vec<Uuid> {
    ids.into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn v3_form_proposed_nodes(content: &DraftFormsStepContentV3) -> Vec<ProposedNode> {
    let mut nodes = Vec::new();
    for pos in &content.pos {
        nodes.push(ProposedNode {
            id: pos.pos_id,
            node_type: "pos",
            step: PersistedWordStep::Forms,
            parent_node_id: None,
            node_role: "forms.pos".to_owned(),
            stable_slot: false,
        });
        for group in &pos.form_groups {
            nodes.push(ProposedNode {
                id: group.id,
                node_type: "form_group",
                step: PersistedWordStep::Forms,
                parent_node_id: Some(pos.pos_id),
                node_role: "forms.form_group".to_owned(),
                stable_slot: false,
            });
            for membership in &group.members {
                nodes.push(ProposedNode {
                    id: membership.id,
                    node_type: "group_membership",
                    step: PersistedWordStep::Forms,
                    parent_node_id: Some(group.id),
                    node_role: "forms.group_membership".to_owned(),
                    stable_slot: false,
                });
            }
        }
        for form in &pos.forms {
            nodes.push(ProposedNode {
                id: form.id,
                node_type: "concrete_form",
                step: PersistedWordStep::Forms,
                parent_node_id: Some(pos.pos_id),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            });
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => push_v3_form_variant_nodes(
                    &mut nodes,
                    form.id,
                    common.id,
                    "common",
                    common.pronunciations.iter().map(|value| value.id),
                    common.component_usages.iter().map(|value| match value {
                        PhraseComponentUsageV3::Unresolved { id, .. }
                        | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                    }),
                ),
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    push_v3_form_variant_nodes(
                        &mut nodes,
                        form.id,
                        uk.id,
                        "uk",
                        uk.pronunciations.iter().map(|value| value.id),
                        uk.component_usages.iter().map(|value| match value {
                            PhraseComponentUsageV3::Unresolved { id, .. }
                            | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                        }),
                    );
                    push_v3_form_variant_nodes(
                        &mut nodes,
                        form.id,
                        us.id,
                        "us",
                        us.pronunciations.iter().map(|value| value.id),
                        us.component_usages.iter().map(|value| match value {
                            PhraseComponentUsageV3::Unresolved { id, .. }
                            | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                        }),
                    );
                }
            }
        }
    }
    nodes
}

fn push_v3_form_variant_nodes(
    nodes: &mut Vec<ProposedNode>,
    form_id: Uuid,
    variant_id: Uuid,
    dialect: &str,
    pronunciation_ids: impl IntoIterator<Item = Uuid>,
    component_ids: impl IntoIterator<Item = Uuid>,
) {
    nodes.push(ProposedNode {
        id: variant_id,
        node_type: "form_variant",
        step: PersistedWordStep::Forms,
        parent_node_id: Some(form_id),
        node_role: format!("forms.form_variant:{dialect}"),
        stable_slot: true,
    });
    nodes.extend(pronunciation_ids.into_iter().map(|id| ProposedNode {
        id,
        node_type: "pronunciation",
        step: PersistedWordStep::Forms,
        parent_node_id: Some(variant_id),
        node_role: "forms.pronunciation".to_owned(),
        stable_slot: false,
    }));
    nodes.extend(component_ids.into_iter().map(|id| ProposedNode {
        id,
        node_type: "phrase_component_usage",
        step: PersistedWordStep::Forms,
        parent_node_id: Some(variant_id),
        node_role: "forms.phrase_component_usage".to_owned(),
        stable_slot: false,
    }));
}

async fn preflight_v3_form_node_identities(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    current: &DraftFormsStepContentV3,
    proposed_content: &DraftFormsStepContentV3,
) -> Result<V3AuditNodeDelta, LexiconServiceError> {
    let proposed = v3_form_proposed_nodes(proposed_content);
    let proposed_ids = sorted_unique_node_ids(proposed.iter().map(|node| node.id));
    LexiconRepository::lock_node_ids(tx, &proposed_ids)
        .await
        .map_err(repository_error)?;
    let existing = LexiconRepository::node_identities(tx, entry_id, &proposed_ids)
        .await
        .map_err(repository_error)?;
    let mut locator_forms = v3_meaning_validation_forms(proposed_content);
    for (locator_pos, proposed_pos) in locator_forms.pos.iter_mut().zip(&proposed_content.pos) {
        locator_pos.form_groups = proposed_pos
            .form_groups
            .iter()
            .map(|group| WordFormGroupV2 {
                id: group.id,
                is_regular: group.is_regular,
                slots: Vec::new(),
            })
            .collect();
    }
    let node_issues = validate_node_identities(entry_id, &locator_forms, &proposed, &existing);
    if node_issues
        .iter()
        .any(|issue| issue.code == "stable_node_id_changed")
    {
        return Err(LexiconServiceError::StableNodeIdChanged);
    }
    if !node_issues.is_empty() {
        return Err(v3_validation_failed(node_issues));
    }
    Ok(v3_audit_node_delta(
        entry_id,
        &v3_form_node_ids(current),
        &proposed_ids,
        &existing,
    ))
}

fn v3_audit_node_delta(
    entry_id: Uuid,
    current_ids: &[Uuid],
    proposed_ids: &[Uuid],
    existing: &[NodeIdentityRecord],
) -> V3AuditNodeDelta {
    let current = current_ids.iter().copied().collect::<BTreeSet<_>>();
    let proposed = proposed_ids.iter().copied().collect::<BTreeSet<_>>();
    let persisted = existing
        .iter()
        .filter(|node| node.entry_id == entry_id)
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    V3AuditNodeDelta {
        generated_node_ids: proposed.difference(&persisted).copied().collect(),
        changed_node_ids: proposed.intersection(&persisted).copied().collect(),
        retired_node_ids: current.difference(&proposed).copied().collect(),
    }
}

fn v3_save_audit_metadata(
    intent: StepSaveIntent,
    migration_batch_id: Option<Uuid>,
    delta: V3AuditNodeDelta,
) -> Value {
    serde_json::json!({
        "schema_version": 3,
        "migration_batch_id": migration_batch_id,
        "generated_node_ids": delta.generated_node_ids,
        "changed_node_ids": delta.changed_node_ids,
        "retired_node_ids": delta.retired_node_ids,
        "intent": intent,
    })
}

async fn v3_migration_batch_id(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Option<Uuid>, LexiconServiceError> {
    sqlx::query_scalar("SELECT migration_batch_id FROM lexicon.v3_entry_state WHERE entry_id = $1")
        .bind(entry_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)
}

fn v3_form_node_types(content: &DraftFormsStepContentV3) -> HashMap<Uuid, FormsImpactNodeTypeV3> {
    let mut nodes = HashMap::new();
    for pos in &content.pos {
        nodes.insert(pos.pos_id, FormsImpactNodeTypeV3::Pos);
        for group in &pos.form_groups {
            nodes.insert(group.id, FormsImpactNodeTypeV3::FormGroup);
            for membership in &group.members {
                nodes.insert(membership.id, FormsImpactNodeTypeV3::Membership);
            }
        }
        for form in &pos.forms {
            nodes.insert(form.id, FormsImpactNodeTypeV3::Form);
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    nodes.insert(common.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &common.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                    for component in &common.component_usages {
                        nodes.insert(
                            match component {
                                PhraseComponentUsageV3::Unresolved { id, .. }
                                | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                            },
                            FormsImpactNodeTypeV3::PhraseComponentUsage,
                        );
                    }
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    nodes.insert(uk.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &uk.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                    for component in &uk.component_usages {
                        nodes.insert(
                            match component {
                                PhraseComponentUsageV3::Unresolved { id, .. }
                                | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                            },
                            FormsImpactNodeTypeV3::PhraseComponentUsage,
                        );
                    }
                    nodes.insert(us.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &us.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                    for component in &us.component_usages {
                        nodes.insert(
                            match component {
                                PhraseComponentUsageV3::Unresolved { id, .. }
                                | PhraseComponentUsageV3::Resolved { id, .. } => *id,
                            },
                            FormsImpactNodeTypeV3::PhraseComponentUsage,
                        );
                    }
                }
            }
        }
    }
    nodes
}

async fn resolve_v3_catalog_parts(
    tx: &mut Transaction<'_, Postgres>,
    content: &DraftFormsStepContentV3,
) -> Result<HashMap<String, Uuid>, LexiconServiceError> {
    let form_codes = content
        .pos
        .iter()
        .flat_map(|p| p.forms.iter().map(|f| f.form_type.clone()))
        .collect::<Vec<_>>();
    let configured = LexiconRepository::form_types_for_reference(tx, &form_codes)
        .await
        .map_err(repository_error)?;
    let mut issues = Vec::new();
    for pos in &content.pos {
        for form in &pos.forms {
            if !configured.contains(&form.form_type) {
                issues.push(sense_component_issue(
                    V3ValidationIssueCode::InvalidFormTypeForPartOfSpeech,
                    form.id,
                    "form_type",
                    "词形类型不存在或已删除，请刷新配置",
                    "forms.concrete_form",
                    pos.pos_id,
                    vec![pos.pos_id],
                ));
            }
        }
    }
    if !issues.is_empty() {
        return Err(LexiconServiceError::ValidationFailedV3(
            crate::lexicon::v3_contract::v3_issues(&issues),
        ));
    }
    let codes = content
        .pos
        .iter()
        .map(|pos| pos.pos.clone())
        .collect::<Vec<_>>();
    let parts = LexiconRepository::catalog_parts_for_reference(tx, &codes)
        .await
        .map_err(repository_error)?;
    let mapped = parts
        .into_iter()
        .map(|part| (part.code, part.id))
        .collect::<HashMap<_, _>>();
    if mapped.len() != codes.len() {
        return Err(LexiconServiceError::CatalogMismatch);
    }
    Ok(mapped)
}

async fn replace_v3_forms(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &DraftFormsStepContentV3,
    catalog_parts: &HashMap<String, Uuid>,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        UPDATE lexicon.nodes
        SET removed_from_draft_at = now()
        WHERE entry_id = $1
          AND removed_from_draft_at IS NULL
          AND node_role = ANY($2)
        "#,
    )
    .bind(entry_id)
    .bind([
        "forms.pos",
        "forms.form_group",
        "forms.group_membership",
        "forms.concrete_form",
        "forms.form_variant:common",
        "forms.form_variant:uk",
        "forms.form_variant:us",
        "forms.pronunciation",
        "forms.phrase_component_usage",
    ])
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for statement in [
        "DELETE FROM lexicon.v3_phrase_variant_component_usages WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_pronunciations WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_group_memberships WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_concrete_forms WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_groups WHERE entry_id = $1",
    ] {
        sqlx::query(statement)
            .bind(entry_id)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
    }
    let active_pos = content.pos.iter().map(|pos| pos.pos_id).collect::<Vec<_>>();
    sqlx::query(
        r#"
        DELETE FROM lexicon.entry_pos
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND NOT (id = ANY($2))
        "#,
    )
    .bind(entry_id)
    .bind(&active_pos)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    // Move every retained POS ordinal above the current range before assigning
    // the submitted 0..N order. The V3 ordinal index is immediate (not
    // deferrable), so a direct A/B swap would otherwise collide on the first
    // row update.
    sqlx::query(
        r#"
        WITH ordinal_bound AS (
            SELECT COALESCE(MAX(sort_order), 0) + 1 AS offset
            FROM lexicon.entry_pos
            WHERE entry_id = $1 AND content_schema_version = 3
        )
        UPDATE lexicon.entry_pos AS pos
        SET sort_order = pos.sort_order + ordinal_bound.offset
        FROM ordinal_bound
        WHERE pos.entry_id = $1 AND pos.content_schema_version = 3
        "#,
    )
    .bind(entry_id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    for (pos_ordinal, pos) in content.pos.iter().enumerate() {
        upsert_v3_node(tx, pos.pos_id, entry_id, "pos", None, "forms.pos").await?;
        let part_id = catalog_parts
            .get(&pos.pos)
            .copied()
            .ok_or(LexiconServiceError::CatalogMismatch)?;
        let result = sqlx::query(
            r#"
            INSERT INTO lexicon.entry_pos (
                id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode,
                sort_order, content_schema_version
            ) VALUES ($1, $2, $3, $4, $5, $6, 3)
            ON CONFLICT (id) DO UPDATE
            SET spelling_mode = EXCLUDED.spelling_mode,
                phonetic_mode = EXCLUDED.phonetic_mode,
                sort_order = EXCLUDED.sort_order
            WHERE lexicon.entry_pos.entry_id = EXCLUDED.entry_id
              AND lexicon.entry_pos.content_schema_version = 3
              AND lexicon.entry_pos.part_of_speech_id = EXCLUDED.part_of_speech_id
            "#,
        )
        .bind(pos.pos_id)
        .bind(entry_id)
        .bind(part_id)
        .bind(pos.dialect_rules.spelling_mode.as_str())
        .bind(pos.dialect_rules.phonetic_mode.as_str())
        .bind(pos_ordinal as i32)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        if result.rows_affected() != 1 {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }

        for (group_ordinal, group) in pos.form_groups.iter().enumerate() {
            upsert_v3_node(
                tx,
                group.id,
                entry_id,
                "form_group",
                Some(pos.pos_id),
                "forms.form_group",
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_form_groups (
                    id, entry_id, entry_pos_id, is_regular, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(group.id)
            .bind(entry_id)
            .bind(pos.pos_id)
            .bind(group.is_regular)
            .bind(group_ordinal as i32)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        }
        for (form_ordinal, form) in pos.forms.iter().enumerate() {
            upsert_v3_node(
                tx,
                form.id,
                entry_id,
                "concrete_form",
                Some(pos.pos_id),
                "forms.concrete_form",
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_concrete_forms (
                    id, entry_id, entry_pos_id, form_type, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(form.id)
            .bind(entry_id)
            .bind(pos.pos_id)
            .bind(v3_form_type_name(&form.form_type))
            .bind(form_ordinal as i32)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "common",
                        common.id,
                        &common.spelling,
                        common.origin,
                        &common.pronunciations,
                        &common.component_usages,
                    )
                    .await?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "uk",
                        uk.id,
                        &uk.spelling,
                        uk.origin,
                        &uk.pronunciations,
                        &uk.component_usages,
                    )
                    .await?;
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "us",
                        us.id,
                        &us.spelling,
                        us.origin,
                        &us.pronunciations,
                        &us.component_usages,
                    )
                    .await?;
                }
            }
        }
        for group in &pos.form_groups {
            for (membership_ordinal, membership) in group.members.iter().enumerate() {
                upsert_v3_node(
                    tx,
                    membership.id,
                    entry_id,
                    "group_membership",
                    Some(group.id),
                    "forms.group_membership",
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_group_memberships (
                        id, entry_id, entry_pos_id, form_group_id, form_id, ordinal
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    "#,
                )
                .bind(membership.id)
                .bind(entry_id)
                .bind(pos.pos_id)
                .bind(group.id)
                .bind(membership.form_id)
                .bind(membership_ordinal as i32)
                .execute(&mut **tx)
                .await
                .map_err(database_error)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_variant(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    form_id: Uuid,
    dialect: &str,
    variant_id: Uuid,
    spelling: &str,
    origin: TextOrigin,
    pronunciations: &[crate::lexicon::dto::WordPronunciationV3],
    component_usages: &[PhraseComponentUsageV3],
) -> Result<(), LexiconServiceError> {
    upsert_v3_node(
        tx,
        variant_id,
        entry_id,
        "form_variant",
        Some(form_id),
        &format!("forms.form_variant:{dialect}"),
    )
    .await?;
    // Draft variants are stable identity shells, so `intent=save` may persist
    // an empty spelling. Empty shells deliberately have no surface projection;
    // `intent=complete` rejects them before this storage path.
    let normalized_spelling = if spelling.is_empty() {
        String::new()
    } else {
        crate::lexicon::normalization::normalize_headword(spelling)
            .map_err(|_| invariant_record())?
            .key
    };
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_form_variants (
            id, entry_id, form_id, dialect, spelling, normalized_spelling,
            normalization_version, origin
        ) VALUES ($1, $2, $3, $4, $5, $6, 1, $7)
        "#,
    )
    .bind(variant_id)
    .bind(entry_id)
    .bind(form_id)
    .bind(dialect)
    .bind(spelling)
    .bind(normalized_spelling)
    .bind(text_origin_name(origin))
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for (ordinal, component) in component_usages.iter().enumerate() {
        match component {
            PhraseComponentUsageV3::Unresolved { id, literal } => {
                upsert_v3_node(
                    tx,
                    *id,
                    entry_id,
                    "phrase_component_usage",
                    Some(variant_id),
                    "forms.phrase_component_usage",
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_phrase_variant_component_usages (
                        id, entry_id, form_variant_id, ordinal, state, literal
                    ) VALUES ($1, $2, $3, $4, 'unresolved', $5)
                    "#,
                )
                .bind(id)
                .bind(entry_id)
                .bind(variant_id)
                .bind(ordinal as i16)
                .bind(literal)
                .execute(&mut **tx)
                .await
                .map_err(database_error)?;
            }
            PhraseComponentUsageV3::Resolved {
                id,
                literal,
                target_word_id,
                target_publication_id,
                target_pos_id,
                target_base_form_id,
                target_sense_id,
                target_form_id,
                target_variant_id,
                target_dialect,
                target_form_type,
                target_headword,
                target_gloss,
            } => {
                upsert_v3_node(
                    tx,
                    *id,
                    entry_id,
                    "phrase_component_usage",
                    Some(variant_id),
                    "forms.phrase_component_usage",
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_phrase_variant_component_usages (
                        id, entry_id, form_variant_id, ordinal, state, literal,
                        target_entry_id, target_publication_id, target_pos_id,
                        target_base_form_id, target_sense_id, target_form_id,
                        target_variant_id, target_dialect, target_form_type,
                        target_headword_snapshot, target_gloss_snapshot
                    ) VALUES (
                        $1, $2, $3, $4, 'resolved', $5, $6, $7, $8, $9,
                        $10, $11, $12, $13, $14, $15, $16
                    )
                    "#,
                )
                .bind(id)
                .bind(entry_id)
                .bind(variant_id)
                .bind(ordinal as i16)
                .bind(literal)
                .bind(target_word_id)
                .bind(target_publication_id)
                .bind(target_pos_id)
                .bind(target_base_form_id)
                .bind(target_sense_id)
                .bind(target_form_id)
                .bind(target_variant_id)
                .bind(crate::lexicon::node_identity::dialect_name(*target_dialect))
                .bind(v3_form_type_name(target_form_type))
                .bind(target_headword)
                .bind(target_gloss)
                .execute(&mut **tx)
                .await
                .map_err(database_error)?;
            }
        }
    }
    for (ordinal, pronunciation) in pronunciations.iter().enumerate() {
        upsert_v3_node(
            tx,
            pronunciation.id,
            entry_id,
            "pronunciation",
            Some(variant_id),
            "forms.pronunciation",
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_pronunciations (
                id, entry_id, form_variant_id, dict_phonetic, actual_pron,
                normalized_dict_phonetic, normalized_actual_pron, style,
                normalization_version, ordinal
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, $9)
            "#,
        )
        .bind(pronunciation.id)
        .bind(entry_id)
        .bind(variant_id)
        .bind(&pronunciation.dict_phonetic)
        .bind(&pronunciation.actual_pron)
        .bind(normalize_v3_text(&pronunciation.dict_phonetic))
        .bind(normalize_v3_text(&pronunciation.actual_pron))
        .bind(pronunciation.style.map(pronunciation_style_name))
        .bind(ordinal as i32)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn upsert_v3_node(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
) -> Result<(), LexiconServiceError> {
    let stable_slot = matches!(
        node_role,
        "forms.form_variant:common" | "forms.form_variant:uk" | "forms.form_variant:us"
    );
    if stable_slot {
        let existing = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM lexicon.nodes
            WHERE entry_id = $1
              AND parent_node_id = $2
              AND node_role = $3
              AND stable_slot = TRUE
            "#,
        )
        .bind(entry_id)
        .bind(parent_node_id)
        .bind(node_role)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if existing.is_some_and(|existing_id| existing_id != id) {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    let result = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (id) DO UPDATE
        SET removed_from_draft_at = NULL
        WHERE lexicon.nodes.entry_id = EXCLUDED.entry_id
          AND lexicon.nodes.node_type = EXCLUDED.node_type
          AND lexicon.nodes.parent_node_id IS NOT DISTINCT FROM EXCLUDED.parent_node_id
          AND lexicon.nodes.node_role = EXCLUDED.node_role
          AND lexicon.nodes.stable_slot = EXCLUDED.stable_slot
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(LexiconServiceError::StableNodeIdChanged)
    }
}

async fn update_v3_step_progress<T: serde::Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    step: &str,
    revision: i64,
    content: &T,
    complete: bool,
) -> Result<(), LexiconServiceError> {
    if !complete {
        sqlx::query("DELETE FROM lexicon.entry_step_progress WHERE entry_id = $1 AND step = $2")
            .bind(entry_id)
            .bind(step)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_step_progress (
            entry_id, step, completed_revision, content_hash, completed_at
        ) VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (entry_id, step) DO UPDATE
        SET completed_revision = EXCLUDED.completed_revision,
            content_hash = EXCLUDED.content_hash,
            completed_at = EXCLUDED.completed_at
        "#,
    )
    .bind(entry_id)
    .bind(step)
    .bind(revision)
    .bind(sha256_json(content).map_err(serialization_error)?)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

fn v3_form_type_name(value: &str) -> &str {
    value
}

const fn text_origin_name(value: TextOrigin) -> &'static str {
    match value {
        TextOrigin::Dictionary => "dictionary",
        TextOrigin::Converted => "converted",
        TextOrigin::Manual => "manual",
    }
}

const fn pronunciation_style_name(value: PronunciationStyle) -> &'static str {
    match value {
        PronunciationStyle::Normal => "normal",
        PronunciationStyle::Strong => "strong",
        PronunciationStyle::Weak => "weak",
    }
}

fn normalize_v3_text(value: &str) -> String {
    value.nfkc().collect::<String>().trim().to_lowercase()
}

fn canonicalize_v3_forms(content: &mut DraftFormsStepContentV3) -> Result<(), LexiconServiceError> {
    for pos in &mut content.pos {
        for form in &mut pos.forms {
            match &mut form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    canonicalize_v3_spelling(&mut common.spelling)?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    canonicalize_v3_spelling(&mut uk.spelling)?;
                    canonicalize_v3_spelling(&mut us.spelling)?;
                }
            }
        }
    }
    strip_forms_phonetic_liaisons(content);
    Ok(())
}

/// 连读只标在实际发音上。字典音标里的历史连读在保存和发布时一并落掉，不做搬迁：
/// 两个框的文本常常不一样，错位搬过去比重标更糟。
fn strip_phonetic_liaisons(pronunciations: &mut [WordPronunciationV3]) {
    for pronunciation in pronunciations {
        if let Some(rich) = &mut pronunciation.dict_phonetic_rich {
            rich.strip_liaisons();
        }
    }
}

/// 发布不走 canonicalize，但快照是不可变的：连读要在冻进去之前落掉。
pub(crate) fn strip_forms_phonetic_liaisons(content: &mut DraftFormsStepContentV3) {
    for pos in &mut content.pos {
        for form in &mut pos.forms {
            match &mut form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    strip_phonetic_liaisons(&mut common.pronunciations);
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    strip_phonetic_liaisons(&mut uk.pronunciations);
                    strip_phonetic_liaisons(&mut us.pronunciations);
                }
            }
        }
    }
}

fn canonicalize_v3_spelling(spelling: &mut String) -> Result<(), LexiconServiceError> {
    if spelling.trim().is_empty() {
        spelling.clear();
        return Ok(());
    }
    let normalized = crate::lexicon::normalization::normalize_headword(spelling).map_err(|_| {
        LexiconServiceError::UnprocessableField {
            field: "spelling",
            message: "spelling must contain between 1 and 200 valid codepoints",
        }
    })?;
    *spelling = normalized.display;
    Ok(())
}

async fn replace_v3_surface_projection(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    entry_kind: WordEntryKindV3,
    revision: i64,
    forms: &DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let sources =
        crate::lexicon::v3_projection::form_variant_sources(entry_id, forms).map_err(|_| {
            LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                "validated V3 forms could not be projected",
            ))
        })?;
    let event_offset = sqlx::query_scalar::<_, i64>(
        "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET source_revision = $2,
            event_offset = $3,
            is_deleted = TRUE,
            updated_at = now()
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND content_scope = 'draft'
          AND (source_revision, event_offset) <= ($2, $3)
        "#,
    )
    .bind(entry_id)
    .bind(revision)
    .bind(event_offset)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    let source_count = sources.len();
    for source in sources {
        sqlx::query(
            r#"
            INSERT INTO lexicon.surface_sources (
                entry_id, source_id, source_kind, source_node_id,
                language, entry_kind, dialect, dialect_scope,
                surface, normalized_surface, normalization_version,
                source_revision, event_offset, is_deleted, content_scope, publication_id,
                pos_id, pos, form_type, content_schema_version,
                form_id, variant_id, group_ids, projection_version, updated_at
            ) VALUES (
                $1, $2, 'form_variant', $3,
                'en', $4, $5, $6,
                $7, $8, $9,
                $10, $11, FALSE, 'draft', NULL,
                $12, $13, $14, 3,
                $15, $16, $17, $18, now()
            )
            ON CONFLICT (source_id, content_scope, dialect_scope, normalization_version)
            DO UPDATE SET
                entry_id = EXCLUDED.entry_id,
                source_kind = EXCLUDED.source_kind,
                source_node_id = EXCLUDED.source_node_id,
                language = EXCLUDED.language,
                entry_kind = EXCLUDED.entry_kind,
                dialect = EXCLUDED.dialect,
                surface = EXCLUDED.surface,
                normalized_surface = EXCLUDED.normalized_surface,
                source_revision = EXCLUDED.source_revision,
                event_offset = EXCLUDED.event_offset,
                is_deleted = FALSE,
                publication_id = NULL,
                pos_id = EXCLUDED.pos_id,
                pos = EXCLUDED.pos,
                form_type = EXCLUDED.form_type,
                content_schema_version = 3,
                form_id = EXCLUDED.form_id,
                variant_id = EXCLUDED.variant_id,
                group_ids = EXCLUDED.group_ids,
                projection_version = EXCLUDED.projection_version,
                updated_at = now()
            "#,
        )
        .bind(source.entry_id)
        .bind(source.source_id)
        .bind(source.variant_id)
        .bind(v3_kind_string(entry_kind))
        .bind(source.dialect.as_str())
        .bind(source.dialect_scope.as_str())
        .bind(source.surface)
        .bind(source.normalized_surface)
        .bind(source.normalization_version)
        .bind(revision)
        .bind(event_offset)
        .bind(source.pos_id)
        .bind(source.pos)
        .bind(v3_form_type_name(&source.form_type))
        .bind(source.form_id)
        .bind(source.variant_id)
        .bind(source.group_ids)
        .bind(source.projection_version)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.surface_projection', $2, $3,
            'lexicon.surface_projection_replaced', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(event_offset)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "content_schema_version": 3,
        "content_scope": "draft",
        "publication_id": Option::<Uuid>::None,
        "source_revision": revision,
        "event_offset": event_offset,
        "source_count": source_count,
    }))
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn upsert_presentation(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    revision: i64,
    presentation: &EntryPresentationV3,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_presentation_projection (
            entry_id, content_schema_version, label, matched_surfaces, strategy_version,
            source_revision, updated_at
        ) VALUES ($1, 3, $2, $3, $4, $5, now())
        ON CONFLICT (entry_id) DO UPDATE
        SET content_schema_version = EXCLUDED.content_schema_version,
            label = EXCLUDED.label,
            matched_surfaces = EXCLUDED.matched_surfaces,
            strategy_version = EXCLUDED.strategy_version,
            source_revision = EXCLUDED.source_revision,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(entry_id)
    .bind(&presentation.label)
    .bind(&presentation.matched_surfaces)
    .bind(&presentation.strategy_version)
    .bind(revision)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    resource_id: Uuid,
    response_status: i16,
    response_body: Value,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO platform.idempotency_records (
            scope, idempotency_key, actor_id, request_hash, resource_id,
            response_status, response_body, expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '24 hours')
        "#,
    )
    .bind(scope)
    .bind(idempotency_key)
    .bind(actor_id)
    .bind(request_hash)
    .bind(resource_id)
    .bind(response_status)
    .bind(response_body)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    request_id: Uuid,
    action: &str,
    resource_id: Uuid,
    revision: i64,
    metadata: Value,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        )
        SELECT $1, $2, $3, 'lexicon.entry', $4, $5, $6, $7
        WHERE NOT EXISTS (
            SELECT 1
            FROM audit.admin_actions
            WHERE actor_admin_id = $2
              AND action = $3
              AND resource_type = 'lexicon.entry'
              AND resource_id = $4
              AND request_id = $6
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(resource_id)
    .bind(revision)
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use sqlx::PgPool;
    use tokio::sync::oneshot;

    use super::*;
    use crate::lexicon::dto::{
        CommonDialectV3, DialectRulesV3, WordCommonFormVariantV3, WordConcreteFormV3,
        WordFormGroupMemberV3, WordFormGroupV3, WordPosFormsV3, WordPronunciationV3,
    };

    fn fixed_id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn detection_basis_uses_the_original_surface_instead_of_the_confirmed_source_side() {
        let headwords = WordHeadwordsV2::Distinguish {
            uk: "centre".to_owned(),
            us: "center".to_owned(),
            source_dialect: SourceDialect::Uk,
        };

        assert_eq!(
            detection_basis_dialect_for_headwords("center", &headwords).unwrap(),
            Some(SourceDialect::Us)
        );
        assert_eq!(
            detection_basis_dialect_for_headwords("centre", &headwords).unwrap(),
            Some(SourceDialect::Uk)
        );
        assert_eq!(
            detection_basis_dialect_for_headwords("edited", &headwords).unwrap(),
            None
        );
    }

    #[test]
    fn detection_basis_from_v2_snapshot_prefers_the_original_surface_over_the_dictionary_side() {
        // 词典以英式主条命中，但管理员当初输入的是美式拼写：基准要跟着 surface 走，
        // 不能被 matched_dialect 这个词典主条方言带偏。
        let snapshot = json!({
            "matched_dialect": "uk",
            "normalized_headword": "center",
            "headwords": {
                "mode": "distinguish",
                "uk": "centre",
                "us": "center",
                "source_dialect": "uk"
            }
        });
        assert_eq!(
            stored_detection_basis_dialect("migrated_v2", &snapshot, None),
            Some(SourceDialect::Us)
        );
    }

    #[test]
    fn detection_basis_from_v2_snapshot_falls_back_to_matched_dialect_only_without_materials() {
        // 迁移词条的快照可能只剩词典命中方言，这时才允许用这个近似值。
        assert_eq!(
            stored_detection_basis_dialect(
                "migrated_v2",
                &json!({ "matched_dialect": "uk" }),
                None
            ),
            Some(SourceDialect::Uk)
        );
        assert_eq!(
            stored_detection_basis_dialect(
                "migrated_v2",
                &json!({ "matched_dialect": "us" }),
                None
            ),
            Some(SourceDialect::Us)
        );
        // common 命中不指向任何一侧。
        assert_eq!(
            stored_detection_basis_dialect(
                "migrated_v2",
                &json!({ "matched_dialect": "common" }),
                None
            ),
            None
        );
        // 比对材料齐全却没有唯一命中侧，是明确结论，不该被近似值盖掉。
        let same_spelling = json!({
            "matched_dialect": "uk",
            "normalized_headword": "sport",
            "headwords": {
                "mode": "distinguish",
                "uk": "sport",
                "us": "sport",
                "source_dialect": "uk"
            }
        });
        assert_eq!(
            stored_detection_basis_dialect("migrated_v2", &same_spelling, None),
            None
        );
    }

    #[test]
    fn detection_basis_from_v3_snapshot_compares_the_original_surface() {
        let initial = json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk"
        });
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "normalized_surface": "center" }),
                Some(&initial)
            ),
            Some(SourceDialect::Us)
        );
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "normalized_surface": "centre" }),
                Some(&initial)
            ),
            Some(SourceDialect::Uk)
        );
        // 两侧都没命中（拼写后来被改过）就没有基准可言。
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "normalized_surface": "edited" }),
                Some(&initial)
            ),
            None
        );
    }

    #[test]
    fn detection_basis_dispatches_on_entry_origin_not_on_snapshot_keys() {
        let initial = json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk"
        });
        // 原生词条即便将来快照里也多出 matched_dialect，仍按 surface 精确比对，
        // 不会静默降级成 v2 的近似口径。
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "matched_dialect": "uk", "normalized_surface": "center" }),
                Some(&initial)
            ),
            Some(SourceDialect::Us)
        );
    }

    #[test]
    fn detection_basis_degrades_instead_of_failing_the_entry_read() {
        let initial = json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk"
        });
        // v3 路径缺初始主词：算不出来，但不该报错。
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "normalized_surface": "center" }),
                None
            ),
            None
        );
        // 两种形状的线索都没有。
        assert_eq!(
            stored_detection_basis_dialect("native", &json!({}), Some(&initial)),
            None
        );
        assert_eq!(
            stored_detection_basis_dialect("migrated_v2", &json!({}), Some(&initial)),
            None
        );
        // 存量主词结构漂移（WordHeadwordsV2 带 deny_unknown_fields）：这个字段只是展示
        // 信息，解析不了就不显示，绝不能把整条词条读挂。
        let drifted = json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk",
            "legacy_extra": true
        });
        assert_eq!(
            stored_detection_basis_dialect(
                "native",
                &json!({ "normalized_surface": "center" }),
                Some(&drifted)
            ),
            None
        );
        // v2 快照里的主词漂移时退回近似值，仍然不报错。
        assert_eq!(
            stored_detection_basis_dialect(
                "migrated_v2",
                &json!({
                    "matched_dialect": "uk",
                    "normalized_headword": "center",
                    "headwords": drifted
                }),
                None
            ),
            Some(SourceDialect::Uk)
        );
    }

    #[test]
    fn detection_basis_needs_a_single_side_to_win() {
        // 英美同形：两侧都命中，谁也不算基准。
        let same_spelling = WordHeadwordsV2::Distinguish {
            uk: "sport".to_owned(),
            us: "sport".to_owned(),
            source_dialect: SourceDialect::Uk,
        };
        assert_eq!(
            detection_basis_dialect_for_headwords("sport", &same_spelling).unwrap(),
            None
        );
        // unified 主词没有英美之分。
        let unified = WordHeadwordsV2::Unified {
            common: "sport".to_owned(),
        };
        assert_eq!(
            detection_basis_dialect_for_headwords("sport", &unified).unwrap(),
            None
        );
    }

    fn common_form(form_id: Uuid, variant_id: Uuid, pronunciation_id: Uuid) -> WordConcreteFormV3 {
        WordConcreteFormV3 {
            id: form_id,
            form_type: "base".to_owned(),
            regional_variants: WordRegionalVariantsV3::Common {
                common: WordCommonFormVariantV3 {
                    id: variant_id,
                    dialect: CommonDialectV3::Common,
                    spelling: format!("form-{form_id}"),
                    origin: TextOrigin::Manual,
                    pronunciations: vec![WordPronunciationV3 {
                        dict_phonetic_rich: None,
                        actual_pron_rich: None,
                        voice_profile: None,
                        audio_assets: Vec::new(),
                        id: pronunciation_id,
                        dict_phonetic: "test".to_owned(),
                        actual_pron: "test".to_owned(),
                        style: Some(PronunciationStyle::Normal),
                    }],
                    component_usages: Vec::new().into(),
                },
            },
        }
    }

    #[test]
    fn component_usages_are_phrase_only() {
        let mut form = common_form(fixed_id(1), fixed_id(2), fixed_id(3));
        let WordRegionalVariantsV3::Common { common } = &mut form.regional_variants else {
            unreachable!();
        };
        common
            .component_usages
            .push(PhraseComponentUsageV3::Unresolved {
                id: fixed_id(4),
                literal: "component".to_owned(),
            });
        let content = DraftFormsStepContentV3 {
            pos: vec![WordPosFormsV3 {
                pos_id: fixed_id(5),
                pos: "phrase".to_owned(),
                dialect_rules: DialectRulesV3::UNIFIED,
                forms: vec![form],
                form_groups: Vec::new(),
            }],
        };
        assert!(ensure_phrase_component_ownership(WordEntryKindV3::Phrase, &content).is_ok());
        assert!(ensure_phrase_component_ownership(WordEntryKindV3::Word, &content).is_err());
    }

    fn sense_meanings(component_usages: Option<Value>) -> DraftMeaningsStepContentV3 {
        let mut sense = json!({
            "id": fixed_id(700),
            "sub_pos": "N-COUNT",
            "level": "A1",
            "sense_group_id": fixed_id(701),
            "frequency": "100",
            "depends_on_context": false,
            "definitions": [],
            "sentences": [],
            "relations": []
        });
        if let Some(component_usages) = component_usages {
            sense["component_usages"] = component_usages;
        }
        serde_json::from_value(json!({
            "sense_groups": [{
                "id": fixed_id(701),
                "name_zh": "核心义",
                "name_en": "core"
            }],
            "pos": [{
                "pos_id": fixed_id(702),
                "grammar_structures": [],
                "senses": [sense]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn missing_sense_component_field_preserves_stock_while_explicit_empty_clears() {
        let stock = json!([{
            "id": fixed_id(703),
            "state": "unresolved",
            "literal": "component"
        }]);
        let current = sense_meanings(Some(stock));

        let mut absent = sense_meanings(None);
        preserve_missing_sense_component_usages(&mut absent, &current);
        assert_eq!(
            absent.pos[0].senses[0].component_usages.len(),
            1,
            "缺键的旧客户端不得清空存量成分"
        );

        let mut cleared = sense_meanings(Some(json!([])));
        preserve_missing_sense_component_usages(&mut cleared, &current);
        assert!(
            cleared.pos[0].senses[0].component_usages.is_empty(),
            "显式空数组必须清空"
        );

        // V2 往返把字段整个吞掉，落库前只能靠往返前的内容回填。
        let mut round_tripped = sense_meanings(None);
        restore_sense_component_usages(&current, &mut round_tripped);
        assert_eq!(round_tripped.pos[0].senses[0].component_usages.len(), 1);
    }

    #[test]
    fn missing_component_field_preserves_stable_variant_but_rejects_dialect_change_or_deletion() {
        let mut current = two_form_content(false);
        let WordRegionalVariantsV3::Common { common } =
            &mut current.pos[0].forms[0].regional_variants
        else {
            panic!("fixture must use common variant");
        };
        let component_id = Uuid::now_v7();
        common
            .component_usages
            .push(PhraseComponentUsageV3::Unresolved {
                id: component_id,
                literal: "component".to_owned(),
            });

        let mut stable_json = serde_json::to_value(&current).unwrap();
        stable_json["pos"][0]["forms"][0]["regional_variants"]["common"]
            .as_object_mut()
            .unwrap()
            .remove("component_usages");
        let mut stable: DraftFormsStepContentV3 = serde_json::from_value(stable_json).unwrap();
        preserve_missing_component_usages(&mut stable, &current).unwrap();
        let WordRegionalVariantsV3::Common { common } = &stable.pos[0].forms[0].regional_variants
        else {
            panic!("stable fixture must remain common");
        };
        assert_eq!(
            common.component_usages[0],
            PhraseComponentUsageV3::Unresolved {
                id: component_id,
                literal: "component".to_owned(),
            }
        );

        let current_common = serde_json::to_value(&current.pos[0].forms[0].regional_variants)
            .unwrap()["common"]
            .clone();
        let mut uk = current_common.clone();
        uk["dialect"] = json!("uk");
        uk.as_object_mut().unwrap().remove("component_usages");
        let mut us = current_common;
        us["id"] = json!(Uuid::now_v7());
        us["dialect"] = json!("us");
        us.as_object_mut().unwrap().remove("component_usages");
        let mut dialect_changed = current.clone();
        dialect_changed.pos[0].forms[0].regional_variants = serde_json::from_value(json!({
            "mode": "uk_us",
            "uk": uk,
            "us": us
        }))
        .unwrap();
        assert!(matches!(
            preserve_missing_component_usages(&mut dialect_changed, &current),
            Err(LexiconServiceError::ReferenceConflict)
        ));

        let mut deleted = current.clone();
        deleted.pos[0].forms.remove(0);
        assert!(matches!(
            preserve_missing_component_usages(&mut deleted, &current),
            Err(LexiconServiceError::ReferenceConflict)
        ));
    }

    fn two_form_content(reverse: bool) -> DraftFormsStepContentV3 {
        let first_form = common_form(fixed_id(400), fixed_id(401), fixed_id(402));
        let second_form = common_form(fixed_id(500), fixed_id(501), fixed_id(502));
        let first_group = WordFormGroupV3 {
            id: fixed_id(200),
            is_regular: true,
            members: vec![WordFormGroupMemberV3 {
                id: fixed_id(201),
                form_id: first_form.id,
            }],
        };
        let second_group = WordFormGroupV3 {
            id: fixed_id(300),
            is_regular: false,
            members: vec![WordFormGroupMemberV3 {
                id: fixed_id(301),
                form_id: second_form.id,
            }],
        };
        let (forms, form_groups) = if reverse {
            (
                vec![second_form, first_form],
                vec![second_group, first_group],
            )
        } else {
            (
                vec![first_form, second_form],
                vec![first_group, second_group],
            )
        };
        DraftFormsStepContentV3 {
            pos: vec![WordPosFormsV3 {
                pos_id: fixed_id(100),
                pos: "noun".to_owned(),
                dialect_rules: DialectRulesV3::UNIFIED,
                forms,
                form_groups,
            }],
        }
    }

    async fn seed_admin(pool: &PgPool) -> Uuid {
        let admin_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'V3 node test')",
        )
        .bind(admin_id)
        .bind(format!("v3-node-{}", admin_id.simple()))
        .execute(pool)
        .await
        .unwrap();
        admin_id
    }

    async fn seed_v3_entry(
        pool: &PgPool,
        admin_id: Uuid,
        migration_batch_id: Option<Uuid>,
    ) -> Uuid {
        let entry_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, source_dialect, detection_snapshot,
                created_by_admin_id, updated_by_admin_id
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, NULL, '{}', $2, $2)
            "#,
        )
        .bind(entry_id)
        .bind(admin_id)
        .execute(pool)
        .await
        .unwrap();
        if let Some(batch_id) = migration_batch_id {
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_entry_state (
                    entry_id, origin, migration_batch_id, source_revision
                ) VALUES ($1, 'migrated_v2', $2, 1)
                "#,
            )
            .bind(entry_id)
            .bind(batch_id)
            .execute(pool)
            .await
            .unwrap();
        } else {
            sqlx::query(
                "INSERT INTO lexicon.v3_entry_state (entry_id, origin) VALUES ($1, 'native')",
            )
            .bind(entry_id)
            .execute(pool)
            .await
            .unwrap();
        }
        entry_id
    }

    #[test]
    fn audit_delta_is_sorted_and_separates_new_retained_and_retired_ids() {
        let entry_id = fixed_id(1);
        let other_entry_id = fixed_id(2);
        let current = vec![fixed_id(30), fixed_id(10), fixed_id(20)];
        let proposed = vec![fixed_id(40), fixed_id(30), fixed_id(20)];
        let existing = vec![
            NodeIdentityRecord {
                id: fixed_id(30),
                entry_id,
                node_type: "concrete_form".to_owned(),
                parent_node_id: Some(fixed_id(100)),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            },
            NodeIdentityRecord {
                id: fixed_id(20),
                entry_id,
                node_type: "form_group".to_owned(),
                parent_node_id: Some(fixed_id(100)),
                node_role: "forms.form_group".to_owned(),
                stable_slot: false,
            },
            NodeIdentityRecord {
                id: fixed_id(40),
                entry_id: other_entry_id,
                node_type: "concrete_form".to_owned(),
                parent_node_id: Some(fixed_id(101)),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            },
        ];

        let delta = v3_audit_node_delta(entry_id, &current, &proposed, &existing);

        assert_eq!(delta.generated_node_ids, vec![fixed_id(40)]);
        assert_eq!(delta.changed_node_ids, vec![fixed_id(20), fixed_id(30)]);
        assert_eq!(delta.retired_node_ids, vec![fixed_id(10)]);
        let metadata = v3_save_audit_metadata(StepSaveIntent::Complete, Some(fixed_id(9)), delta);
        assert_eq!(metadata["schema_version"], 3);
        assert_eq!(metadata["migration_batch_id"], fixed_id(9).to_string());
        assert_eq!(metadata["intent"], "complete");
    }

    #[sqlx::test]
    async fn retired_v3_variant_slot_rejects_a_replacement_uuid_before_writes(pool: PgPool) {
        let admin_id = seed_admin(&pool).await;
        let entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let proposed_content = two_form_content(false);
        let proposed = v3_form_proposed_nodes(&proposed_content);
        let mut tx = pool.begin().await.unwrap();
        for node in proposed
            .iter()
            .filter(|node| node.node_type != "form_variant" && node.node_type != "pronunciation")
        {
            upsert_v3_node(
                &mut tx,
                node.id,
                entry_id,
                node.node_type,
                node.parent_node_id,
                &node.node_role,
            )
            .await
            .unwrap();
        }
        let retired_variant_id = fixed_id(999);
        upsert_v3_node(
            &mut tx,
            retired_variant_id,
            entry_id,
            "form_variant",
            Some(fixed_id(400)),
            "forms.form_variant:common",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE lexicon.nodes SET removed_from_draft_at = now() WHERE id = $1")
            .bind(retired_variant_id)
            .execute(&mut *tx)
            .await
            .unwrap();

        let error = preflight_v3_form_node_identities(
            &mut tx,
            entry_id,
            &DraftFormsStepContentV3::default(),
            &proposed_content,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, LexiconServiceError::StableNodeIdChanged));
        let proposed_variant_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lexicon.nodes WHERE id = $1)")
                .bind(fixed_id(401))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(!proposed_variant_exists, "preflight 冲突不得先写入新节点");
        tx.rollback().await.unwrap();
    }

    #[sqlx::test]
    async fn reverse_order_cross_entry_node_reuse_waits_then_returns_stable_validation(
        pool: PgPool,
    ) {
        let admin_id = seed_admin(&pool).await;
        let first_entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let second_entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let first_content = two_form_content(false);
        let second_content = two_form_content(true);
        let mut first_tx = pool.begin().await.unwrap();
        preflight_v3_form_node_identities(
            &mut first_tx,
            first_entry_id,
            &DraftFormsStepContentV3::default(),
            &first_content,
        )
        .await
        .unwrap();

        let second_pool = pool.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let mut second = tokio::spawn(async move {
            let mut tx = second_pool.begin().await.unwrap();
            started_tx.send(()).unwrap();
            let result = preflight_v3_form_node_identities(
                &mut tx,
                second_entry_id,
                &DraftFormsStepContentV3::default(),
                &second_content,
            )
            .await;
            tx.rollback().await.unwrap();
            result
        });
        started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "第二个逆序提交应等待第一个事务持有的排序 advisory locks"
        );

        for node in v3_form_proposed_nodes(&first_content) {
            upsert_v3_node(
                &mut first_tx,
                node.id,
                first_entry_id,
                node.node_type,
                node.parent_node_id,
                &node.node_role,
            )
            .await
            .unwrap();
        }
        first_tx.commit().await.unwrap();

        let error = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("排序锁释放后不得死锁")
            .unwrap()
            .unwrap_err();
        let LexiconServiceError::ValidationFailedV3(issues) = error else {
            panic!("跨词条 UUID 应返回稳定 V3 validation，而不是数据库错误: {error:?}");
        };
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == V3ValidationIssueCode::NodeIdReused)
        );
        let second_nodes: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lexicon.nodes WHERE entry_id = $1")
                .bind(second_entry_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(second_nodes, 0, "冲突事务不得留下部分节点");
    }

    #[sqlx::test]
    async fn save_audit_includes_migration_batch_and_retry_is_action_idempotent(pool: PgPool) {
        let admin_id = seed_admin(&pool).await;
        let batch_id = Uuid::now_v7();
        let entry_id = seed_v3_entry(&pool, admin_id, Some(batch_id)).await;
        let request_id = Uuid::now_v7();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            v3_migration_batch_id(&mut tx, entry_id).await.unwrap(),
            Some(batch_id)
        );
        let metadata = v3_save_audit_metadata(
            StepSaveIntent::Save,
            Some(batch_id),
            V3AuditNodeDelta {
                generated_node_ids: vec![fixed_id(10)],
                changed_node_ids: vec![fixed_id(20)],
                retired_node_ids: vec![fixed_id(30)],
            },
        );
        for action in [
            "lexicon.entry.forms.save.v3",
            "lexicon.entry.meanings.save.v3",
        ] {
            insert_v3_audit(
                &mut tx,
                admin_id,
                request_id,
                action,
                entry_id,
                2,
                metadata.clone(),
            )
            .await
            .unwrap();
            insert_v3_audit(
                &mut tx,
                admin_id,
                request_id,
                action,
                entry_id,
                2,
                metadata.clone(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let rows: Vec<(String, Value)> = sqlx::query_as(
            r#"
            SELECT action, metadata
            FROM audit.admin_actions
            WHERE resource_id = $1 AND request_id = $2
            ORDER BY action
            "#,
        )
        .bind(entry_id)
        .bind(request_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2, "同一 action 的重试不得重复审计");
        assert_eq!(rows[0].1["migration_batch_id"], batch_id.to_string());
        assert_eq!(rows[0].1["generated_node_ids"][0], fixed_id(10).to_string());
        assert_eq!(rows[0].1["changed_node_ids"][0], fixed_id(20).to_string());
        assert_eq!(rows[0].1["retired_node_ids"][0], fixed_id(30).to_string());
    }
}

/// V2 中间结构不含语义区间语音字段，按稳定 id 恢复 V3 扩展。
pub(super) fn restore_sense_group_voice(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) {
    let by_id: HashMap<_, _> = source
        .sense_groups
        .iter()
        .map(|group| (group.id, group))
        .collect();
    for group in &mut target.sense_groups {
        if let Some(original) = by_id.get(&group.id) {
            group.name_en_rich = original.name_en_rich.clone();
            group.voice_profile = original.voice_profile.clone();
        }
    }
}

#[cfg(test)]
mod sense_group_voice_tests {
    use super::*;
    #[test]
    fn sense_group_voice_survives_v2_bridge_and_legacy_omission() {
        let legacy = serde_json::json!({"sense_groups":[{"id":Uuid::new_v4(),"name_zh":"工作","name_en":"work"}],"pos":[]});
        let old: DraftMeaningsStepContentV3 = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&old).unwrap(), legacy);
        let mut raw = legacy;
        raw["sense_groups"][0]["name_en_rich"] =
            serde_json::json!({"version":2,"text":"work","annotations":[]});
        raw["sense_groups"][0]["voice_profile"] =
            serde_json::json!({"voices":[{"voice_id":"sonia","enabled":true,"rate_percent":10}]});
        let source: DraftMeaningsStepContentV3 = serde_json::from_value(raw.clone()).unwrap();
        let bridge: DraftMeaningsStepContent = serde_json::from_value(raw.clone()).unwrap();
        let mut target: DraftMeaningsStepContentV3 =
            serde_json::from_value(serde_json::to_value(bridge).unwrap()).unwrap();
        restore_sense_group_voice(&source, &mut target);
        assert_eq!(serde_json::to_value(&target).unwrap(), raw);
        restore_sense_group_voice(&old, &mut target);
        assert_eq!(
            serde_json::to_value(&target).unwrap(),
            serde_json::to_value(old).unwrap()
        );
    }
}
