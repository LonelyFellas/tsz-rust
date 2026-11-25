/// Normalizes Wiktextract's English labels before resolving any application catalog codes.
pub(crate) fn canonical(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_lowercase().as_str() {
        "noun" => Some("noun"),
        "name" | "proper noun" => Some("proper_name"),
        "verb" | "auxiliary" | "modal" => Some("verb"),
        "adj" | "adjective" => Some("adjective"),
        "adv" | "adverb" => Some("adverb"),
        "pron" | "pronoun" => Some("pronoun"),
        "prep" | "preposition" => Some("preposition"),
        "det" | "determiner" => Some("determiner"),
        "conj" | "conjunction" => Some("conjunction"),
        "intj" | "interjection" => Some("interjection"),
        "num" | "number" | "numeral" => Some("numeral"),
        "article" => Some("article"),
        "prep_phrase" => Some("prepositional_phrase"),
        _ => None,
    }
}
