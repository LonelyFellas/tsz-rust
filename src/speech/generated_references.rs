use serde_json::Value;
use uuid::Uuid;

/// Collect every retained version, including unselected versions, for reclamation protection.
pub(crate) fn asset_references(value: &Value) -> Vec<(Uuid, Vec<Uuid>)> {
    fn walk(value: &Value, node: Uuid, out: &mut Vec<(Uuid, Vec<Uuid>)>) {
        let node = value
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .unwrap_or(node);
        if let Some(voices) = value
            .get("voice_profile")
            .and_then(|profile| profile.get("voices"))
            .and_then(Value::as_array)
        {
            for voice in voices {
                if let Some(asset) = voice
                    .get("audio_asset_id")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
                {
                    let version = voice
                        .get("version_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .unwrap_or(node);
                    out.push((version, vec![asset]));
                }
            }
        }
        match value {
            Value::Object(fields) => {
                for (key, child) in fields {
                    if key != "voice_profile" {
                        walk(child, node, out);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, node, out);
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    walk(value, Uuid::nil(), &mut out);
    out
}
