pub(crate) const NOUN_FORM_TYPES: &[&str] = &["plural"];
pub(crate) const VERB_FORM_TYPES: &[&str] = &[
    "third_person_singular",
    "present_participle",
    "past_tense",
    "past_participle",
];
pub(crate) const ADJECTIVE_FORM_TYPES: &[&str] = &["comparative", "superlative"];
pub(crate) const ADVERB_FORM_TYPES: &[&str] = &["comparative", "superlative"];

pub(crate) fn allowed_form_types(part_of_speech: &str) -> &'static [&'static str] {
    match part_of_speech {
        "noun" => NOUN_FORM_TYPES,
        "verb" => VERB_FORM_TYPES,
        "adjective" => ADJECTIVE_FORM_TYPES,
        "adverb" => ADVERB_FORM_TYPES,
        _ => &[],
    }
}

pub(crate) fn owned_allowed_form_types(part_of_speech: &str) -> Vec<String> {
    allowed_form_types(part_of_speech)
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parts_expose_ordered_authoritative_form_types_and_fail_closed() {
        assert_eq!(allowed_form_types("noun"), ["plural"]);
        assert_eq!(
            allowed_form_types("verb"),
            [
                "third_person_singular",
                "present_participle",
                "past_tense",
                "past_participle"
            ]
        );
        assert_eq!(
            allowed_form_types("adjective"),
            ["comparative", "superlative"]
        );
        assert_eq!(allowed_form_types("adverb"), ["comparative", "superlative"]);
        assert!(allowed_form_types("preposition").is_empty());
        assert!(allowed_form_types("custom_part").is_empty());
    }
}
