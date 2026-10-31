use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{
    SpeechOptions, Voice,
    library::dto::{DubbingGroup, DubbingRecommendation},
};
use crate::{
    error::AppError,
    lexicon::dto::{DubbingAreaV3, VoiceProfileModeV3, VoiceProfileV3, VoiceSettingV3},
};

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveDubbingRequest {
    pub profile: VoiceProfileV3,
    pub source: super::generated_source::GeneratedSpeechSource,
    pub accent: super::locale::VoiceAccent,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ResolveDubbingResponse {
    pub voices: Vec<VoiceSettingV3>,
}

#[utoipa::path(post,path="/api/v1/admin/speech/resolve",tag="admin-speech",security(("bearer_auth"=[])),request_body=ResolveDubbingRequest,
    responses((status=200,body=ResolveDubbingResponse),(status=400),(status=401),(status=403),(status=422)))]
pub async fn resolve(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
    crate::api::ApiJson(request): crate::api::ApiJson<ResolveDubbingRequest>,
) -> Result<axum::Json<ResolveDubbingResponse>, AppError> {
    crate::admin::authorization::require_active_admin(&state, &auth).await?;
    if request.accent == super::locale::VoiceAccent::None {
        return Err(unavailable());
    }
    let canonical = request.source.canonical()?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let permission = crate::admin::permissions::lock(&mut tx, auth.subject).await?;
    if canonical["form_links"]
        .as_array()
        .is_some_and(|links| !links.is_empty())
    {
        permission.require("words.access")?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
    let source_locale = if request.accent == super::locale::VoiceAccent::Gb {
        "en-GB"
    } else {
        "en-US"
    };
    let resolved = request.source.resolve(&mut conn, source_locale).await?;
    let voices = resolve_in(
        &mut conn,
        &request.profile,
        request.accent.as_str(),
        &resolved,
        matches!(
            request.source,
            super::generated_source::GeneratedSpeechSource::Phoneme { .. }
        )
        .then_some(source_locale),
    )
    .await?;
    for voice in &voices {
        request
            .source
            .resolve(&mut conn, voice.locale.ok_or_else(unavailable)?.as_str())
            .await?;
    }
    Ok(axum::Json(ResolveDubbingResponse { voices }))
}

fn unavailable() -> AppError {
    AppError::bad_request(
        crate::error::ErrorCode::InvalidSpeechPreview,
        "当前口音没有满足声音、参数和读法能力的已选推荐，请明确选择或修复配置",
    )
}

pub fn resolve_profile(
    profile: &VoiceProfileV3,
    groups: &[DubbingGroup],
    accent: &str,
    eligible: impl Fn(&VoiceSettingV3) -> bool,
) -> Vec<VoiceSettingV3> {
    let local = if profile.mode() == Some(VoiceProfileModeV3::Inherit) {
        vec![]
    } else {
        profile
            .voices
            .iter()
            .filter(|voice| voice.enabled && eligible(voice))
            .cloned()
            .collect::<Vec<_>>()
    };
    if !local.is_empty()
        || profile.mode().is_none()
        || (profile.is_provider_pool() && profile.mode() == Some(VoiceProfileModeV3::Local))
    {
        return local;
    }
    let from_group = |group: &DubbingGroup| {
        (if accent == "gb" { &group.uk } else { &group.us })
            .iter()
            .filter(|item| item.selected)
            .map(setting)
            .find(|voice| eligible(voice))
            .into_iter()
            .collect::<Vec<_>>()
    };
    for group in groups.iter().filter(|group| {
        !group.is_global
            && profile
                .area()
                .is_some_and(|area| group.areas.iter().any(|value| value == area.as_str()))
    }) {
        let values = from_group(group);
        if !values.is_empty() {
            return values;
        }
    }
    groups
        .iter()
        .find(|group| group.is_global)
        .map(from_group)
        .unwrap_or_default()
}

fn setting(item: &DubbingRecommendation) -> VoiceSettingV3 {
    VoiceSettingV3 {
        output_format: None,
        audio_asset_id: None,
        version_id: Some(item.id),
        version_name: Some(item.name.clone()),
        locale: None,
        voice_id: item.voice_alias.clone(),
        enabled: true,
        rate_percent: item.parameters.rate_percent,
        pitch_semitones: Some(item.parameters.pitch_semitones),
        volume_percent: item.parameters.volume_percent,
        style: item.parameters.style.clone(),
        style_degree: item
            .parameters
            .style_degree
            .and_then(serde_json::Number::from_f64),
        role: item.parameters.role.clone(),
    }
}

pub async fn resolve_in(
    conn: &mut PgConnection,
    profile: &VoiceProfileV3,
    accent: &str,
    content: &crate::lexicon::dto::RichTextV2,
    native_locale: Option<&str>,
) -> Result<Vec<VoiceSettingV3>, AppError> {
    let groups: Value =
        sqlx::query_scalar("SELECT groups FROM speech.dubbing_groups WHERE singleton")
            .fetch_one(&mut *conn)
            .await
            .map_err(AppError::internal)?;
    let groups: Vec<DubbingGroup> = serde_json::from_value(groups).map_err(AppError::internal)?;
    resolve_in_groups(conn, profile, accent, content, native_locale, &groups).await
}

pub(crate) async fn validate_pool_identities(
    conn: &mut PgConnection,
    profile: &VoiceProfileV3,
) -> Result<(), AppError> {
    if profile.is_provider_pool() {
        let aliases = profile
            .voices
            .iter()
            .map(|setting| setting.voice_id.as_str())
            .collect::<Vec<_>>();
        let rows = sqlx::query(
            "SELECT alias,provider,provider_voice_id FROM speech.voices WHERE alias=ANY($1)",
        )
        .bind(&aliases)
        .fetch_all(&mut *conn)
        .await
        .map_err(AppError::internal)?;
        let mut identities = std::collections::HashSet::new();
        for setting in &profile.voices {
            let identity = rows
                .iter()
                .find(|row| row.get::<String, _>("alias") == setting.voice_id)
                .map(|row| {
                    (
                        row.get::<String, _>("provider"),
                        row.get::<String, _>("provider_voice_id"),
                    )
                })
                .unwrap_or_else(|| ("alias".into(), setting.voice_id.clone()));
            if !identities.insert(identity) {
                return Err(AppError::bad_request(
                    crate::error::ErrorCode::InvalidSpeechPreview,
                    "局部池同一供应商官方音色只能保留一个配置，请明确处理历史冲突",
                ));
            }
        }
    }
    Ok(())
}

/// One catalog snapshot and eligibility rule for recommendation resolution and publication playback.
pub(crate) async fn playback_catalog(
    conn: &mut PgConnection,
) -> Result<Vec<sqlx::postgres::PgRow>, AppError> {
    sqlx::query("SELECT id,alias,provider_voice_id,locale,accent,styles,min_rate_percent,max_rate_percent,min_pitch_semitones,max_pitch_semitones FROM speech.voices WHERE enabled AND provider='azure' ORDER BY alias").fetch_all(&mut *conn).await.map_err(AppError::internal)
}
pub(crate) fn setting_available(
    rows: &[sqlx::postgres::PgRow],
    setting: &VoiceSettingV3,
    accent: &str,
    content: &crate::lexicon::dto::RichTextV2,
    native_locale: Option<&str>,
) -> bool {
    let phoneme = content.annotations.iter().any(|mark| {
        matches!(
            mark,
            crate::lexicon::dto::RichTextAnnotation::Phoneme { .. }
        )
    });
    rows.iter()
        .find(|row| row.get::<String, _>("alias") == setting.voice_id)
        .is_some_and(|row| {
            let locale = row.get::<String, _>("locale");
            let id = row.get::<String, _>("provider_voice_id");
            if row.get::<String, _>("accent") != accent
                || !super::capabilities::synthesis_available(&id, &locale)
                || native_locale.is_some_and(|required| locale != required)
                || (phoneme && locale != if accent == "gb" { "en-GB" } else { "en-US" })
            {
                return false;
            }
            let Ok(styles) = serde_json::from_value::<Vec<String>>(row.get("styles")) else {
                return false;
            };
            let Ok(voice) = Voice::new("azure", id, &locale, styles) else {
                return false;
            };
            let effective = super::generated::effective_setting_with_limits(
                setting,
                &voice,
                (row.get("min_rate_percent"), row.get("max_rate_percent")),
                (
                    row.get("min_pitch_semitones"),
                    row.get("max_pitch_semitones"),
                ),
            );
            let setting = &effective;
            if setting.locale.is_some_and(|value| value.as_str() != locale)
                || setting.rate_percent < row.get("min_rate_percent")
                || setting.rate_percent > row.get("max_rate_percent")
                || i16::from(setting.pitch_semitones.unwrap_or(0)) < row.get("min_pitch_semitones")
                || i16::from(setting.pitch_semitones.unwrap_or(0)) > row.get("max_pitch_semitones")
            {
                return false;
            }
            SpeechOptions::new(
                &voice,
                setting.style.clone(),
                setting.rate_percent,
                setting.pitch_semitones.unwrap_or(0),
                setting.volume_percent.unwrap_or(100),
            )
            .and_then(|options| {
                options.with_expression(
                    &voice,
                    setting
                        .style_degree
                        .as_ref()
                        .and_then(serde_json::Number::as_f64),
                    setting.role.clone(),
                )
            })
            .and_then(|options| super::SynthesisRequest::new(voice, options, content.clone()))
            .is_ok()
        })
}

pub(crate) async fn resolve_in_groups(
    conn: &mut PgConnection,
    profile: &VoiceProfileV3,
    accent: &str,
    content: &crate::lexicon::dto::RichTextV2,
    native_locale: Option<&str>,
    groups: &[DubbingGroup],
) -> Result<Vec<VoiceSettingV3>, AppError> {
    validate_pool_identities(conn, profile).await?;
    let rows = playback_catalog(conn).await?;
    let eligible = |setting: &VoiceSettingV3| {
        setting_available(&rows, setting, accent, content, native_locale)
    };
    let local_selected = profile.mode() != Some(VoiceProfileModeV3::Inherit)
        && profile.voices.iter().any(|setting| {
            setting.enabled
                && rows.iter().any(|row| {
                    row.get::<String, _>("alias") == setting.voice_id
                        && row.get::<String, _>("accent") == accent
                })
        });
    if !profile.is_provider_pool()
        && profile.mode() != Some(VoiceProfileModeV3::Inherit)
        && profile.voices.iter().any(|setting| {
            setting.enabled
                && !rows
                    .iter()
                    .any(|row| row.get::<String, _>("alias") == setting.voice_id)
        })
    {
        return Err(unavailable());
    }
    if !profile.is_provider_pool()
        && local_selected
        && profile.voices.iter().any(|setting| {
            setting.enabled
                && rows.iter().any(|row| {
                    row.get::<String, _>("alias") == setting.voice_id
                        && row.get::<String, _>("accent") == accent
                })
                && !eligible(setting)
        })
    {
        return Err(unavailable());
    }
    let mut resolved = resolve_profile(profile, groups, accent, eligible);
    // Retain the historical local-empty behavior until the explicit pool migration.
    if resolved.is_empty()
        && !profile.is_provider_pool()
        && profile.mode() == Some(VoiceProfileModeV3::Local)
    {
        for row in &rows {
            let candidate = VoiceSettingV3 {
                output_format: None,
                audio_asset_id: None,
                version_id: Some(row.get("id")),
                version_name: Some("默认自然朗读".into()),
                locale: None,
                voice_id: row.get("alias"),
                enabled: true,
                rate_percent: 0,
                pitch_semitones: Some(0),
                volume_percent: None,
                style: None,
                style_degree: None,
                role: None,
            };
            if eligible(&candidate) {
                resolved.push(candidate);
                break;
            }
        }
    }
    for setting in &mut resolved {
        if let Some(row) = rows
            .iter()
            .find(|row| row.get::<String, _>("alias") == setting.voice_id)
        {
            setting.locale = super::locale::EnglishLocale::parse(&row.get::<String, _>("locale"));
            if let Some(stored) = profile
                .voices
                .iter()
                .find(|stored| stored.version_id == setting.version_id)
            {
                setting.output_format = stored.output_format;
            }
        }
    }
    if resolved.is_empty() {
        return Err(unavailable());
    }
    Ok(resolved)
}

pub(crate) async fn validate_setting(
    conn: &mut PgConnection,
    profile: &VoiceProfileV3,
    source: &super::generated_source::GeneratedSpeechSource,
    setting: &VoiceSettingV3,
    current: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<(), AppError> {
    if profile.dubbing.is_none() {
        return Ok(());
    }
    let row = sqlx::query("SELECT accent,provider,provider_voice_id,locale,styles,min_rate_percent,max_rate_percent,min_pitch_semitones,max_pitch_semitones FROM speech.voices WHERE alias=$1 AND enabled")
        .bind(&setting.voice_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(unavailable)?;
    let accent = row.get::<String, _>("accent");
    if accent == "none" {
        return if profile.mode() == Some(VoiceProfileModeV3::Inherit) {
            Err(unavailable())
        } else {
            Ok(())
        };
    }
    let locale = setting.locale.ok_or_else(unavailable)?;
    let content = source
        .resolve_with_forms(conn, locale.as_str(), current, batch)
        .await?;
    let resolved = resolve_in(
        conn,
        profile,
        &accent,
        &content,
        matches!(
            source,
            super::generated_source::GeneratedSpeechSource::Phoneme { .. }
        )
        .then_some(locale.as_str()),
    )
    .await?;
    let voice = Voice::new(
        row.get::<String, _>("provider"),
        row.get::<String, _>("provider_voice_id"),
        row.get::<String, _>("locale"),
        serde_json::from_value::<Vec<String>>(row.get("styles")).map_err(AppError::internal)?,
    )
    .map_err(|_| unavailable())?;
    let effect = |setting: &VoiceSettingV3| {
        let effective = super::generated::effective_setting_with_limits(
            setting,
            &voice,
            (row.get("min_rate_percent"), row.get("max_rate_percent")),
            (
                row.get("min_pitch_semitones"),
                row.get("max_pitch_semitones"),
            ),
        );
        let mut value = super::generated_source::voice_input(&effective);
        value
            .as_object_mut()
            .expect("setting object")
            .remove("version_id");
        value
            .as_object_mut()
            .expect("setting object")
            .remove("output_format");
        value
    };
    if !resolved
        .iter()
        .any(|candidate| effect(candidate) == effect(setting))
    {
        return Err(AppError::conflict(
            crate::error::ErrorCode::RevisionConflict,
            None,
            "有效推荐已变化，请重新解析并生成音频",
        ));
    }
    Ok(())
}

pub fn ensure_direct_phonemes_preserved(previous: &Value, next: &Value) -> Result<(), AppError> {
    fn collect(
        value: &Value,
        path: String,
        derived: bool,
        out: &mut std::collections::BTreeMap<String, Vec<Value>>,
    ) {
        if value.get("version") == Some(&serde_json::json!(2)) && value.get("text").is_some() {
            let marks = value
                .get("annotations")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|mark| mark["type"] == "phoneme")
                        .map(|mark| {
                            let text = value["text"]
                                .as_str()
                                .unwrap_or_default()
                                .chars()
                                .collect::<Vec<_>>();
                            let start = mark["start"].as_u64().unwrap_or(u64::MAX) as usize;
                            let end = mark["end"].as_u64().unwrap_or(u64::MAX) as usize;
                            serde_json::json!([
                                mark,
                                text.get(start..end)
                                    .map(|slice| slice.iter().collect::<String>())
                            ])
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !marks.is_empty() {
                out.insert(path, marks);
            }
            return;
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "synthesis"
                        || derived
                            && fields
                                .get("form_links")
                                .or_else(|| fields.get("reading_links"))
                                .and_then(Value::as_array)
                                .is_some_and(|links| !links.is_empty())
                            && key == "content"
                    {
                        continue;
                    }
                    collect(value, format!("{path}/{key}"), derived, out);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| index.to_string());
                    collect(item, format!("{path}/{id}"), derived, out);
                }
            }
            _ => {}
        }
    }
    let mut old = std::collections::BTreeMap::new();
    let mut new = std::collections::BTreeMap::new();
    collect(previous, String::new(), false, &mut old);
    collect(next, String::new(), true, &mut new);
    if new.iter().any(|(path, marks)| {
        marks
            .iter()
            .any(|mark| !old.get(path).is_some_and(|old| old.contains(mark)))
    }) {
        return Err(AppError::bad_request(
            crate::error::ErrorCode::InvalidSpeechPreview,
            "新增直接音素仅允许试听；请使用结构化读法引用或特殊读法保存",
        ));
    }
    Ok(())
}

pub fn dependencies(value: &Value) -> Vec<(Uuid, String, DubbingAreaV3)> {
    fn walk(
        value: &Value,
        node: Option<Uuid>,
        path: String,
        recorded: bool,
        out: &mut Vec<(Uuid, String, DubbingAreaV3)>,
    ) {
        let recorded = recorded
            || value
                .get("audio_assets")
                .and_then(Value::as_array)
                .is_some_and(|assets| !assets.is_empty());
        let node = value
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .or(node);
        if let (Some(node), Some(profile)) = (node, value.get("voice_profile"))
            && let Ok(profile) = serde_json::from_value::<VoiceProfileV3>(profile.clone())
            && profile.mode() == Some(VoiceProfileModeV3::Inherit)
            && !recorded
            && let Some(area) = profile.area()
        {
            out.push((node, path.clone(), area));
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key != "voice_profile" {
                        walk(value, node, format!("{path}/{key}"), recorded, out);
                    }
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    walk(value, node, format!("{path}/{index}"), recorded, out);
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    walk(value, None, String::new(), false, &mut out);
    out
}

pub async fn replace_dependencies(
    conn: &mut PgConnection,
    kind: &str,
    owner: Uuid,
    revision: i64,
    value: &Value,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM speech.dubbing_dependencies WHERE owner_kind=$1 AND owner_id=$2")
        .bind(kind)
        .bind(owner)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    for (target, path, area) in dependencies(value) {
        sqlx::query("INSERT INTO speech.dubbing_dependencies(owner_kind,owner_id,target_id,target_path,area,content_revision) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING").bind(kind).bind(owner).bind(target).bind(path).bind(area.as_str()).bind(revision).execute(&mut *conn).await.map_err(AppError::internal)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(mode: Option<&str>, voices: Value, order: Vec<Uuid>) -> VoiceProfileV3 {
        let mut value = json!({"voices":voices});
        if let Some(mode) = mode {
            value["dubbing"] = json!({"mode":mode,"area":"word","recommendation_order":order});
        }
        serde_json::from_value(value).unwrap()
    }
    fn recommendation(id: Uuid, name: &str, selected: bool) -> DubbingRecommendation {
        serde_json::from_value(json!({"id":id,"name":name,"voice_alias":name,"selected":selected,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}})).unwrap()
    }
    fn group(global: bool, items: Vec<DubbingRecommendation>) -> DubbingGroup {
        DubbingGroup {
            id: Uuid::now_v7(),
            name: if global { "global" } else { "word" }.into(),
            areas: if global { vec![] } else { vec!["word".into()] },
            uk: items,
            us: vec![],
            revision: 1,
            is_global: global,
        }
    }
    #[test]
    fn explicit_local_selected_precedes_recommendations_and_unselected_never_falls_back() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let groups = vec![
            group(
                false,
                vec![
                    recommendation(first, "first", true),
                    recommendation(second, "second", true),
                ],
            ),
            group(true, vec![recommendation(Uuid::now_v7(), "global", true)]),
        ];
        let inherited = profile(Some("inherit"), json!([]), vec![second, first]);
        assert_eq!(
            resolve_profile(&inherited, &groups, "gb", |_| true)
                .iter()
                .map(|v| v.voice_id.as_str())
                .collect::<Vec<_>>(),
            ["first"]
        );
        let local = profile(
            Some("local"),
            json!([{"voice_id":"local","enabled":true,"rate_percent":-27,"version_id":Uuid::now_v7(),"version_name":"named","locale":"en-GB"}]),
            vec![],
        );
        assert_eq!(
            resolve_profile(&local, &groups, "gb", |_| true)[0].voice_id,
            "local"
        );
        let mut groups = groups;
        groups[0].uk.iter_mut().for_each(|r| r.selected = false);
        assert_eq!(
            resolve_profile(&inherited, &groups, "gb", |_| true)[0].voice_id,
            "global"
        );
        groups[1].uk.iter_mut().for_each(|r| r.selected = false);
        assert!(resolve_profile(&inherited, &groups, "gb", |_| true).is_empty());
        let old = profile(None, json!([]), vec![]);
        assert!(resolve_profile(&old, &groups, "gb", |_| true).is_empty());
    }
    #[test]
    fn priority_skips_empty_invalid_and_unselected_groups_before_global() {
        let first = Uuid::now_v7();
        let later = Uuid::now_v7();
        let inherited = profile(Some("inherit"), json!([]), vec![first]);
        let mut groups = vec![
            group(
                false,
                vec![
                    recommendation(first, "invalid", true),
                    recommendation(Uuid::now_v7(), "unselected", false),
                ],
            ),
            group(false, vec![]),
            group(
                false,
                vec![
                    recommendation(later, "later", true),
                    recommendation(Uuid::now_v7(), "other", true),
                ],
            ),
            group(true, vec![recommendation(Uuid::now_v7(), "global", true)]),
        ];
        let eligible = |voice: &VoiceSettingV3| voice.voice_id != "invalid";
        let voices = resolve_profile(&inherited, &groups, "gb", eligible);
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].version_id, Some(later));
        groups[2].uk[0].selected = false;
        groups[2].uk[1].selected = false;
        assert_eq!(
            resolve_profile(&inherited, &groups, "gb", eligible)[0].voice_id,
            "global"
        );
        groups[3].uk.clear();
        assert!(resolve_profile(&inherited, &groups, "gb", eligible).is_empty());
    }

    #[test]
    fn provider_pool_empty_or_invalid_never_falls_back_and_disabled_cache_is_not_selected() {
        let mut local = profile(Some("local"), json!([]), vec![]);
        local.dubbing.as_mut().unwrap().pool_version =
            Some(crate::lexicon::dto::VoicePoolVersionV3::ProviderIdV1);
        let groups = vec![group(
            true,
            vec![recommendation(Uuid::now_v7(), "global", true)],
        )];
        assert!(resolve_profile(&local, &groups, "gb", |_| true).is_empty());
        local.voices = vec![
            setting(&recommendation(Uuid::now_v7(), "a", true)),
            setting(&recommendation(Uuid::now_v7(), "b", true)),
            setting(&recommendation(Uuid::now_v7(), "cache", true)),
        ];
        local.voices[2].enabled = false;
        assert_eq!(
            resolve_profile(&local, &groups, "gb", |v| v.voice_id != "a")
                .iter()
                .map(|v| v.voice_id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        assert!(resolve_profile(&local, &groups, "gb", |_| false).is_empty());
        local.dubbing.as_mut().unwrap().pool_version = None;
        local.voices.clear();
        assert_eq!(
            resolve_profile(&local, &groups, "gb", |_| true)[0].voice_id,
            "global"
        );
    }

    #[test]
    fn dependencies_only_include_explicit_inheritance_and_exclude_uploaded_audio() {
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let value = json!([{"id":id,"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"word"}}},{"id":other,"voice_profile":{"voices":[]}},{"id":Uuid::now_v7(),"audio_assets":[{"id":Uuid::now_v7()}],"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"meaning"}}}]);
        assert_eq!(
            dependencies(&value),
            vec![(id, "/0".into(), DubbingAreaV3::Word)]
        );
    }
    #[test]
    fn same_pronunciation_ipa_ups_and_accents_have_distinct_dependency_paths() {
        let id = Uuid::now_v7();
        let speech =
            json!({"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"azure"}}});
        let value = json!({"id":id,"synthesis":{"uk":{"ipa_speech":speech,"ups_speech":speech},"us":{"ipa_speech":speech}}});
        assert_eq!(
            dependencies(&value),
            vec![
                (id, "/synthesis/uk/ipa_speech".into(), DubbingAreaV3::Azure),
                (id, "/synthesis/uk/ups_speech".into(), DubbingAreaV3::Azure),
                (id, "/synthesis/us/ipa_speech".into(), DubbingAreaV3::Azure),
            ]
        );
    }
    #[test]
    fn pronunciation_recordings_exclude_nested_synthesis_but_not_sibling_spelling() {
        let id = Uuid::now_v7();
        let speech =
            json!({"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"azure"}}});
        let value = json!({"id":id,"spelling_speech":{"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"word"}}},"pronunciations":[{"id":Uuid::now_v7(),"audio_assets":[{"id":Uuid::now_v7()}],"synthesis":{"uk":{"ipa_speech":speech,"ups_speech":speech}}}]});
        assert_eq!(
            dependencies(&value),
            vec![(id, "/spelling_speech".into(), DubbingAreaV3::Word)]
        );
    }
    #[test]
    fn direct_phoneme_guard_preserves_history_and_normal_synthesis_configuration() {
        let mark = json!({"type":"phoneme","start":0,"end":3,"alphabet":"ipa","phoneme":"kæt"});
        let old =
            json!({"id":Uuid::now_v7(),"value":{"version":2,"text":"cat","annotations":[mark]}});
        assert!(ensure_direct_phonemes_preserved(&old, &old).is_ok());
        assert!(ensure_direct_phonemes_preserved(&json!({}), &old).is_err());
        let mut changed = old.clone();
        changed["value"]["text"] = json!("dog");
        assert!(ensure_direct_phonemes_preserved(&old, &changed).is_err());
        let structured = json!({"synthesis":{"uk":{"ipa_speech":{"content":{"version":2,"text":"cat","annotations":[mark]}}}}});
        assert!(ensure_direct_phonemes_preserved(&json!({}), &structured).is_ok());
    }
}
