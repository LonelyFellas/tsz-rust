//! Stable form type codes. Dictionary mappings retain their built-in vocabulary;
//! business candidates and write validation are resolved from the database catalog.
pub type WordFormTypeWithoutBase = String;

pub fn valid_code(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

pub fn parse_code(value: &str) -> Result<String, ()> {
    if valid_code(value) {
        Ok(value.to_owned())
    } else {
        Err(())
    }
}
