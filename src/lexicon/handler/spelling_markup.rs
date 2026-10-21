use axum::{
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use serde::Serialize;

use crate::lexicon::dto::{DraftFormsStepContentV3, WordRegionalVariantsV3};

const HEADER: &str = "x-tsz-spelling-markup";
const SPEECH_HEADER: &str = "x-tsz-spelling-speech";

/// Only project the owned response after the service has preserved canonical data.
pub(super) fn project_spelling_markup(headers: &HeaderMap, forms: &mut DraftFormsStepContentV3) {
    let capability = headers.get(HEADER);
    let markup = capability.is_some_and(|value| value == "v1");
    let speech = headers
        .get(SPEECH_HEADER)
        .is_some_and(|value| value == "v1");
    let phoneme_speech = headers
        .get("x-tsz-phoneme-speech")
        .is_some_and(|value| value == "v1");
    for form in forms.pos.iter_mut().flat_map(|pos| &mut pos.forms) {
        if !phoneme_speech {
            let pronunciations = match &mut form.regional_variants {
                WordRegionalVariantsV3::Common { common } => vec![&mut common.pronunciations],
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    vec![&mut uk.pronunciations, &mut us.pronunciations]
                }
            };
            for pronunciation in pronunciations.into_iter().flatten() {
                if let Some(synthesis) = &mut pronunciation.synthesis {
                    for candidate in [&mut synthesis.uk, &mut synthesis.us].into_iter().flatten() {
                        candidate.ipa_speech = None;
                        candidate.ups_speech = None;
                    }
                }
            }
        }
        match &mut form.regional_variants {
            WordRegionalVariantsV3::Common { common } => {
                if !markup {
                    common.spelling_rich = None;
                }
                if !speech {
                    common.spelling_speech = None;
                }
            }
            WordRegionalVariantsV3::UkUs { uk, us } => {
                if !markup {
                    uk.spelling_rich = None;
                    us.spelling_rich = None;
                }
                if !speech {
                    uk.spelling_speech = None;
                    us.spelling_speech = None;
                }
            }
        }
    }
}

pub(super) fn spelling_markup_response<T: Serialize>(
    headers: &HeaderMap,
    status: StatusCode,
    body: T,
) -> Result<Response, crate::error::AppError> {
    let mut value = serde_json::to_value(body).map_err(crate::error::AppError::internal)?;
    super::super::shared_forms::project_response(headers, &mut value)?;
    let mut response = crate::lexicon::sentence_formatting::response(headers, status, value)?;
    response.headers_mut().insert(
        header::VARY,
        axum::http::HeaderValue::from_static(
            "x-tsz-spelling-markup, x-tsz-spelling-speech, x-tsz-phoneme-speech, x-tsz-sentence-formatting, x-tsz-form-contents",
        ),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn phoneme_settings_require_their_own_capability() {
        let speech = serde_json::json!({"content":{"version":2,"text":"cat","annotations":[]},"voice_profile":{"voices":[]}});
        let stored: DraftFormsStepContentV3 = serde_json::from_value(json!({"pos":[{"pos_id":Uuid::now_v7(),"pos":"noun","form_groups":[],"forms":[{"id":Uuid::now_v7(),"form_type":"base","regional_variants":{"mode":"common","common":{"id":Uuid::now_v7(),"dialect":"common","spelling":"cat","origin":"manual","pronunciations":[{"id":Uuid::now_v7(),"dict_phonetic":"kæt","actual_pron":"kæt","synthesis":{"alphabet":"ipa","ipa":"","ups":"","uk":{"ipa":"kæt","ups":"K AE T","ipa_speech":speech,"ups_speech":speech},"us":{"ipa":"kæt","ups":"K AE T","ipa_speech":speech,"ups_speech":speech}}}]}}}]}]})).unwrap();
        let original = stored.clone();
        for value in [None, Some("unknown"), Some("v1")] {
            let mut headers = HeaderMap::new();
            if let Some(value) = value {
                headers.insert("x-tsz-phoneme-speech", value.parse().unwrap());
            }
            let mut projected = stored.clone();
            project_spelling_markup(&headers, &mut projected);
            let json = serde_json::to_value(projected).unwrap();
            for side in ["uk", "us"] {
                for alphabet in ["ipa", "ups"] {
                    assert_eq!(json.pointer(&format!("/pos/0/forms/0/regional_variants/common/pronunciations/0/synthesis/{side}")).unwrap().get(format!("{alphabet}_speech")).is_some(), value == Some("v1"));
                }
            }
            assert_eq!(stored, original);
        }
    }

    #[test]
    fn speech_projection_requires_its_own_capability_and_preserves_storage() {
        let stored: DraftFormsStepContentV3 = serde_json::from_value(json!({"pos":[{
            "pos_id":Uuid::now_v7(),"pos":"verb","form_groups":[],"forms":[{
                "id":Uuid::now_v7(),"form_type":"base","regional_variants":{"mode":"common","common":{
                    "id":Uuid::now_v7(),"dialect":"common","spelling":"give up","origin":"manual","pronunciations":[],
                    "spelling_rich":{"version":2,"text":"give up","annotations":[{"type":"italic","start":0,"end":4}]},
                    "spelling_speech":{"content":{"version":2,"text":"give up","annotations":[{"type":"pause","at":4,"duration_ms":500}]},"voice_profile":{"voices":[]}}
                }}
            }]
        }]})).unwrap();
        let path = "/pos/0/forms/0/regional_variants/common";
        let canonical = serde_json::to_value(&stored).unwrap();
        for capability in [None, Some("unknown"), Some("v1")] {
            let mut headers = HeaderMap::new();
            headers.insert(HEADER, "v1".parse().unwrap());
            if let Some(capability) = capability {
                headers.insert(SPEECH_HEADER, capability.parse().unwrap());
            }
            let mut projected = stored.clone();
            project_spelling_markup(&headers, &mut projected);
            let value = serde_json::to_value(projected).unwrap();
            assert_eq!(
                value
                    .pointer(path)
                    .unwrap()
                    .get("spelling_speech")
                    .is_some(),
                capability == Some("v1")
            );
            assert_eq!(
                value.pointer(path).unwrap()["spelling_rich"],
                canonical.pointer(path).unwrap()["spelling_rich"]
            );
            assert_eq!(serde_json::to_value(&stored).unwrap(), canonical);
        }
    }

    #[test]
    fn response_projection_preserves_canonical_data_for_all_regions() {
        let variant = |dialect| {
            json!({
                "id": Uuid::now_v7(), "dialect": dialect, "spelling": "give", "origin": "manual",
                "pronunciations": [], "spelling_rich": {
                    "version": 2, "text": "give", "annotations": [{"type":"italic","start":0,"end":2}]
                }
            })
        };
        let stored: DraftFormsStepContentV3 = serde_json::from_value(json!({"pos":[{
            "pos_id":Uuid::now_v7(),"pos":"verb","form_groups":[],"forms":[
                {"id":Uuid::now_v7(),"form_type":"base","regional_variants":{"mode":"common","common":variant("common")}},
                {"id":Uuid::now_v7(),"form_type":"plural","regional_variants":{"mode":"uk_us","uk":variant("uk"),"us":variant("us")}}
            ]
        }]})).unwrap();
        let canonical = serde_json::to_value(&stored).unwrap();
        let mut legacy = canonical.clone();
        for path in [
            "/pos/0/forms/0/regional_variants/common",
            "/pos/0/forms/1/regional_variants/uk",
            "/pos/0/forms/1/regional_variants/us",
        ] {
            legacy
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("spelling_rich");
        }
        for capability in [None, Some("v1"), Some("unknown")] {
            let mut headers = HeaderMap::new();
            if let Some(capability) = capability {
                headers.insert(HEADER, capability.parse().unwrap());
            }
            let mut response = stored.clone();
            project_spelling_markup(&headers, &mut response);
            assert_eq!(
                serde_json::to_value(response).unwrap(),
                if capability == Some("v1") {
                    &canonical
                } else {
                    &legacy
                }
                .clone()
            );
            assert_eq!(serde_json::to_value(&stored).unwrap(), canonical);
        }
    }
}
