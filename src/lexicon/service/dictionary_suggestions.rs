use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::lexicon::{
    dto::{
        CommonDialectV3, DictionaryPronunciationEvidenceV3, PronunciationStyle,
        SuggestedCommonFormVariantV3, SuggestedConcreteFormV3, SuggestedRegionalVariantsV3,
        SuggestedUkFormVariantV3, SuggestedUsFormVariantV3, UkDialectV3, UsDialectV3,
        WordFormTypeV3,
    },
    model::DictionaryContentRecord,
};

use super::helpers::map_dictionary_pos;

#[derive(Debug)]
pub(super) struct DictionarySuggestionResult {
    pub forms: Vec<SuggestedConcreteFormV3>,
    pub has_form_evidence: bool,
    pub has_pronunciations: bool,
}

pub(super) fn build_dictionary_suggestions(
    headword: &str,
    suggested_pos: &[String],
    records: &[DictionaryContentRecord],
) -> DictionarySuggestionResult {
    let mut forms = suggested_pos
        .iter()
        .map(|pos| {
            let common = pronunciations(records, pos, Some(headword), SourceDialect::Common, true);
            let uk = pronunciations(records, pos, Some(headword), SourceDialect::Uk, true);
            let us = pronunciations(records, pos, Some(headword), SourceDialect::Us, true);
            if !uk.is_empty() && !us.is_empty() {
                uk_us_form(pos, WordFormTypeV3::Base, headword, headword, uk, us)
            } else {
                common_form(pos, WordFormTypeV3::Base, headword, common)
            }
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut regional = BTreeMap::<(usize, u8), RegionalForms>::new();

    for record in records {
        let Some(pos) = map_dictionary_pos(std::slice::from_ref(&record.pos))
            .into_iter()
            .next()
        else {
            continue;
        };
        let Some(pos_index) = suggested_pos.iter().position(|candidate| candidate == &pos) else {
            continue;
        };
        let Some(raw_forms) = record.forms.as_array() else {
            continue;
        };
        for raw in raw_forms {
            let Some(spelling) = raw
                .get("form")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
            else {
                continue;
            };
            if spelling.is_empty() {
                continue;
            }
            let tags = string_tags(raw);
            if tags.iter().any(|tag| disallowed_form_tag(tag)) {
                continue;
            }
            let dialect = source_dialect(&tags);
            if dialect == SourceDialect::Conflict {
                continue;
            }
            for form_type in mapped_form_types(&tags) {
                let rank = form_type_rank(form_type);
                match dialect {
                    SourceDialect::Common => {
                        let key = (pos.clone(), rank, spelling.to_owned(), "common");
                        if seen.insert(key) {
                            forms.push(common_form(
                                &pos,
                                form_type,
                                spelling,
                                pronunciations(
                                    records,
                                    &pos,
                                    Some(spelling),
                                    SourceDialect::Common,
                                    false,
                                ),
                            ));
                        }
                    }
                    SourceDialect::Uk | SourceDialect::Us => {
                        let entry =
                            regional
                                .entry((pos_index, rank))
                                .or_insert_with(|| RegionalForms {
                                    pos: pos.clone(),
                                    form_type,
                                    uk: BTreeSet::new(),
                                    us: BTreeSet::new(),
                                });
                        if dialect == SourceDialect::Uk {
                            entry.uk.insert(spelling.to_owned());
                        } else {
                            entry.us.insert(spelling.to_owned());
                        }
                    }
                    SourceDialect::Conflict => {}
                }
            }
        }
    }

    for entry in regional.into_values() {
        for (uk, us) in entry.uk.into_iter().zip(entry.us) {
            forms.push(uk_us_form(
                &entry.pos,
                entry.form_type,
                &uk,
                &us,
                pronunciations(records, &entry.pos, Some(&uk), SourceDialect::Uk, false),
                pronunciations(records, &entry.pos, Some(&us), SourceDialect::Us, false),
            ));
        }
    }

    forms.sort_by(|left, right| {
        suggestion_sort_key(left, suggested_pos).cmp(&suggestion_sort_key(right, suggested_pos))
    });
    let has_form_evidence = forms.len() > suggested_pos.len();
    let has_pronunciations = forms.iter().any(suggestion_has_pronunciations);
    DictionarySuggestionResult {
        forms,
        has_form_evidence,
        has_pronunciations,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceDialect {
    Common,
    Uk,
    Us,
    Conflict,
}

struct RegionalForms {
    pos: String,
    form_type: WordFormTypeV3,
    uk: BTreeSet<String>,
    us: BTreeSet<String>,
}

fn string_tags(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|tag| tag.trim().to_ascii_lowercase())
        .collect()
}

fn source_dialect(tags: &BTreeSet<String>) -> SourceDialect {
    let uk = tags
        .iter()
        .any(|tag| matches!(tag.as_str(), "uk" | "british" | "received-pronunciation"));
    let us = tags
        .iter()
        .any(|tag| matches!(tag.as_str(), "us" | "american" | "general-american"));
    match (uk, us) {
        (false, false) if tags.iter().any(|tag| unsupported_region_tag(tag)) => {
            SourceDialect::Conflict
        }
        (false, false) => SourceDialect::Common,
        (true, false) => SourceDialect::Uk,
        (false, true) => SourceDialect::Us,
        (true, true) => SourceDialect::Common,
    }
}

fn unsupported_region_tag(tag: &str) -> bool {
    matches!(
        tag,
        "australia"
            | "canada"
            | "commonwealth"
            | "ireland"
            | "new-zealand"
            | "northern-england"
            | "northern-ireland"
            | "scotland"
            | "south-asia"
            | "southern"
            | "southern-england"
            | "southern-us"
            | "wales"
    )
}

fn disallowed_form_tag(tag: &str) -> bool {
    matches!(
        tag,
        "alternative"
            | "archaic"
            | "colloquial"
            | "dialectal"
            | "historical"
            | "nonstandard"
            | "obsolete"
            | "pronunciation-spelling"
            | "proscribed"
            | "rare"
    )
}

fn mapped_form_types(tags: &BTreeSet<String>) -> Vec<WordFormTypeV3> {
    let mut output = Vec::new();
    let has = |tag: &str| tags.contains(tag);
    if has("present") && has("singular") && has("third-person") {
        output.push(WordFormTypeV3::ThirdPersonSingular);
    }
    if has("present") && has("participle") {
        output.push(WordFormTypeV3::PresentParticiple);
    }
    if has("past") && !has("participle") {
        output.push(WordFormTypeV3::PastTense);
    }
    if has("past") && has("participle") {
        output.push(WordFormTypeV3::PastParticiple);
    }
    if has("plural") {
        output.push(WordFormTypeV3::Plural);
    }
    if has("comparative") {
        output.push(WordFormTypeV3::Comparative);
    }
    if has("superlative") {
        output.push(WordFormTypeV3::Superlative);
    }
    output
}

fn pronunciations(
    records: &[DictionaryContentRecord],
    pos: &str,
    form: Option<&str>,
    dialect: SourceDialect,
    include_unscoped: bool,
) -> Vec<DictionaryPronunciationEvidenceV3> {
    let mut values = BTreeSet::new();
    for record in records {
        if map_dictionary_pos(std::slice::from_ref(&record.pos)).first() != Some(&pos.to_owned()) {
            continue;
        }
        let Some(sounds) = record.sounds.as_array() else {
            continue;
        };
        for sound in sounds {
            if source_dialect(&string_tags(sound)) != dialect {
                continue;
            }
            let sound_form = sound
                .get("form")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match sound_form {
                Some(sound_form) if Some(sound_form) != form => continue,
                None if !include_unscoped => continue,
                Some(_) | None => {}
            }
            let Some(ipa) = sound
                .get("ipa")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            values.insert(ipa.to_owned());
        }
    }
    values
        .into_iter()
        .map(|dict_phonetic| DictionaryPronunciationEvidenceV3 {
            dict_phonetic,
            actual_pron: None,
            style: Some(PronunciationStyle::Normal),
        })
        .collect()
}

fn common_form(
    pos: &str,
    form_type: WordFormTypeV3,
    spelling: &str,
    pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
) -> SuggestedConcreteFormV3 {
    SuggestedConcreteFormV3 {
        pos: pos.to_owned(),
        form_type,
        regional_variants: SuggestedRegionalVariantsV3::Common {
            common: SuggestedCommonFormVariantV3 {
                dialect: CommonDialectV3::Common,
                spelling: spelling.to_owned(),
                pronunciations,
            },
        },
    }
}

fn uk_us_form(
    pos: &str,
    form_type: WordFormTypeV3,
    uk: &str,
    us: &str,
    uk_pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
    us_pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
) -> SuggestedConcreteFormV3 {
    SuggestedConcreteFormV3 {
        pos: pos.to_owned(),
        form_type,
        regional_variants: SuggestedRegionalVariantsV3::UkUs {
            uk: SuggestedUkFormVariantV3 {
                dialect: UkDialectV3::Uk,
                spelling: uk.to_owned(),
                pronunciations: uk_pronunciations,
            },
            us: SuggestedUsFormVariantV3 {
                dialect: UsDialectV3::Us,
                spelling: us.to_owned(),
                pronunciations: us_pronunciations,
            },
        },
    }
}

const fn form_type_rank(form_type: WordFormTypeV3) -> u8 {
    match form_type {
        WordFormTypeV3::Base => 0,
        WordFormTypeV3::ThirdPersonSingular => 1,
        WordFormTypeV3::PresentParticiple => 2,
        WordFormTypeV3::PastTense => 3,
        WordFormTypeV3::PastParticiple => 4,
        WordFormTypeV3::Plural => 5,
        WordFormTypeV3::Comparative => 6,
        WordFormTypeV3::Superlative => 7,
    }
}

fn suggestion_sort_key(
    form: &SuggestedConcreteFormV3,
    suggested_pos: &[String],
) -> (usize, u8, String) {
    let spelling = match &form.regional_variants {
        SuggestedRegionalVariantsV3::Common { common } => common.spelling.clone(),
        SuggestedRegionalVariantsV3::UkUs { uk, us } => format!("{}\0{}", uk.spelling, us.spelling),
    };
    (
        suggested_pos
            .iter()
            .position(|pos| pos == &form.pos)
            .unwrap_or(usize::MAX),
        form_type_rank(form.form_type),
        spelling,
    )
}

fn suggestion_has_pronunciations(form: &SuggestedConcreteFormV3) -> bool {
    match &form.regional_variants {
        SuggestedRegionalVariantsV3::Common { common } => !common.pronunciations.is_empty(),
        SuggestedRegionalVariantsV3::UkUs { uk, us } => {
            !uk.pronunciations.is_empty() || !us.pronunciations.is_empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn record(pos: &str, forms: Value, sounds: Value) -> DictionaryContentRecord {
        DictionaryContentRecord {
            pos: pos.to_owned(),
            forms,
            sounds,
            provider_name: "Kaikki English Wiktionary".to_owned(),
            provider_version: "enwiktionary-test".to_owned(),
        }
    }

    #[test]
    fn child_maps_plural_and_base_ipa_without_fabricating_actual_pronunciation() {
        let result = build_dictionary_suggestions(
            "child",
            &["noun".to_owned()],
            &[record(
                "noun",
                json!([
                    {"form": "children", "tags": ["plural"]},
                    {"form": "childer", "tags": ["archaic", "dialectal", "plural"]}
                ]),
                json!([{"ipa": "/tʃaɪld/"}]),
            )],
        );
        let encoded = serde_json::to_value(&result.forms).unwrap();

        assert_eq!(encoded.as_array().unwrap().len(), 2);
        assert_eq!(encoded[0]["form_type"], "base");
        assert_eq!(
            encoded[0]["regional_variants"]["common"]["spelling"],
            "child"
        );
        assert_eq!(
            encoded[0]["regional_variants"]["common"]["pronunciations"][0]["dict_phonetic"],
            "/tʃaɪld/"
        );
        assert!(
            encoded[0]["regional_variants"]["common"]["pronunciations"][0]
                .get("actual_pron")
                .is_none()
        );
        assert_eq!(encoded[1]["form_type"], "plural");
        assert_eq!(
            encoded[1]["regional_variants"]["common"]["spelling"],
            "children"
        );
        assert!(
            encoded[1]["regional_variants"]["common"]["pronunciations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(result.has_pronunciations);
    }

    #[test]
    fn verb_tags_map_to_distinct_platform_form_types_in_stable_order() {
        let result = build_dictionary_suggestions(
            "run",
            &["verb".to_owned()],
            &[record(
                "verb",
                json!([
                    {"form": "runs", "tags": ["present", "simple", "singular", "third-person"]},
                    {"form": "running", "tags": ["present", "participle"]},
                    {"form": "ran", "tags": ["past", "simple"]},
                    {"form": "run", "tags": ["past", "participle"]},
                    {"form": "run", "tags": ["past", "simple", "participle"]}
                ]),
                json!([]),
            )],
        );
        let encoded = serde_json::to_value(&result.forms).unwrap();
        let types = encoded
            .as_array()
            .unwrap()
            .iter()
            .map(|form| form["form_type"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            types,
            [
                "base",
                "third_person_singular",
                "present_participle",
                "past_tense",
                "past_participle"
            ]
        );
    }

    #[test]
    fn unknown_empty_and_unsupported_region_evidence_is_ignored() {
        let result = build_dictionary_suggestions(
            "clear",
            &["adjective".to_owned()],
            &[record(
                "adj",
                json!([
                    {"form": "clearer", "tags": ["comparative"]},
                    {"form": "", "tags": ["superlative"]},
                    {"form": "clearest", "tags": ["future-tag"]},
                    {"form": "clearer", "tags": ["comparative", "UK"]}
                ]),
                json!([
                    {"ipa": ""},
                    {"ipa": "/klɪə/", "tags": ["Canada"]}
                ]),
            )],
        );
        let encoded = serde_json::to_value(&result.forms).unwrap();

        assert_eq!(encoded.as_array().unwrap().len(), 2);
        assert_eq!(encoded[1]["form_type"], "comparative");
        assert_eq!(
            encoded[1]["regional_variants"]["common"]["spelling"],
            "clearer"
        );
        assert!(!result.has_pronunciations);
    }

    #[test]
    fn paired_uk_us_ipa_is_preserved_but_single_sided_evidence_is_not_common() {
        let paired = build_dictionary_suggestions(
            "route",
            &["noun".to_owned()],
            &[record(
                "noun",
                json!([]),
                json!([
                    {"ipa": "/ruːt/", "tags": ["UK"]},
                    {"ipa": "/raʊt/", "tags": ["US"]}
                ]),
            )],
        );
        let paired = serde_json::to_value(&paired.forms).unwrap();
        assert_eq!(paired[0]["regional_variants"]["mode"], "uk_us");
        assert_eq!(
            paired[0]["regional_variants"]["uk"]["pronunciations"][0]["dict_phonetic"],
            "/ruːt/"
        );
        assert_eq!(
            paired[0]["regional_variants"]["us"]["pronunciations"][0]["dict_phonetic"],
            "/raʊt/"
        );

        let single = build_dictionary_suggestions(
            "route",
            &["noun".to_owned()],
            &[record(
                "noun",
                json!([]),
                json!([{"ipa": "/ruːt/", "tags": ["UK"]}]),
            )],
        );
        let single = serde_json::to_value(&single.forms).unwrap();
        assert_eq!(single[0]["regional_variants"]["mode"], "common");
        assert!(
            single[0]["regional_variants"]["common"]["pronunciations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn current_kaikki_child_and_verb_tags_map_without_requiring_simple() {
        let child = build_dictionary_suggestions(
            "child",
            &["noun".to_owned()],
            &[record(
                "noun",
                json!([{"form": "children", "tags": ["plural"]}]),
                json!([{
                    "ipa": "/tʃaɪld/",
                    "tags": ["General-American", "Received-Pronunciation"]
                }]),
            )],
        );
        let child = serde_json::to_value(&child.forms).unwrap();
        assert_eq!(
            child[0]["regional_variants"]["common"]["pronunciations"][0]["dict_phonetic"],
            "/tʃaɪld/"
        );

        let verb = build_dictionary_suggestions(
            "color",
            &["verb".to_owned()],
            &[record(
                "verb",
                json!([
                    {"form": "colors", "tags": ["present", "singular", "third-person"]},
                    {"form": "colored", "tags": ["past"]}
                ]),
                json!([]),
            )],
        );
        let verb = serde_json::to_value(&verb.forms).unwrap();
        assert_eq!(verb[1]["form_type"], "third_person_singular");
        assert_eq!(verb[2]["form_type"], "past_tense");
    }
}
