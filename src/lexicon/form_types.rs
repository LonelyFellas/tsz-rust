pub(crate) const FIXED_FORM_TYPES_WITHOUT_BASE: &[&str] = &[
    "third_person_singular",
    "present_participle",
    "past_tense",
    "past_participle",
    "plural",
    "comparative",
    "superlative",
];

pub(crate) fn allowed_form_types(_part_of_speech: &str) -> &'static [&'static str] {
    FIXED_FORM_TYPES_WITHOUT_BASE
}

pub(crate) fn catalog_form_types(part_of_speech: &str) -> Vec<WordFormTypeWithoutBase> {
    allowed_form_types(part_of_speech)
        .iter()
        .filter_map(|value| match *value {
            "third_person_singular" => Some(WordFormTypeWithoutBase::ThirdPersonSingular),
            "present_participle" => Some(WordFormTypeWithoutBase::PresentParticiple),
            "past_tense" => Some(WordFormTypeWithoutBase::PastTense),
            "past_participle" => Some(WordFormTypeWithoutBase::PastParticiple),
            "plural" => Some(WordFormTypeWithoutBase::Plural),
            "comparative" => Some(WordFormTypeWithoutBase::Comparative),
            "superlative" => Some(WordFormTypeWithoutBase::Superlative),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_exposes_the_complete_ordered_fixed_form_type_catalog() {
        let expected = [
            "third_person_singular",
            "present_participle",
            "past_tense",
            "past_participle",
            "plural",
            "comparative",
            "superlative",
        ];
        for part in [
            "noun",
            "pronoun",
            "verb",
            "adjective",
            "adverb",
            "preposition",
            "article",
            "determiner",
            "conjunction",
            "numeral",
            "interjection",
            "custom_part",
        ] {
            assert_eq!(allowed_form_types(part), expected, "{part}");
        }
    }
}
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WordFormTypeWithoutBase {
    ThirdPersonSingular,
    PresentParticiple,
    PastTense,
    PastParticiple,
    Plural,
    Comparative,
    Superlative,
}
