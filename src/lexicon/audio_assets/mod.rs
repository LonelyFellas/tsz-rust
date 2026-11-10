mod cleanup;
pub mod dto;
pub mod handler;
mod repository;
mod service;

pub use cleanup::{reclaim_once, run_worker};

/// 录音元数据由服务端决定，默认选择属于引用它的正文，不能被元数据回填清掉。
pub(crate) fn hydrate_recordings(
    assets: &mut [dto::AudioAsset],
    canonical: &std::collections::HashMap<uuid::Uuid, dto::AudioAsset>,
) -> Result<(), crate::error::AppError> {
    use crate::error::{AppError, ErrorCode};
    let mut defaults = std::collections::HashSet::new();
    for asset in assets {
        let metadata = canonical.get(&asset.id).ok_or_else(|| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, "录音不存在或不可访问")
        })?;
        *asset = dto::AudioAsset {
            is_default: asset.is_default,
            ..metadata.clone()
        };
        if asset.is_default && !defaults.insert(asset.locale.accent_group()) {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "每种口音只能选择一条默认播放录音",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn asset(locale: &str) -> dto::AudioAsset {
        serde_json::from_value(json!({
            "id": Uuid::now_v7(), "locale": locale, "gender":"female",
            "content_type":"audio/mpeg", "size_bytes":3,"duration_ms":null,
            "original_name":"fixture.mp3", "created_at":"2026-11-10T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn recording_defaults_survive_authoritative_metadata_and_round_trip() {
        let original = asset("en-GB");
        assert!(
            serde_json::to_value(&original)
                .unwrap()
                .get("is_default")
                .is_none()
        );
        let mut selected = original.clone();
        selected.is_default = true;
        selected.locale = dto::AudioAssetLocale::Us;
        selected.original_name = "forged.mp3".into();
        let mut assets = vec![selected];
        hydrate_recordings(
            &mut assets,
            &HashMap::from([(original.id, original.clone())]),
        )
        .unwrap();
        assert_eq!(
            assets[0],
            dto::AudioAsset {
                is_default: true,
                ..original
            }
        );
        let saved = serde_json::to_value(&assets).unwrap();
        assert_eq!(saved[0]["is_default"], true);
        assert_eq!(
            serde_json::from_value::<Vec<dto::AudioAsset>>(saved).unwrap(),
            assets
        );
    }

    #[test]
    fn default_recordings_are_unique_using_authoritative_accents() {
        let uk = asset("en-GB");
        let another = asset("en-GB");
        let us = asset("en-US");
        let canonical = [uk.clone(), another.clone(), us.clone()]
            .into_iter()
            .map(|asset| (asset.id, asset))
            .collect();
        let mut selected = vec![uk.clone(), us];
        selected
            .iter_mut()
            .for_each(|asset| asset.is_default = true);
        hydrate_recordings(&mut selected, &canonical).unwrap();
        let mut duplicate = another;
        duplicate.is_default = true;
        duplicate.locale = dto::AudioAssetLocale::Us;
        assert!(hydrate_recordings(&mut [selected[0].clone(), duplicate], &canonical).is_err());
    }
}
