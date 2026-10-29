//! Publication requirements, distinct from the retained-asset graph used by GC.
//! Collection does not establish that a registered asset is ready or authorized.
pub mod review;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    generated_source::{GeneratedSpeechSource, source_at},
    locale::AccentGroup,
};
use crate::{
    error::AppError,
    lexicon::dto::{AdminWordV3, EnglishTextV3, VoiceProfileV3, VoiceSettingV3},
};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequirementIssue {
    MissingConfiguration,
    MissingAsset,
    InvalidSource,
    AssetUnavailable,
    EffectMismatch,
    ObjectUnavailable,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRequirement {
    pub target_id: Uuid,
    #[schema(required = true)]
    pub content_id: Option<Uuid>,
    /// JSON pointer to the source owner; never a client-provided storage key.
    pub source_path: String,
    pub default_for: Vec<AccentGroup>,
    #[schema(required = true)]
    pub setting: Option<VoiceSettingV3>,
    // A profile may contain thousands of alternatives. Share it for server-side
    // validation instead of copying and returning the full list per selected voice.
    #[serde(skip)]
    pub profile: Option<Arc<VoiceProfileV3>>,
    #[schema(required = true)]
    pub asset_id: Option<Uuid>,
    #[schema(required = true)]
    pub source: Option<GeneratedSpeechSource>,
    #[schema(required = true)]
    pub issue: Option<RequirementIssue>,
}

pub fn entry_requirements(word: &AdminWordV3) -> Result<Vec<AudioRequirement>, AppError> {
    // Match the canonical authoring graph even when an older stored draft still
    // uses the expanded representation. Binding projections are not audio owners.
    let forms = word
        .forms
        .shared_representation()
        .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
    let value = serde_json::json!({"forms":forms,"meanings":word.meanings});
    collect(&value)
}

pub fn sentence_requirements(text: &EnglishTextV3) -> Result<Vec<AudioRequirement>, AppError> {
    collect(&serde_json::json!({"en_text":text}))
}

fn id(value: &Value) -> Option<Uuid> {
    value.as_str().and_then(|s| Uuid::parse_str(s).ok())
}

fn collect(value: &Value) -> Result<Vec<AudioRequirement>, AppError> {
    let mut requirements = Vec::new();
    walk(value, value, "", Uuid::nil(), None, &mut requirements)?;
    Ok(requirements)
}

fn walk(
    root: &Value,
    value: &Value,
    path: &str,
    target: Uuid,
    content: Option<Uuid>,
    out: &mut Vec<AudioRequirement>,
) -> Result<(), AppError> {
    let target = id(&value["id"]).unwrap_or(target);
    let content =
        if path.starts_with("/forms/contents/") && value.get("regional_variants").is_some() {
            Some(target)
        } else {
            content
        };
    if value.get("spelling").is_some() && value["pronunciations"].is_array() {
        return variant(root, value, path, content, out);
    }
    add_selected(root, value, path, target, content, out)?;
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if matches!(
                    key.as_str(),
                    "voice_profile" | "audio_assets" | "regional_bindings" | "pronunciation_slots"
                ) {
                    continue;
                }
                let key = key.replace('~', "~0").replace('/', "~1");
                walk(root, child, &format!("{path}/{key}"), target, content, out)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk(
                    root,
                    child,
                    &format!("{path}/{index}"),
                    target,
                    content,
                    out,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn add_selected(
    root: &Value,
    owner: &Value,
    path: &str,
    target: Uuid,
    content: Option<Uuid>,
    out: &mut Vec<AudioRequirement>,
) -> Result<(), AppError> {
    if let Some(voices) = owner
        .pointer("/voice_profile/voices")
        .and_then(Value::as_array)
    {
        let profile: VoiceProfileV3 =
            serde_json::from_value(owner["voice_profile"].clone()).map_err(AppError::internal)?;
        let profile = Arc::new(profile);
        if profile.mode() == Some(crate::lexicon::dto::VoiceProfileModeV3::Inherit)
            && !profile.voices.iter().any(|voice| voice.enabled)
        {
            out.push(AudioRequirement {
                target_id: target,
                content_id: content,
                source_path: path.into(),
                default_for: vec![],
                setting: None,
                profile: Some(profile.clone()),
                asset_id: None,
                source: source_at(root, path).ok(),
                issue: Some(RequirementIssue::MissingConfiguration),
            });
        }
        for setting in voices.iter().filter(|v| v["enabled"] == true) {
            let setting: VoiceSettingV3 =
                serde_json::from_value(setting.clone()).map_err(AppError::internal)?;
            let source = source_at(root, path).ok();
            let issue = if source.is_none() {
                Some(RequirementIssue::InvalidSource)
            } else if setting.audio_asset_id.is_none() {
                Some(RequirementIssue::MissingAsset)
            } else {
                None
            };
            out.push(AudioRequirement {
                target_id: target,
                content_id: content,
                source_path: path.into(),
                default_for: vec![],
                asset_id: setting.audio_asset_id,
                setting: Some(setting),
                profile: Some(profile.clone()),
                source,
                issue,
            });
        }
    }
    // Uploaded recordings are selected business assets, not TTS candidates.
    if let Some(assets) = owner["audio_assets"].as_array() {
        for asset in assets {
            if let Some(asset_id) = id(&asset["id"]) {
                out.push(AudioRequirement {
                    target_id: target,
                    content_id: content,
                    source_path: path.into(),
                    default_for: vec![],
                    setting: None,
                    profile: None,
                    asset_id: Some(asset_id),
                    source: None,
                    issue: None,
                });
            }
        }
    }
    Ok(())
}

fn variant(
    root: &Value,
    value: &Value,
    path: &str,
    content: Option<Uuid>,
    out: &mut Vec<AudioRequirement>,
) -> Result<(), AppError> {
    let variant_id = id(&value["id"]).ok_or_else(super::generated_source::invalid)?;
    let spelling_path = format!("{path}/spelling_speech");
    if let Some(spelling) = value.get("spelling_speech") {
        add_selected(root, spelling, &spelling_path, variant_id, content, out)?;
    }
    let accents: &[(&str, AccentGroup)] = match path.rsplit('/').next() {
        Some("uk") => &[("uk", AccentGroup::Uk)],
        Some("us") => &[("us", AccentGroup::Us)],
        _ => &[("uk", AccentGroup::Uk), ("us", AccentGroup::Us)],
    };
    for (index, pronunciation) in value["pronunciations"]
        .as_array()
        .expect("checked variant")
        .iter()
        .enumerate()
    {
        let target = id(&pronunciation["id"]).ok_or_else(super::generated_source::invalid)?;
        let pron_path = format!("{path}/pronunciations/{index}");
        let start = out.len();
        add_selected(root, pronunciation, &pron_path, target, content, out)?;
        let raw_end = out.len();
        let synthesis = &pronunciation["synthesis"];
        let active_start = if synthesis.is_object() {
            raw_end
        } else {
            start
        };
        if synthesis["use_spelling"] == true {
            if let Some(spelling) = value.get("spelling_speech") {
                add_selected(root, spelling, &spelling_path, target, content, out)?;
            }
        } else {
            let field = match synthesis["alphabet"].as_str() {
                Some("ipa") => Some("ipa_speech"),
                Some("ups") => Some("ups_speech"),
                _ => None,
            };
            if let Some(field) = field {
                for (side, _) in accents {
                    if let Some(speech) = synthesis.get(*side).and_then(|s| s.get(field)) {
                        let speech_path = format!("{pron_path}/synthesis/{side}/{field}");
                        add_selected(root, speech, &speech_path, target, content, out)?;
                    }
                }
            }
        }
        for (_, accent) in accents {
            let candidates = (active_start..out.len()).chain((start..raw_end).filter(|index| {
                synthesis.is_object()
                    && out[*index].setting.is_none()
                    && out[*index].asset_id.is_some()
            }));
            let selected = candidates.into_iter().find(|index| {
                let item = &out[*index];
                if let Some(setting) = &item.setting {
                    setting
                        .locale
                        .is_some_and(|locale| locale.accent_group() == *accent)
                } else {
                    root.pointer(&item.source_path)
                        .and_then(|v| v["audio_assets"].as_array())
                        .is_some_and(|assets| {
                            assets.iter().any(|asset| {
                                id(&asset["id"]) == item.asset_id
                                    && asset["locale"]
                                        .as_str()
                                        .and_then(super::locale::EnglishLocale::parse)
                                        .is_some_and(|locale| locale.accent_group() == *accent)
                            })
                        })
                }
            });
            if let Some(index) = selected {
                out[index].default_for.push(*accent);
            } else {
                out.push(AudioRequirement {
                    target_id: target,
                    content_id: content,
                    source_path: pron_path.clone(),
                    default_for: vec![*accent],
                    setting: None,
                    profile: None,
                    asset_id: None,
                    source: None,
                    issue: Some(RequirementIssue::MissingConfiguration),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn voice(locale: &str, enabled: bool, asset: Option<Uuid>) -> Value {
        let mut v = json!({"voice_id":"test","enabled":enabled,"rate_percent":0,"locale":locale});
        if let Some(asset) = asset {
            v["audio_asset_id"] = json!(asset);
        }
        v
    }
    fn fixture(mode: &str) -> Value {
        json!({"forms":{"contents":[{"id":Uuid::now_v7(),"regional_variants":{
            mode:{"id":Uuid::now_v7(),"spelling":"endurance","pronunciations":[{
                "id":Uuid::now_v7(),"dict_phonetic":"test","synthesis":{"alphabet":"ipa","use_spelling":false,
                    "uk":{"ipa":"test","ipa_speech":{"content":{"version":2,"text":"endurance","annotations":[]},"voice_profile":{"voices":[voice("en-GB",true,Some(Uuid::now_v7()))]}}}
                }
            }]}
        }}]}})
    }
    #[test]
    fn explicitly_inherited_empty_optional_profile_is_not_silently_ready() {
        let value = json!({"meanings":{"id":Uuid::now_v7(),"content":{"version":2,"text":"meaning","annotations":[]},
            "voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"word"}}}});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        assert!(items[0].asset_id.is_none());
    }

    #[test]
    fn legacy_generated_voice_cannot_supply_an_explicit_phoneme_default() {
        let mut value = fixture("common");
        value["forms"]["contents"][0]["regional_variants"]["common"]["pronunciations"][0]["voice_profile"] =
            json!({"voices":[voice("en-US",true,Some(Uuid::now_v7()))]});
        let items = collect(&value).unwrap();
        assert!(
            items
                .iter()
                .any(|item| item.default_for == vec![AccentGroup::Us]
                    && matches!(item.issue, Some(RequirementIssue::MissingConfiguration)))
        );
        assert!(
            items
                .iter()
                .filter(|item| item.source_path.ends_with("/pronunciations/0"))
                .filter(|item| item.setting.is_some())
                .all(|item| item.default_for.is_empty())
        );
    }

    #[test]
    fn missing_us_is_not_hidden_by_ready_uk_or_shared_bindings() {
        let mut value = fixture("common");
        value["forms"]["pos"] = json!([{"forms":[{"regional_bindings":{"uk":{"pronunciations":[{"id":Uuid::now_v7()}]}}}]}]);
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0].asset_id.is_some());
        assert_eq!(items[0].default_for, vec![AccentGroup::Uk]);
        assert!(matches!(
            items[1].issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        assert_eq!(items[1].default_for, vec![AccentGroup::Us]);
        assert!(items.iter().all(|i| i.content_id.is_some()));
    }
    #[test]
    fn selected_missing_audio_is_required_but_unselected_and_inactive_alphabet_are_not() {
        let mut value = fixture("uk");
        let candidate = &mut value["forms"]["contents"][0]["regional_variants"]["uk"]["pronunciations"]
            [0]["synthesis"]["uk"];
        candidate["ipa_speech"]["voice_profile"]["voices"] =
            json!([voice("en-GB", true, None), voice("en-GB", false, None)]);
        candidate["ups_speech"] = candidate["ipa_speech"].clone();
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].issue,
            Some(RequirementIssue::MissingAsset)
        ));
        assert!(matches!(
            items[0].source,
            Some(GeneratedSpeechSource::Phoneme { .. })
        ));
    }
    #[test]
    fn optional_text_only_requires_explicitly_enabled_voices() {
        let value = json!({"en_text":{"common":{"id":Uuid::now_v7(),"value":{"version":2,"text":"Hello","annotations":[]},"voice_profile":{"voices":[voice("en-US",true,None),voice("en-GB",false,None)]}}}});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].default_for.is_empty());
        assert!(matches!(
            items[0].issue,
            Some(RequirementIssue::MissingAsset)
        ));
    }
    #[test]
    fn spelling_route_uses_its_form_source_and_uploaded_audio_can_supply_default() {
        let mut value = fixture("uk");
        let variant = &mut value["forms"]["contents"][0]["regional_variants"]["uk"];
        variant["pronunciations"][0]["synthesis"]["use_spelling"] = true.into();
        variant["spelling_speech"] = json!({"content":{"version":2,"text":"endurance","annotations":[]},"voice_profile":{"voices":[voice("en-GB",true,Some(Uuid::now_v7()))]}});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[1].source,
            Some(GeneratedSpeechSource::Text { .. })
        ));
        assert_eq!(items[1].default_for, vec![AccentGroup::Uk]);
        let variant = &mut value["forms"]["contents"][0]["regional_variants"]["uk"];
        variant["spelling_speech"] = Value::Null;
        variant["pronunciations"][0]["audio_assets"] =
            json!([{"id":Uuid::now_v7(),"locale":"en-GB"}]);
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].asset_id.is_some());
        assert_eq!(items[0].default_for, vec![AccentGroup::Uk]);
    }
}
