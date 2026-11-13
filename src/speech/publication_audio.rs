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
    /// Some owners encode their dialect as a field rather than a path segment.
    #[serde(skip)]
    pub source_dialect: Option<crate::lexicon::dto::Dialect>,
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

pub(crate) fn collect(value: &Value) -> Result<Vec<AudioRequirement>, AppError> {
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
    let source_dialect = match owner["dialect"].as_str() {
        Some("uk") => Some(crate::lexicon::dto::Dialect::Uk),
        Some("us") => Some(crate::lexicon::dto::Dialect::Us),
        Some("common") => Some(crate::lexicon::dto::Dialect::Common),
        _ => None,
    };
    let accents = super::dubbing::active_accents(path, owner["dialect"].as_str());
    let in_scope = |group| match group {
        Some(AccentGroup::Uk) => accents.contains(&"gb"),
        Some(AccentGroup::Us) => accents.contains(&"us"),
        _ => true,
    };
    let assets: Vec<_> = owner["audio_assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|asset| {
            in_scope(
                asset["locale"]
                    .as_str()
                    .and_then(super::locale::EnglishLocale::parse)
                    .map(|locale| locale.accent_group()),
            )
        })
        .collect();
    let recorded = |group| {
        assets.iter().any(|asset| {
            asset["locale"]
                .as_str()
                .and_then(super::locale::EnglishLocale::parse)
                .is_none_or(|locale| Some(locale.accent_group()) == group)
        })
    };
    let voice_accents = accents
        .iter()
        .copied()
        .filter(|accent| {
            !recorded(Some(if *accent == "gb" {
                AccentGroup::Uk
            } else {
                AccentGroup::Us
            }))
        })
        .collect::<Vec<_>>();
    // Review and capability checks must not resolve inheritance on a recorded side.
    let voice_dialect = match voice_accents.as_slice() {
        ["gb"] if !assets.is_empty() => Some(crate::lexicon::dto::Dialect::Uk),
        ["us"] if !assets.is_empty() => Some(crate::lexicon::dto::Dialect::Us),
        _ => source_dialect,
    };
    if let Some(voices) = owner
        .pointer("/voice_profile/voices")
        .and_then(Value::as_array)
        .filter(|_| !voice_accents.is_empty())
    {
        let profile: VoiceProfileV3 =
            serde_json::from_value(owner["voice_profile"].clone()).map_err(AppError::internal)?;
        let profile = Arc::new(profile);
        for accent in voice_accents {
            if profile.dubbing.is_none()
                || profile.voices.iter().any(|voice| {
                    voice.enabled
                        && voice.accent_group().is_some_and(|group| {
                            group
                                == if accent == "gb" {
                                    AccentGroup::Uk
                                } else {
                                    AccentGroup::Us
                                }
                        })
                })
            {
                continue;
            }
            out.push(AudioRequirement {
                target_id: target,
                content_id: content,
                source_path: path.into(),
                source_dialect: voice_dialect,
                default_for: vec![if accent == "gb" {
                    AccentGroup::Uk
                } else {
                    AccentGroup::Us
                }],
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
            if !in_scope(setting.accent_group()) || recorded(setting.accent_group()) {
                continue;
            }
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
                source_dialect: voice_dialect,
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
    for asset in assets {
        if let Some(asset_id) = id(&asset["id"]) {
            out.push(AudioRequirement {
                target_id: target,
                content_id: content,
                source_path: path.into(),
                source_dialect,
                default_for: vec![],
                setting: None,
                profile: None,
                asset_id: Some(asset_id),
                source: None,
                issue: None,
            });
        }
    }
    Ok(())
}

fn add_side_selected(
    root: &Value,
    owner: &Value,
    path: &str,
    identity: (Uuid, Option<Uuid>),
    side: &str,
    upload: bool,
    out: &mut Vec<AudioRequirement>,
) -> Result<(), AppError> {
    let (target, content) = identity;
    let mut selected = owner.clone();
    selected["dialect"] = side.into();
    let locale = if side == "uk" { "en-GB" } else { "en-US" };
    if upload {
        selected
            .as_object_mut()
            .expect("source owner")
            .remove("voice_profile");
        if let Some(assets) = selected["audio_assets"].as_array_mut() {
            let choice = assets
                .iter()
                .any(|asset| asset["locale"] == locale && asset["is_default"].is_boolean());
            assets.retain(|asset| {
                asset["locale"] == locale && (!choice || asset["is_default"] == true)
            });
        }
    } else {
        selected
            .as_object_mut()
            .expect("source owner")
            .remove("audio_assets");
        if let Some(voices) = selected
            .pointer_mut("/voice_profile/voices")
            .and_then(Value::as_array_mut)
        {
            voices.retain(|voice| {
                serde_json::from_value::<VoiceSettingV3>(voice.clone())
                    .ok()
                    .and_then(|setting| setting.accent_group())
                    == Some(if side == "uk" {
                        AccentGroup::Uk
                    } else {
                        AccentGroup::Us
                    })
            });
        }
    }
    add_selected(root, &selected, path, target, content, out)
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
    let explicit_sources = value["pronunciations"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            ["uk", "us"]
                .iter()
                .any(|side| item["synthesis"][side]["source"].is_string())
        })
    });
    if !explicit_sources && let Some(spelling) = value.get("spelling_speech") {
        add_selected(root, spelling, &spelling_path, variant_id, content, out)?;
    }
    let accents: &[(&str, AccentGroup)] = match path
        .rsplit('/')
        .find(|part| matches!(*part, "common" | "uk" | "us"))
    {
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
        let synthesis = &pronunciation["synthesis"];
        if ["uk", "us"]
            .iter()
            .any(|side| synthesis[side]["source"].is_string())
        {
            for (side, accent) in accents {
                let source = synthesis[side]["source"].as_str().unwrap_or_else(|| {
                    if synthesis["use_spelling"] == true {
                        "spelling"
                    } else {
                        synthesis["alphabet"].as_str().unwrap_or("ipa")
                    }
                });
                if source == "none" {
                    continue;
                }
                let start = out.len();
                match source {
                    "spelling" => {
                        if let Some(spelling) = value.get("spelling_speech") {
                            add_side_selected(
                                root,
                                spelling,
                                &spelling_path,
                                (target, content),
                                side,
                                false,
                                out,
                            )?;
                        }
                    }
                    "ipa" | "ups" => {
                        let field = format!("{source}_speech");
                        if let Some(speech) = synthesis
                            .get(*side)
                            .and_then(|candidate| candidate.get(&field))
                        {
                            add_side_selected(
                                root,
                                speech,
                                &format!("{pron_path}/synthesis/{side}/{field}"),
                                (target, content),
                                side,
                                false,
                                out,
                            )?;
                        }
                    }
                    "upload" => add_side_selected(
                        root,
                        pronunciation,
                        &pron_path,
                        (target, content),
                        side,
                        true,
                        out,
                    )?,
                    _ => {}
                }
                if let Some(index) = (start..out.len()).min_by_key(|index| {
                    let item = &out[*index];
                    !root
                        .pointer(&item.source_path)
                        .and_then(|owner| owner["audio_assets"].as_array())
                        .is_some_and(|assets| {
                            assets.iter().any(|asset| {
                                id(&asset["id"]) == item.asset_id && asset["is_default"] == true
                            })
                        })
                }) {
                    if !out[index].default_for.contains(accent) {
                        out[index].default_for.push(*accent);
                    }
                } else {
                    out.push(AudioRequirement {
                        target_id: target,
                        content_id: content,
                        source_path: pron_path.clone(),
                        source_dialect: Some(if *side == "uk" {
                            crate::lexicon::dto::Dialect::Uk
                        } else {
                            crate::lexicon::dto::Dialect::Us
                        }),
                        default_for: vec![*accent],
                        setting: None,
                        profile: None,
                        asset_id: None,
                        source: None,
                        issue: Some(RequirementIssue::MissingConfiguration),
                    });
                }
            }
            continue;
        }
        let start = out.len();
        let synthesis = &pronunciation["synthesis"];
        let selected_recording = |item: &AudioRequirement| {
            root.pointer(&item.source_path)
                .and_then(|owner| owner["audio_assets"].as_array())
                .is_some_and(|assets| {
                    assets.iter().any(|asset| {
                        id(&asset["id"]) == item.asset_id && asset["is_default"] == true
                    })
                })
        };
        let item_accent = |item: &AudioRequirement| {
            item.setting
                .as_ref()
                .and_then(VoiceSettingV3::accent_group)
                .or_else(|| {
                    root.pointer(&item.source_path)
                        .and_then(|owner| owner["audio_assets"].as_array())
                        .and_then(|assets| {
                            assets
                                .iter()
                                .find(|asset| id(&asset["id"]) == item.asset_id)
                        })
                        .and_then(|asset| asset["locale"].as_str())
                        .and_then(super::locale::EnglishLocale::parse)
                        .map(|locale| locale.accent_group())
                })
        };
        let recording_choice_for = |accent: AccentGroup| {
            pronunciation["audio_assets"]
                .as_array()
                .is_some_and(|assets| {
                    assets.iter().any(|asset| {
                        asset["is_default"].is_boolean()
                            && asset["locale"]
                                .as_str()
                                .and_then(super::locale::EnglishLocale::parse)
                                .is_some_and(|locale| locale.accent_group() == accent)
                    })
                })
        };
        let recording_for = |accent: AccentGroup| {
            pronunciation["audio_assets"]
                .as_array()
                .is_some_and(|assets| {
                    assets.iter().any(|asset| {
                        asset["is_default"] == true
                            && asset["locale"]
                                .as_str()
                                .and_then(super::locale::EnglishLocale::parse)
                                .is_some_and(|locale| locale.accent_group() == accent)
                    })
                })
        };
        let recording_for_all = accents.iter().all(|(_, accent)| recording_for(*accent));
        let mut raw = Vec::new();
        add_selected(root, pronunciation, &pron_path, target, content, &mut raw)?;
        raw.retain(|item| {
            if item.profile.is_some() && (synthesis.is_object() || recording_for_all) {
                return false;
            }
            if item
                .setting
                .as_ref()
                .and_then(|setting| setting.locale)
                .is_some_and(|locale| recording_for(locale.accent_group()))
            {
                return false;
            }
            item.setting.is_some()
                || item_accent(item).is_none_or(|accent| !recording_choice_for(accent))
                || selected_recording(item)
        });
        out.extend(raw);
        let raw_end = out.len();
        let mut active = Vec::new();
        let active_start = if synthesis.is_object() {
            raw_end
        } else {
            start
        };
        if synthesis["use_spelling"] == true {
            if let Some(spelling) = value.get("spelling_speech")
                && !recording_for_all
            {
                add_selected(root, spelling, &spelling_path, target, content, &mut active)?;
            }
        } else {
            let field = match synthesis["alphabet"].as_str() {
                Some("ipa") => Some("ipa_speech"),
                Some("ups") => Some("ups_speech"),
                _ => None,
            };
            if let Some(field) = field {
                for (side, accent) in accents {
                    if recording_for(*accent) {
                        continue;
                    }
                    if let Some(speech) = synthesis.get(*side).and_then(|s| s.get(field)) {
                        let speech_path = format!("{pron_path}/synthesis/{side}/{field}");
                        add_selected(root, speech, &speech_path, target, content, &mut active)?;
                    }
                }
            }
        }
        active.retain(|item| !item_accent(item).is_some_and(recording_for));
        out.extend(active);
        for (_, accent) in accents {
            let candidates = (active_start..out.len()).chain((start..raw_end).filter(|index| {
                synthesis.is_object()
                    && out[*index].setting.is_none()
                    && out[*index].asset_id.is_some()
            }));
            let selected = candidates
                .into_iter()
                .filter(|index| {
                    let item = &out[*index];
                    if let Some(setting) = &item.setting {
                        setting.accent_group() == Some(*accent)
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
                })
                .min_by_key(|index| {
                    let item = &out[*index];
                    (!selected_recording(item), item.source_path != pron_path)
                });
            if let Some(index) = selected {
                out[index].default_for.push(*accent);
            } else {
                out.push(AudioRequirement {
                    target_id: target,
                    content_id: content,
                    source_path: pron_path.clone(),
                    source_dialect: None,
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
    fn explicit_sources_are_independent_and_paused_caches_do_not_require_audio() {
        let mut value = fixture("common");
        let path = "/forms/contents/0/regional_variants/common/pronunciations/0";
        value.pointer_mut(path).unwrap()["synthesis"]["uk"]["source"] = json!("none");
        let mut us = value.pointer(path).unwrap()["synthesis"]["uk"].clone();
        us["source"] = json!("ipa");
        us["ipa_speech"]["voice_profile"]["voices"][0]["locale"] = json!("en-US");
        value.pointer_mut(path).unwrap()["synthesis"]["us"] = us;
        let items = collect(&value).unwrap();
        assert!(!items.is_empty());
        assert!(
            items
                .iter()
                .all(|item| !item.default_for.contains(&AccentGroup::Uk))
        );
        assert!(items.iter().all(|item| item.source_path.contains("/us/")));
        let cached = value.pointer(path).unwrap()["synthesis"]["uk"].clone();
        let recording = Uuid::now_v7();
        value.pointer_mut(path).unwrap()["audio_assets"] =
            json!([{"id":recording,"locale":"en-US","is_default":true}]);
        value.pointer_mut(path).unwrap()["synthesis"]["us"]["source"] = json!("upload");
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].asset_id, Some(recording));
        assert!(items[0].setting.is_none());
        assert_eq!(items[0].default_for, vec![AccentGroup::Us]);
        value.pointer_mut(path).unwrap()["audio_assets"][0]["is_default"] = json!(false);
        let cancelled = collect(&value).unwrap();
        assert_eq!(cancelled.len(), 1);
        assert!(cancelled[0].asset_id.is_none());
        assert!(matches!(
            cancelled[0].issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        value.pointer_mut(path).unwrap()["audio_assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("is_default");
        assert_eq!(
            collect(&value).unwrap()[0].asset_id,
            Some(recording),
            "omitted legacy choice keeps its fallback"
        );

        assert_eq!(value.pointer(path).unwrap()["synthesis"]["uk"], cached);
    }

    #[test]
    fn selected_recording_overrides_generated_default_for_its_accent() {
        let mut value = fixture("common");
        let recording = Uuid::now_v7();
        value["forms"]["contents"][0]["regional_variants"]["common"]["pronunciations"][0]["audio_assets"] =
            json!([{"id":recording,"locale":"en-GB","is_default":true}]);
        let items = collect(&value).unwrap();
        assert_eq!(
            items
                .iter()
                .find(|item| item.default_for.contains(&AccentGroup::Uk))
                .unwrap()
                .asset_id,
            Some(recording)
        );
        assert!(!items.iter().any(|item| item.setting.is_some()));
    }
    #[test]
    fn pronunciation_recording_overrides_spelling_and_clearing_restores_active_source() {
        let mut value = fixture("common");
        let variant = &mut value["forms"]["contents"][0]["regional_variants"]["common"];
        let target = id(&variant["pronunciations"][0]["id"]).unwrap();
        let spelling = Uuid::now_v7();
        let recording = Uuid::now_v7();
        variant["spelling_speech"] = json!({
            "content":{"version":2,"text":"endurance","annotations":[]},
            "voice_profile":{"voices":[]},
            "audio_assets":[{"id":spelling,"locale":"en-GB","is_default":true}]
        });
        variant["pronunciations"][0]["synthesis"]["use_spelling"] = json!(true);
        variant["pronunciations"][0]["audio_assets"] =
            json!([{"id":recording,"locale":"en-GB","is_default":true}]);
        let selected = |value: &Value| {
            collect(value)
                .unwrap()
                .into_iter()
                .find(|item| {
                    item.target_id == target && item.default_for.contains(&AccentGroup::Uk)
                })
                .unwrap()
        };
        assert_eq!(selected(&value).asset_id, Some(recording));
        value["forms"]["contents"][0]["regional_variants"]["common"]["pronunciations"][0]["audio_assets"]
            [0]["is_default"] = json!(false);
        assert_eq!(selected(&value).asset_id, Some(spelling));
        value["forms"]["contents"][0]["regional_variants"]["common"]["spelling_speech"]["audio_assets"] =
            json!([]);
        assert!(matches!(
            selected(&value).issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        value["forms"]["contents"][0]["regional_variants"]["common"]["pronunciations"][0]["audio_assets"][0].as_object_mut().unwrap().remove("is_default");
        assert_eq!(selected(&value).asset_id, Some(recording));
    }

    #[test]
    fn independent_spelling_arrays_keep_every_audio_default_on_its_own_accent() {
        let mut value = fixture("uk");
        let uk = value["forms"]["contents"][0]["regional_variants"]["uk"].clone();
        let mut us = uk.clone();
        us["pronunciations"][0]["synthesis"]["us"] =
            us["pronunciations"][0]["synthesis"]["uk"].clone();
        us["pronunciations"][0]["synthesis"]["us"]["ipa_speech"]["voice_profile"]["voices"] =
            json!([voice("en-US", true, Some(Uuid::now_v7()))]);
        us["pronunciations"][0]["synthesis"]
            .as_object_mut()
            .unwrap()
            .remove("uk");
        value["forms"]["contents"][0]["regional_variants"] =
            json!({"mode":"uk_us_many","uk":[uk.clone(),uk],"us":[us.clone(),us.clone(),us]});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 5);
        for item in items {
            assert!(item.issue.is_none());
            assert!(item.asset_id.is_some());
            assert!(item.source.is_some());
            assert_eq!(
                item.default_for,
                if item.source_path.contains("/uk/") {
                    vec![AccentGroup::Uk]
                } else {
                    vec![AccentGroup::Us]
                }
            );
        }
    }

    #[test]
    fn grammar_owner_dialect_is_retained_internally_not_in_the_wire_manifest() {
        let value = json!({"meanings":{"id":Uuid::now_v7(),"dialect":"uk","content":{"version":2,"text":"grammar","annotations":[]},
            "voice_profile":{"voices":[voice("en-GB",true,Some(Uuid::now_v7()))]}}});
        let items = collect(&value).unwrap();
        assert_eq!(
            items[0].source_dialect,
            Some(crate::lexicon::dto::Dialect::Uk)
        );
        assert!(
            serde_json::to_value(&items[0])
                .unwrap()
                .get("source_dialect")
                .is_none()
        );
    }

    #[test]
    fn split_grammar_ignores_other_side_voice_and_recording_caches() {
        let uk_audio = Uuid::now_v7();
        let us_audio = Uuid::now_v7();
        for assets in [json!([]), json!([{"id":us_audio,"locale":"en-US"}])] {
            let value = json!({"meanings":{"id":Uuid::now_v7(),"dialect":"uk",
                "content":{"version":2,"text":"a safe harbour","annotations":[]},
                "voice_profile":{"voices":[voice("en-GB",true,Some(uk_audio)),voice("en-US",true,None)],
                    "dubbing":{"area":"grammar","uk":{"mode":"local"},"us":{"mode":"inherit"}}},
                "audio_assets":assets}});
            let before = value.clone();
            let items = collect(&value).unwrap();
            assert_eq!(
                items.len(),
                1,
                "inactive side caches are not publication requirements"
            );
            assert_eq!(items[0].asset_id, Some(uk_audio));
            assert!(items[0].issue.is_none());
            assert_eq!(value, before, "collection must preserve both caches");
        }
    }

    #[test]
    fn common_text_recording_does_not_hide_the_other_accents_missing_audio() {
        let recording = Uuid::now_v7();
        let generated = Uuid::now_v7();
        for selected in [false, true] {
            let mut value = json!({"mode":"unified","common":{
                "id":Uuid::now_v7(),"origin":"manual",
                "value":{"version":2,"text":"Hello","annotations":[]},
                "voice_profile":{"voices":[voice("en-GB",true,None)],
                    "dubbing":{"area":"example","uk":{"mode":"inherit"},"us":{"mode":"local"}}},
                "audio_assets":[{"id":recording,"locale":"en-GB","gender":"female",
                    "content_type":"audio/mpeg","size_bytes":3,"original_name":"recording.mp3",
                    "created_at":"2026-11-13T01:00:00Z"}]
            }});
            if selected {
                value["common"]["voice_profile"]["voices"]
                    .as_array_mut()
                    .unwrap()
                    .push(voice("en-US", true, None));
            }
            let requirements = |value: &Value| {
                sentence_requirements(&serde_json::from_value(value.clone()).unwrap()).unwrap()
            };
            let items = requirements(&value);
            assert_eq!(items.len(), 2, "one recording cannot cover both accents");
            let missing = items.iter().find(|item| item.issue.is_some()).unwrap();
            assert!(if selected {
                matches!(missing.issue, Some(RequirementIssue::MissingAsset))
            } else {
                matches!(missing.issue, Some(RequirementIssue::MissingConfiguration))
            });
            assert_eq!(
                missing.source_dialect,
                Some(crate::lexicon::dto::Dialect::Us)
            );
            assert!(items.iter().any(|item| item.asset_id == Some(recording)));

            value["common"]["voice_profile"]["voices"] = json!([
                voice("en-GB", true, None),
                voice("en-US", true, Some(generated))
            ]);
            let ready = requirements(&value);
            assert_eq!(ready.len(), 2);
            assert!(ready.iter().all(|item| item.issue.is_none()));
            assert!(ready.iter().any(|item| item.asset_id == Some(generated)));
        }
    }

    #[test]
    fn explicitly_inherited_empty_optional_profile_is_not_silently_ready() {
        let value = json!({"meanings":{"id":Uuid::now_v7(),"content":{"version":2,"text":"meaning","annotations":[]},
            "voice_profile":{"voices":[],"dubbing":{"area":"word","uk":{"mode":"inherit"},"us":{"mode":"inherit"}}}}});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0].issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        assert!(items[0].asset_id.is_none());
    }

    #[test]
    fn new_local_empty_pool_requires_configuration_without_changing_legacy_empty() {
        let value = json!({"en_text":{"common":{"id":Uuid::now_v7(),"value":{"version":2,"text":"Hello","annotations":[]},"voice_profile":{"voices":[],"dubbing":{"area":"meaning","uk":{"mode":"local"},"us":{"mode":"local"}}}}}});
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| matches!(item.issue, Some(RequirementIssue::MissingConfiguration)))
        );
    }

    #[test]
    fn ordered_selected_pool_ignores_cached_voices_and_changes_review_binding() {
        let version = |alias: &str, locale: &str, enabled: bool, asset| {
            let mut setting = voice(locale, enabled, asset);
            setting["voice_id"] = json!(alias);
            setting["version_id"] = json!(Uuid::now_v7());
            setting["version_name"] = json!(alias);
            setting
        };
        let first = version("en-us-jennyneural", "en-US", true, Some(Uuid::now_v7()));
        let second = version("en-us-guyneural", "en-US", true, Some(Uuid::now_v7()));
        let cached = version("en-gb-sonianeural", "en-GB", false, None);
        let mut value = json!({"en_text":{"common":{"id":Uuid::now_v7(),"value":{"version":2,"text":"Hello","annotations":[]},
            "voice_profile":{"voices":[first, cached, second],"dubbing":{"area":"meaning","uk":{"mode":"local"},"us":{"mode":"local"}}}}}});
        let all = collect(&value).unwrap();
        assert!(
            all.iter()
                .any(|item| item.default_for == vec![AccentGroup::Uk]
                    && matches!(item.issue, Some(RequirementIssue::MissingConfiguration)))
        );
        let before = all
            .into_iter()
            .filter(|item| item.setting.is_some())
            .collect::<Vec<_>>();
        assert_eq!(before.len(), 2);
        assert_eq!(
            before[0].asset_id,
            serde_json::from_value(first["audio_asset_id"].clone()).unwrap()
        );
        value["en_text"]["common"]["voice_profile"]["voices"]
            .as_array_mut()
            .unwrap()
            .swap(0, 2);
        let after = collect(&value)
            .unwrap()
            .into_iter()
            .filter(|item| item.setting.is_some())
            .collect::<Vec<_>>();
        assert_eq!(after[0].asset_id, before[1].asset_id);
        assert_ne!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap()
        );
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
        value["forms"]["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"]
            [0]["is_default"] = json!(false);
        let items = collect(&value).unwrap();
        assert!(matches!(
            items[0].issue,
            Some(RequirementIssue::MissingConfiguration)
        ));
        value["forms"]["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"]
            [0]["is_default"] = json!(true);
        let items = collect(&value).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].asset_id.is_some());
        assert_eq!(items[0].default_for, vec![AccentGroup::Uk]);
    }
}
