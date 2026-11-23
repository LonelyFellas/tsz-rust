use serde::Serialize;
use sqlx::{Postgres, Transaction};

use super::dto::{
    DraftFormsStepContentV3, FormContentRegionalVariantsV3, FormContentVariantV3,
    FormsStepRepresentationV3, RichTextAnnotationV3, RichTextV2V3, RichTextV3,
    SharedFormsStepContentV3, SynthesisSpeechV3, VoiceProfileV3, WordCommonFormVariantV3,
    WordPronunciationV3, WordRegionalVariantsV3, WordUkFormVariantV3, WordUsFormVariantV3,
};

pub const ADVISORY_LOCK_KEY: &str = "lexicon.rich-text-repair";

pub async fn lock_transaction(tx: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RepairCounts {
    pub spelling_rich_removed: u64,
    pub spelling_speech_reset: u64,
    pub phonetic_rich_removed: u64,
    pub synthesis_speech_reset: u64,
}

impl RepairCounts {
    pub fn total(&self) -> u64 {
        self.spelling_rich_removed
            + self.spelling_speech_reset
            + self.phonetic_rich_removed
            + self.synthesis_speech_reset
    }

    pub(crate) fn add_assign(&mut self, other: &Self) {
        self.spelling_rich_removed += other.spelling_rich_removed;
        self.spelling_speech_reset += other.spelling_speech_reset;
        self.phonetic_rich_removed += other.phonetic_rich_removed;
        self.synthesis_speech_reset += other.synthesis_speech_reset;
    }
}

fn valid_rich_text(value: &RichTextV3, expected: &str) -> bool {
    value.text() == expected && crate::lexicon::rich_text::is_valid_native(value)
}

fn clear_voice_audio(profile: &mut VoiceProfileV3) {
    for voice in &mut profile.voices {
        voice.audio_asset_id = None;
    }
}

fn empty_rich_text(text: &str) -> RichTextV2V3 {
    RichTextV2V3 {
        version: 2,
        text: text.to_owned(),
        annotations: Vec::new(),
    }
}

fn valid_synthesis_speech(content: &RichTextV2V3, spelling: &str) -> bool {
    if content.text != spelling {
        return false;
    }
    let chars: Vec<_> = spelling.chars().collect();
    let valid_annotations = content
        .annotations
        .iter()
        .all(|annotation| match annotation {
            RichTextAnnotationV3::SayAs { .. } | RichTextAnnotationV3::Sub { .. } => true,
            RichTextAnnotationV3::Pause { at, .. } => {
                *at > 0
                    && *at < chars.len()
                    && (chars[*at - 1].is_whitespace() || chars[*at].is_whitespace())
                    && chars[..*at]
                        .iter()
                        .any(|character| !character.is_whitespace())
                    && chars[*at..]
                        .iter()
                        .any(|character| !character.is_whitespace())
            }
            _ => false,
        });
    if !valid_annotations {
        return false;
    }
    let mut canonical = content.clone();
    crate::lexicon::rich_text::canonicalize_spelling_speech(&mut canonical)
}

fn repair_synthesis_speech(
    speech: &mut SynthesisSpeechV3,
    spelling: &str,
    counts: &mut RepairCounts,
) {
    if valid_synthesis_speech(&speech.content, spelling) {
        return;
    }
    speech.content = empty_rich_text(spelling);
    clear_voice_audio(&mut speech.voice_profile);
    counts.synthesis_speech_reset += 1;
}

fn repair_pronunciation(
    pronunciation: &mut WordPronunciationV3,
    spelling: &str,
    counts: &mut RepairCounts,
) {
    if pronunciation
        .dict_phonetic_rich
        .as_ref()
        .is_some_and(|value| !valid_rich_text(value, &pronunciation.dict_phonetic))
    {
        pronunciation.dict_phonetic_rich = None;
        counts.phonetic_rich_removed += 1;
    }
    if pronunciation
        .actual_pron_rich
        .as_ref()
        .is_some_and(|value| !valid_rich_text(value, &pronunciation.actual_pron))
    {
        pronunciation.actual_pron_rich = None;
        counts.phonetic_rich_removed += 1;
    }
    let Some(synthesis) = pronunciation.synthesis.as_mut() else {
        return;
    };
    for candidate in [synthesis.uk.as_mut(), synthesis.us.as_mut()]
        .into_iter()
        .flatten()
    {
        if let Some(speech) = candidate.ipa_speech.as_mut() {
            repair_synthesis_speech(speech, spelling, counts);
        }
        if let Some(speech) = candidate.ups_speech.as_mut() {
            repair_synthesis_speech(speech, spelling, counts);
        }
    }
}

fn repair_variant_spelling_speech(
    speech: &mut super::dto::SpellingSpeechV3,
    spelling: &str,
    counts: &mut RepairCounts,
) {
    let mut changed = false;
    for content in speech.contents_mut() {
        if content.text != spelling
            || !crate::lexicon::rich_text::canonicalize_spelling_speech(&mut content.clone())
        {
            *content = empty_rich_text(spelling);
            changed = true;
        }
    }
    if changed {
        clear_voice_audio(&mut speech.voice_profile);
        counts.spelling_speech_reset += 1;
    }
}

fn repair_shared_variant(variant: &mut FormContentVariantV3, counts: &mut RepairCounts) {
    if let Some(rich) = variant.spelling_rich.as_ref() {
        let invalid = rich.text != variant.spelling
            || !crate::lexicon::rich_text::canonicalize_spelling(&mut rich.clone());
        if invalid {
            variant.spelling_rich = None;
            counts.spelling_rich_removed += 1;
        }
    }
    let spelling = variant.spelling.clone();
    if let Some(speech) = variant.spelling_speech.as_mut() {
        repair_variant_spelling_speech(speech, &spelling, counts);
    }
    for pronunciation in &mut variant.pronunciations {
        repair_pronunciation(pronunciation, &spelling, counts);
    }
}

fn repair_word_variant(variant: &mut impl WordVariantFields, counts: &mut RepairCounts) {
    if let Some(rich) = variant.spelling_rich()
        && (rich.text != variant.spelling()
            || !crate::lexicon::rich_text::canonicalize_spelling(&mut rich.clone()))
    {
        variant.clear_spelling_rich();
        counts.spelling_rich_removed += 1;
    }
    let spelling = variant.spelling().to_owned();
    if let Some(speech) = variant.spelling_speech_mut() {
        repair_variant_spelling_speech(speech, &spelling, counts);
    }
    for pronunciation in variant.pronunciations_mut() {
        repair_pronunciation(pronunciation, &spelling, counts);
    }
}

trait WordVariantFields {
    fn spelling(&self) -> &str;
    fn spelling_rich(&self) -> Option<&RichTextV2V3>;
    fn spelling_speech_mut(&mut self) -> Option<&mut Box<super::dto::SpellingSpeechV3>>;
    fn clear_spelling_rich(&mut self);
    fn pronunciations_mut(&mut self) -> &mut Vec<WordPronunciationV3>;
}

macro_rules! impl_word_variant_fields {
    ($type:ty) => {
        impl WordVariantFields for $type {
            fn spelling(&self) -> &str {
                &self.spelling
            }
            fn spelling_rich(&self) -> Option<&RichTextV2V3> {
                self.spelling_rich.as_ref()
            }
            fn spelling_speech_mut(&mut self) -> Option<&mut Box<super::dto::SpellingSpeechV3>> {
                self.spelling_speech.as_mut()
            }
            fn clear_spelling_rich(&mut self) {
                self.spelling_rich = None;
            }
            fn pronunciations_mut(&mut self) -> &mut Vec<WordPronunciationV3> {
                &mut self.pronunciations
            }
        }
    };
}

impl_word_variant_fields!(WordCommonFormVariantV3);
impl_word_variant_fields!(WordUkFormVariantV3);
impl_word_variant_fields!(WordUsFormVariantV3);

pub fn repair_shared_forms(content: &mut SharedFormsStepContentV3) -> RepairCounts {
    let mut counts = RepairCounts::default();
    for form_content in &mut content.contents {
        match &mut form_content.regional_variants {
            FormContentRegionalVariantsV3::Common { common } => {
                repair_shared_variant(common, &mut counts)
            }
            FormContentRegionalVariantsV3::UkUs { uk, us } => {
                repair_shared_variant(uk, &mut counts);
                repair_shared_variant(us, &mut counts);
            }
            FormContentRegionalVariantsV3::UkUsMany { uk, us } => {
                for variant in uk.iter_mut().chain(us.iter_mut()) {
                    repair_shared_variant(variant, &mut counts);
                }
            }
        }
    }
    counts
}

pub fn repair_expanded_forms(content: &mut DraftFormsStepContentV3) -> RepairCounts {
    let mut counts = RepairCounts::default();
    for pos in &mut content.pos {
        for form in &mut pos.forms {
            match &mut form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    repair_word_variant(common, &mut counts)
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    repair_word_variant(uk, &mut counts);
                    repair_word_variant(us, &mut counts);
                }
                WordRegionalVariantsV3::UkUsMany { uk, us } => {
                    for variant in uk.iter_mut() {
                        repair_word_variant(variant, &mut counts);
                    }
                    for variant in us.iter_mut() {
                        repair_word_variant(variant, &mut counts);
                    }
                }
            }
        }
    }
    if let Some(shared) = content.shared.as_mut() {
        let shared_counts = repair_shared_forms(shared);
        counts.add_assign(&shared_counts);
    }
    counts
}

pub fn repair_representation(content: &mut FormsStepRepresentationV3) -> RepairCounts {
    match content {
        FormsStepRepresentationV3::Shared(shared) => repair_shared_forms(shared),
        FormsStepRepresentationV3::Expanded(expanded) => {
            let mut counts = RepairCounts::default();
            for pos in &mut expanded.pos {
                for form in &mut pos.forms {
                    match &mut form.regional_variants {
                        WordRegionalVariantsV3::Common { common } => {
                            repair_word_variant(common, &mut counts)
                        }
                        WordRegionalVariantsV3::UkUs { uk, us } => {
                            repair_word_variant(uk, &mut counts);
                            repair_word_variant(us, &mut counts);
                        }
                        WordRegionalVariantsV3::UkUsMany { uk, us } => {
                            for variant in uk.iter_mut() {
                                repair_word_variant(variant, &mut counts);
                            }
                            for variant in us.iter_mut() {
                                repair_word_variant(variant, &mut counts);
                            }
                        }
                    }
                }
            }
            counts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::dto::{
        DialectRulesV3, FormContentRegionalVariantsV3, FormContentV3, TextOrigin,
        WordPronunciationV3,
    };
    use uuid::Uuid;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn repairs_stale_rich_text_and_keeps_plain_fields() {
        let mut content = SharedFormsStepContentV3 {
            forms_format: crate::lexicon::dto::SharedFormsFormatV3::SharedV1,
            contents: vec![FormContentV3 {
                id: id(1),
                dialect_rules: DialectRulesV3::UNIFIED,
                regional_variants: FormContentRegionalVariantsV3::Common {
                    common: FormContentVariantV3 {
                        id: id(2),
                        spelling: "centre".into(),
                        spelling_rich: Some(RichTextV2V3 {
                            version: 2,
                            text: "stale".into(),
                            annotations: vec![],
                        }),
                        spelling_speech: None,
                        origin: TextOrigin::Manual,
                        pronunciations: vec![WordPronunciationV3 {
                            id: id(3),
                            synthesis: None,
                            dict_phonetic: "sen-tre".into(),
                            dict_phonetic_rich: Some(RichTextV3::V2(RichTextV2V3 {
                                version: 2,
                                text: "stale".into(),
                                annotations: vec![],
                            })),
                            voice_profile: None,
                            audio_assets: vec![],
                            actual_pron: "centre".into(),
                            actual_pron_rich: None,
                            style: None,
                        }],
                    },
                },
                pronunciation_slots: vec![],
            }],
            pos: vec![],
        };
        let counts = repair_shared_forms(&mut content);
        assert_eq!(counts.spelling_rich_removed, 1);
        assert_eq!(counts.phonetic_rich_removed, 1);
        let FormContentRegionalVariantsV3::Common { common } =
            &content.contents[0].regional_variants
        else {
            panic!("expected common variants")
        };
        assert_eq!(common.spelling, "centre");
        assert_eq!(common.pronunciations[0].dict_phonetic, "sen-tre");
        assert!(common.spelling_rich.is_none());
        assert!(common.pronunciations[0].dict_phonetic_rich.is_none());
    }
}
