use crate::lexicon::dto::{Dialect, WordDefinitionV2};

pub(crate) const POS_ROLE: &str = "forms.pos";
pub(crate) const BASE_FORM_ROLE: &str = "forms.base_form";
pub(crate) const FORM_GROUP_ROLE: &str = "forms.form_group";
pub(crate) const PRONUNCIATION_ROLE: &str = "forms.pronunciation";
pub(crate) const SENSE_GROUP_ROLE: &str = "meanings.sense_group";
pub(crate) const GRAMMAR_STRUCTURE_ROLE: &str = "meanings.grammar_structure";
pub(crate) const SENSE_ROLE: &str = "meanings.sense";
pub(crate) const SENTENCE_ROLE: &str = "meanings.sentence";
pub(crate) const RELATION_ROLE: &str = "meanings.relation";
pub(crate) const LEGACY_NODE_ROLE: &str = "legacy";

pub(crate) fn form_slot_role(form_type: &str) -> String {
    format!("forms.form_slot:{form_type}")
}

pub(crate) fn form_variant_role(dialect: Dialect) -> String {
    format!("forms.form_variant:{}", dialect_name(dialect))
}

pub(crate) fn definition_role(definition: &WordDefinitionV2) -> &'static str {
    match definition {
        WordDefinitionV2::ZhDefinition { .. } => "meanings.definition:zh:definition",
        WordDefinitionV2::ZhSentence { .. } => "meanings.definition:zh:sentence",
        WordDefinitionV2::EnDefinition { .. } => "meanings.definition:en:definition",
        WordDefinitionV2::EnSentence { .. } => "meanings.definition:en:sentence",
    }
}

pub(crate) fn text_variant_role(field_role: &str, language: &str, dialect: Dialect) -> String {
    format!("meanings.{field_role}:{language}:{}", dialect_name(dialect))
}

pub(crate) const fn dialect_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Common => "common",
        Dialect::Uk => "uk",
        Dialect::Us => "us",
    }
}
