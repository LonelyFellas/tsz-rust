use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::lexicon::dto::{GrammarFormLinkV3, RichTextV2, VoiceSettingV3};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GeneratedSpeechSource {
    Text {
        content: RichTextV2,
        #[serde(default)]
        form_links: Vec<GrammarFormLinkV3>,
    },
    Phoneme {
        content: RichTextV2,
        alphabet: crate::lexicon::dto::RichTextPhonemeAlphabet,
        phoneme: String,
        #[serde(default)]
        ups_words: Vec<crate::lexicon::dto::UpsWordV3>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phoneme_locale: Option<crate::lexicon::dto::PhonemeLocaleV3>,
    },
}

pub(crate) fn invalid() -> crate::error::AppError {
    crate::error::AppError::bad_request(
        crate::error::ErrorCode::InvalidSpeechPreview,
        "配音内容或参数无效，请重新打开编辑器检查",
    )
}

impl GeneratedSpeechSource {
    pub(crate) fn validate_locale(&self, locale: &str) -> Result<(), crate::error::AppError> {
        let locale = super::locale::EnglishLocale::parse(locale).ok_or_else(invalid)?;
        if let Self::Phoneme {
            phoneme_locale: Some(source_locale),
            ..
        } = self
        {
            let source_locale = match source_locale {
                crate::lexicon::dto::PhonemeLocaleV3::EnGb => "en-GB",
                crate::lexicon::dto::PhonemeLocaleV3::EnUs => "en-US",
            };
            if source_locale != locale.as_str() {
                return Err(crate::error::AppError::bad_request(
                    crate::error::ErrorCode::InvalidSpeechPreview,
                    "音素地区必须与音色原生地区完全一致",
                ));
            }
        }
        let designated = match self {
            Self::Phoneme { .. } => true,
            Self::Text {
                content,
                form_links,
            } => {
                !form_links.is_empty()
                    || content.annotations.iter().any(|annotation| {
                        matches!(
                            annotation,
                            crate::lexicon::dto::RichTextAnnotation::Phoneme { .. }
                        )
                    })
            }
        };
        if !locale.is_legacy() && designated {
            return Err(crate::error::AppError::bad_request(
                crate::error::ErrorCode::InvalidSpeechPreview,
                "指定音素或关联词形尚未验证适用于该原生地区，请明确选择自然朗读或使用匹配地区的音色",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_model(
        &self,
        voice: &super::Voice,
    ) -> Result<(), crate::error::AppError> {
        if voice.is_hd_flash()
            && match self {
                Self::Phoneme { .. } => true,
                Self::Text {
                    content,
                    form_links,
                } => {
                    !form_links.is_empty()
                        || content.annotations.iter().any(|annotation| {
                            matches!(
                                annotation,
                                crate::lexicon::dto::RichTextAnnotation::Phoneme { .. }
                                    | crate::lexicon::dto::RichTextAnnotation::Pause { .. }
                            )
                        })
                }
            }
        {
            return Err(crate::error::AppError::bad_request(
                crate::error::ErrorCode::InvalidSpeechPreview,
                super::SpeechModelError::UnsupportedModelCapability.to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn canonical(&self) -> Result<Value, crate::error::AppError> {
        let mut value = serde_json::to_value(self).map_err(crate::error::AppError::internal)?;
        // Locale is validated against the target, but is not part of the historical source identity.
        value
            .as_object_mut()
            .ok_or_else(invalid)?
            .remove("phoneme_locale");
        // Grammar phonemes are resolved from explicit links for the selected accent.
        if value["form_links"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
        {
            value["content"]["annotations"]
                .as_array_mut()
                .ok_or_else(invalid)?
                .retain(|a| a["type"] != "phoneme");
        }
        let content: RichTextV2 =
            serde_json::from_value(value["content"].clone()).map_err(|_| invalid())?;
        value["content"] = serde_json::to_value(normalize_content(content)?)
            .map_err(crate::error::AppError::internal)?;
        // Saving/publishing may pin a draft link to an immutable publication. The
        // resolved phonemes are checked separately, so this metadata alone is not stale audio.
        if let Some(links) = value.get_mut("form_links").and_then(Value::as_array_mut) {
            for link in links {
                if let Some(fields) = link.as_object_mut() {
                    fields.remove("target_publication_id");
                }
            }
        }
        Ok(value)
    }

    pub(crate) async fn resolve(
        &self,
        conn: &mut sqlx::PgConnection,
        locale: &str,
    ) -> Result<RichTextV2, crate::error::AppError> {
        self.resolve_with_forms(conn, locale, None, None).await
    }

    pub(crate) async fn resolve_with_forms(
        &self,
        conn: &mut sqlx::PgConnection,
        locale: &str,
        current: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
        batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
    ) -> Result<RichTextV2, crate::error::AppError> {
        self.validate_locale(locale)?;
        let mut value = self.canonical()?["content"].clone();
        match self {
            Self::Text { form_links, .. } => {
                if !form_links.is_empty() {
                    if form_links.len() > 100 {
                        return Err(invalid());
                    }
                    for link in form_links {
                        let local = current.filter(|(id, _)| {
                            *id == link.target_word_id && link.target_publication_id.is_none()
                        });
                        let batch_target = batch
                            .and_then(|batch| batch.words.get(&link.target_word_id))
                            .filter(|candidate| {
                                link.target_publication_id == Some(candidate.publication_id)
                            });
                        let forms: Option<Value> = if let Some(candidate) = batch_target {
                            Some(
                                serde_json::to_value(&candidate.word.forms)
                                    .map_err(crate::error::AppError::internal)?,
                            )
                        } else if let Some((_, forms)) = local {
                            Some(
                                serde_json::to_value(forms)
                                    .map_err(crate::error::AppError::internal)?,
                            )
                        } else if let Some(publication) = link.target_publication_id {
                            sqlx::query_scalar("SELECT p.snapshot->'forms' FROM lexicon.entry_publications p JOIN lexicon.entries e ON e.id=p.entry_id WHERE p.id=$1 AND e.id=$2 AND e.archived_at IS NULL")
                                .bind(publication).bind(link.target_word_id).fetch_optional(&mut *conn).await.map_err(crate::error::AppError::internal)?
                        } else {
                            sqlx::query_scalar("SELECT p.forms FROM lexicon.entry_editor_projection p JOIN lexicon.entries e ON e.id=p.entry_id WHERE e.id=$1 AND e.archived_at IS NULL FOR SHARE OF e")
                                .bind(link.target_word_id).fetch_optional(&mut *conn).await.map_err(crate::error::AppError::internal)?
                        };
                        let forms: crate::lexicon::dto::DraftFormsStepContentV3 =
                            serde_json::from_value(forms.ok_or_else(invalid)?)
                                .map_err(|_| invalid())?;
                        let (_, dialect, pronunciations) =
                            crate::lexicon::service::grammar_form_links::target_variant(
                                &forms, link,
                            )
                            .ok_or_else(invalid)?;
                        if link.source_segments.len() != 1 {
                            return Err(invalid());
                        }
                        let segment = &link.source_segments[0];
                        let chars = value["text"]
                            .as_str()
                            .ok_or_else(invalid)?
                            .chars()
                            .collect::<Vec<_>>();
                        if segment.start >= segment.end
                            || segment.end > chars.len()
                            || chars[segment.start..segment.end].iter().collect::<String>()
                                != segment.surface
                        {
                            return Err(invalid());
                        }
                        let accent = if locale == "en-GB" {
                            crate::lexicon::dto::Dialect::Uk
                        } else {
                            crate::lexicon::dto::Dialect::Us
                        };
                        if let Some(annotation) =
                            crate::lexicon::service::grammar_form_links::first_annotation(
                                &pronunciations,
                                segment.start,
                                segment.end,
                                accent,
                                dialect,
                            )
                            .map_err(|_| invalid())?
                        {
                            value["annotations"]
                                .as_array_mut()
                                .ok_or_else(invalid)?
                                .push(
                                    serde_json::to_value(annotation)
                                        .map_err(crate::error::AppError::internal)?,
                                );
                        }
                    }
                }
            }
            Self::Phoneme {
                alphabet,
                phoneme,
                ups_words,
                ..
            } => {
                let text = value["text"].as_str().ok_or_else(invalid)?.to_owned();
                if text.trim().is_empty() || phoneme.trim().is_empty() {
                    return Err(invalid());
                }
                let annotations = value["annotations"].as_array_mut().ok_or_else(invalid)?;
                if annotations.iter().any(|a| a["type"] != "pause") {
                    return Err(invalid());
                }
                let words = text.split_whitespace().collect::<Vec<_>>();
                let is_ups = matches!(alphabet, crate::lexicon::dto::RichTextPhonemeAlphabet::Ups);
                let split = (is_ups && (words.len() > 1 || !ups_words.is_empty()))
                    || (!is_ups && !annotations.is_empty() && words.len() > 1);
                if split {
                    let phones = if is_ups {
                        if ups_words.len() != words.len()
                            || ups_words.iter().zip(&words).any(|(p, w)| p.text != *w)
                            || ups_words
                                .iter()
                                .map(|p| p.phoneme.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                                != phoneme.trim()
                        {
                            return Err(invalid());
                        }
                        ups_words
                            .iter()
                            .map(|p| p.phoneme.as_str())
                            .collect::<Vec<_>>()
                    } else {
                        phoneme.split_whitespace().collect::<Vec<_>>()
                    };
                    if phones.len() != words.len() {
                        return Err(invalid());
                    }
                    let mut offset = 0;
                    for (word, phone) in words.iter().zip(phones) {
                        let byte_start = text[offset..].find(word).ok_or_else(invalid)? + offset;
                        let start = text[..byte_start].chars().count();
                        annotations.push(serde_json::json!({"type":"phoneme","start":start,"end":start+word.chars().count(),"alphabet":alphabet,"phoneme":phone}));
                        offset = byte_start + word.len();
                    }
                } else {
                    annotations.push(serde_json::json!({"type":"phoneme","start":0,"end":text.chars().count(),"alphabet":alphabet,"phoneme":phoneme.trim()}));
                }
            }
        }
        normalize_content(serde_json::from_value(value).map_err(|_| invalid())?)
    }
}

fn normalize_content(content: RichTextV2) -> Result<RichTextV2, crate::error::AppError> {
    let mut rich = crate::lexicon::dto::RichText::V2(content);
    crate::lexicon::rich_text::canonicalize(&mut rich).map_err(|_| invalid())?;
    let crate::lexicon::dto::RichText::V2(content) = rich else {
        unreachable!()
    };
    Ok(content)
}

pub(crate) fn voice_input(setting: &VoiceSettingV3) -> Value {
    serde_json::json!({"version_id":setting.version_id,"voice_id":setting.voice_id,"locale":setting.locale,"rate_percent":setting.rate_percent,"pitch_semitones":setting.pitch_semitones.unwrap_or(0),"volume_percent":setting.volume_percent.unwrap_or(100),"style":setting.style.as_deref().unwrap_or("")})
}

#[derive(Debug, Clone)]
pub(crate) struct GeneratedReference {
    pub asset: Uuid,
    pub source: GeneratedSpeechSource,
    pub setting: VoiceSettingV3,
}

/// Walk the owned DTO representation. Each voice_profile is paired with its actual text owner.
pub(crate) fn references(value: &Value) -> Result<Vec<GeneratedReference>, crate::error::AppError> {
    fn walk(
        v: &Value,
        node: Uuid,
        phoneme: Option<(&str, &str, &Value)>,
        native_locale: Option<&str>,
        out: &mut Vec<GeneratedReference>,
    ) -> Result<(), crate::error::AppError> {
        let node = v
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or(node);
        if let Some(profile) = v.get("voice_profile").filter(|profile| !profile.is_null()) {
            let voices = profile["voices"].as_array().ok_or_else(invalid)?;
            for setting in voices {
                let Some(asset) = setting.get("audio_asset_id") else {
                    continue;
                };
                let source = if let Some((alphabet, phone, words)) = phoneme {
                    let mut source = serde_json::json!({"kind":"phoneme","content":v["content"],"alphabet":alphabet,"phoneme":phone,"ups_words":words});
                    if let Some(locale) = native_locale {
                        source["phoneme_locale"] = locale.into();
                    }
                    source
                } else {
                    let content = v.get("content").or_else(||v.get("value")).or_else(||v.get("name_en_rich")).or_else(||v.get("dict_phonetic_rich")).cloned().unwrap_or_else(||serde_json::json!({"version":2,"text":v.get("name_en").or_else(||v.get("dict_phonetic")).and_then(Value::as_str).unwrap_or(""),"annotations":[]}));
                    serde_json::json!({"kind":"text","content":content,"form_links":v.get("form_links").filter(|x|!x.is_null()).cloned().unwrap_or_else(||serde_json::json!([]))})
                };
                out.push(GeneratedReference {
                    asset: serde_json::from_value(asset.clone()).map_err(|_| invalid())?,
                    source: serde_json::from_value(source).map_err(|_| invalid())?,
                    setting: serde_json::from_value(setting.clone()).map_err(|_| invalid())?,
                });
            }
        }
        match v {
            Value::Object(fields) => {
                for (key, child) in fields {
                    if key == "voice_profile" {
                        continue;
                    }
                    let context = if key == "ipa_speech" || key == "ups_speech" {
                        let alphabet = if key == "ipa_speech" { "ipa" } else { "ups" };
                        Some((
                            alphabet,
                            v[alphabet].as_str().unwrap_or(""),
                            v.get("ups_words")
                                .filter(|x| alphabet == "ups" && x.is_array())
                                .unwrap_or(&Value::Null),
                        ))
                    } else {
                        None
                    };
                    if let Some((alphabet, phone, words)) = context {
                        let empty = serde_json::json!([]);
                        walk(
                            child,
                            node,
                            Some((
                                alphabet,
                                phone,
                                if words.is_null() { &empty } else { words },
                            )),
                            v.get(format!("{alphabet}_locale"))
                                .and_then(Value::as_str)
                                .or(native_locale),
                            out,
                        )?;
                    } else {
                        let locale = match key.as_str() {
                            "uk" => Some("en-GB"),
                            "us" => Some("en-US"),
                            _ => native_locale.or_else(|| {
                                v.get("dialect")
                                    .and_then(Value::as_str)
                                    .and_then(|dialect| match dialect {
                                        "uk" => Some("en-GB"),
                                        "us" => Some("en-US"),
                                        _ => None,
                                    })
                            }),
                        };
                        walk(child, node, None, locale, out)?;
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, node, None, native_locale, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut result = vec![];
    walk(value, Uuid::nil(), None, None, &mut result)?;
    Ok(result)
}
