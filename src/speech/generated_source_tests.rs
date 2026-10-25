use super::generated_source::{GeneratedSpeechSource, references};
use serde_json::json;
use uuid::Uuid;

#[test]
fn ipa_reference_ignores_neighboring_ups_word_data_and_accepts_absent_profiles() {
    let source = json!({"version":2,"text":"hello","annotations":[]});
    let version = json!({"version_id":Uuid::now_v7(),"version_name":"自然","locale":"en-GB","voice_id":"sonia","enabled":true,"rate_percent":0,"audio_asset_id":Uuid::now_v7()});
    let owner = json!({"id":Uuid::now_v7(),"voice_profile":null,"synthesis":{"uk":{"ipa":"həˈləʊ","ups":"HH AH L OW","ups_words":[{"text":"hello","phoneme":"HH AH L OW"}],"ipa_speech":{"content":source,"voice_profile":{"voices":[version]}}}}});
    let refs = references(&owner).unwrap();
    assert_eq!(refs.len(), 1);
    let request: GeneratedSpeechSource = serde_json::from_value(
        json!({"kind":"phoneme","content":source,"alphabet":"ipa","phoneme":"həˈləʊ"}),
    )
    .unwrap();
    assert_eq!(
        refs[0].source.canonical().unwrap(),
        request.canonical().unwrap()
    );
}
