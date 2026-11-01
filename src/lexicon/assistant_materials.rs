//! Read-only materials for the assistant, using the same pure checks as publication.
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use uuid::Uuid;

use super::unavailable;
use crate::{
    error::AppError,
    lexicon::{
        dto::{DraftFormsStepContentV3, DraftMeaningsStepContentV3, WordCreationStep},
        repository::LexiconRepository,
        validation,
    },
    state::AppState,
};

pub(super) struct Draft {
    pub forms: DraftFormsStepContentV3,
    pub meanings: DraftMeaningsStepContentV3,
}

pub(super) fn parse_draft(forms: &Value, meanings: &Value) -> Option<Draft> {
    Some(Draft {
        forms: serde_json::from_value(forms.clone()).ok()?,
        meanings: serde_json::from_value(meanings.clone()).ok()?,
    })
}

pub(super) fn check(
    entry_id: Uuid,
    draft: &Draft,
    sub_part_parents: &HashMap<String, String>,
) -> Value {
    let issues = validation::validate_publish_content(
        entry_id,
        &draft.forms,
        &draft.meanings,
        sub_part_parents,
    );
    let locations = locations(&draft.forms, &draft.meanings);
    let listed: Vec<Value> = issues.iter().take(30).map(|issue| {
        let forms_pos=draft.forms.pos.iter().position(|pos|pos.pos_id==issue.node_id);
        let forms_location=if serde_json::to_value(issue.step).ok()==Some(json!("forms")){forms_pos.map(|index|(format!("forms.pos[{index}]"),pos_label(&draft.forms.pos[index].pos).to_owned()))}else{None};
        let location = forms_location.as_ref().or_else(||locations.get(&issue.node_id.to_string()));
        let readable=location.map(|(path,label)|json!({"path":format!("{path}.{}",issue.field),"label":format!("{label} · {}",field_label(&issue.field))})).unwrap_or_else(||{
            let step=serde_json::to_value(issue.step).unwrap_or(Value::Null);
            json!({"path":format!("{}.{}",step.as_str().unwrap_or("meanings"),issue.field),"label":field_label(&issue.field)})
        });
        json!({"step":issue.step,"field":issue.field,"code":issue.code,"message":issue.message,"location":readable})
    }).collect();
    json!({"issue_count":issues.len(),"issues":listed,"limited":issues.len()>30})
}

fn field_label(field: &str) -> &str {
    match field {
        "level" => "CEFR",
        "frequency" => "词频",
        "sub_pos" => "细分词性",
        "spelling" | "spelling_rich" => "拼写",
        "dict_phonetic" => "字典音标",
        "actual_pron" => "实际发音",
        "content" => "内容",
        "pos" => "词性",
        "sense_groups" => "语义区间",
        "grammar_structures" => "语法结构",
        "definitions" => "释义",
        _ => "填写内容",
    }
}

fn pos_label(code: &str) -> &str {
    match code {
        "noun" => "名词",
        "verb" => "动词",
        "adjective" => "形容词",
        "adverb" => "副词",
        "pronoun" => "代词",
        "preposition" => "介词",
        "conjunction" => "连词",
        "interjection" => "感叹词",
        "determiner" => "限定词",
        _ => code,
    }
}

type Locations = HashMap<String, (String, String)>;

fn collect_locations(value: &Value, path: &str, label: &str, output: &mut Locations) {
    match value {
        Value::Object(items) => {
            if let Some(id) = items
                .get("id")
                .or_else(|| items.get("pos_id"))
                .and_then(Value::as_str)
            {
                output.insert(id.into(), (path.into(), label.into()));
            }
            for (key, child) in items {
                if matches!(key.as_str(), "id" | "pos_id") {
                    continue;
                }
                collect_locations(child, &format!("{path}.{key}"), label, output);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let name = match path.rsplit('.').next().unwrap_or("") {
                    "senses" => "义项",
                    "forms" => "词形",
                    "form_groups" => "词形组",
                    "grammar_structures" => "语法结构",
                    "definitions" => "释义",
                    "sentences" => "例句",
                    "sense_groups" => "语义区间",
                    "zh_translations" => "中文译文",
                    "members" => "成员",
                    _ => "内容",
                };
                collect_locations(
                    item,
                    &format!("{path}[{index}]"),
                    &format!("{label} · {name} {}", index + 1),
                    output,
                );
            }
        }
        _ => (),
    }
}

fn locations(forms: &DraftFormsStepContentV3, meanings: &DraftMeaningsStepContentV3) -> Locations {
    let mut result = Locations::new();
    for (index, pos) in forms.pos.iter().enumerate() {
        collect_locations(
            &json!(pos),
            &format!("forms.pos[{index}]"),
            pos_label(&pos.pos),
            &mut result,
        );
    }
    for (index, pos) in meanings.pos.iter().enumerate() {
        let code = forms
            .pos
            .iter()
            .find(|p| p.pos_id == pos.pos_id)
            .map(|p| p.pos.as_str())
            .unwrap_or("词性");
        collect_locations(
            &json!(pos),
            &format!("meanings.pos[{index}]"),
            pos_label(code),
            &mut result,
        );
    }
    collect_locations(
        &json!(meanings.sense_groups),
        "meanings.sense_groups",
        "词义",
        &mut result,
    );
    result
}

fn internal_key(key: &str) -> bool {
    key.contains("audio")
        || key.contains("storage")
        || key.ends_with("_url")
        || matches!(
            key,
            "url"
                | "synthesis"
                | "synthesis_profile"
                | "voice_versions"
                | "voice_profile"
                | "created_by"
                | "archived_by"
                | "shared"
                | "text_links"
        )
}

fn project(value: Value, locations: &Locations) -> Value {
    match value {
        Value::Object(items) => {
            let mut output = Map::new();
            for (key, item) in items {
                if internal_key(&key) {
                    continue;
                }
                if key == "id" || key.ends_with("_id") || key.ends_with("_ids") {
                    let readable = |id: &Value| {
                        id.as_str()
                            .and_then(|id| locations.get(id))
                            .map(|(_, label)| Value::String(label.clone()))
                    };
                    let converted = if let Value::Array(ids) = &item {
                        Some(Value::Array(ids.iter().filter_map(readable).collect()))
                    } else {
                        readable(&item)
                    };
                    if let Some(converted) = converted {
                        let name = if key == "id" {
                            "location".to_owned()
                        } else {
                            key.trim_end_matches("_ids")
                                .trim_end_matches("_id")
                                .to_owned()
                                + "_location"
                        };
                        output.insert(name, converted);
                    }
                } else {
                    output.insert(key, project(item, locations));
                }
            }
            Value::Object(output)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| project(item, locations))
                .collect(),
        ),
        value => value,
    }
}

pub(super) fn content(
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
) -> (Value, Value) {
    let locations = locations(forms, meanings);
    (
        project(json!({"pos":forms.pos}), &locations),
        project(json!(meanings), &locations),
    )
}

fn chars(value: &Value) -> usize {
    value.to_string().chars().count()
}

/// Reduce whole sections, then examples, then complete senses. Never cut a field in half.
fn compress(mut value: Value, step: WordCreationStep, budget: usize) -> Value {
    let mut omitted = Vec::<String>::new();
    let minor = if matches!(step, WordCreationStep::Meanings | WordCreationStep::Preview) {
        "forms"
    } else {
        "meanings"
    };
    if chars(&value) > budget {
        let section = &value[minor];
        let summary: Vec<Value> = section["pos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|pos| {
                json!({
                    "pos":pos.get("pos").or_else(||pos.get("pos_location")),
                    "forms_count":pos["forms"].as_array().map(Vec::len),
                    "senses_count":pos["senses"].as_array().map(Vec::len),
                })
            })
            .collect();
        value[minor] = json!({"summary":summary});
        omitted.push(format!("{minor} 非当前步骤仅保留词性与数量摘要"));
    }
    if chars(&value) > budget {
        for pos in value["meanings"]["pos"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            for sense in pos["senses"].as_array_mut().into_iter().flatten() {
                let label = sense["location"].as_str().unwrap_or("义项").to_owned();
                if let Some(examples) = sense["sentences"].as_array_mut()
                    && examples.len() > 1
                {
                    omitted.push(format!("{label} · 例句 2～{}", examples.len()));
                    examples.truncate(1);
                }
            }
        }
    }
    for section in ["meanings", "forms"] {
        let key = if section == "meanings" {
            "senses"
        } else {
            "forms"
        };
        while chars(&value) > budget {
            let mut removed = None;
            if let Some(parts) = value[section]["pos"].as_array_mut() {
                for part in parts.iter_mut().rev() {
                    if let Some(nodes) = part[key].as_array_mut()
                        && let Some(node) = nodes.pop()
                    {
                        removed = Some(node["location"].as_str().unwrap_or(key).to_owned());
                        break;
                    }
                }
            }
            let Some(label) = removed else {
                break;
            };
            omitted.push(label);
        }
    }
    if chars(&value) > budget {
        for section in ["meanings", "forms"] {
            if !value[section].is_null() {
                value[section] = json!({"omitted":true});
                omitted.push(format!("{section} 内容超出预算，未提供"));
            }
        }
    }
    // Consecutive omitted items retain their readable positions, with a bounded list.
    let omitted_count = omitted.len();
    value["omissions"] =
        json!({"items":omitted.into_iter().take(40).collect::<Vec<_>>(),"total":omitted_count});
    value
}

pub(super) fn draft_view(draft: &Draft, step: WordCreationStep, budget: usize) -> Value {
    let (forms, meanings) = content(&draft.forms, &draft.meanings);
    compress(json!({"forms":forms,"meanings":meanings}), step, budget)
}

pub(super) fn headwords(forms: &DraftFormsStepContentV3) -> Vec<String> {
    super::super::v3_projection::presentation_from_native_forms(Uuid::nil(), forms)
        .map(|p| p.matched_surfaces)
        .unwrap_or_default()
}

const SAME_SPELLING_LIMIT: i64 = 5;
pub(super) async fn same_spelling(
    state: &AppState,
    spelling: &str,
    exclude: Option<Uuid>,
) -> Result<Value, AppError> {
    let key = super::super::normalization::normalize_headword(spelling)
        .map_err(|_| unavailable())?
        .key;
    let rows: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT s.entry_id, array_agg(DISTINCT s.content_scope ORDER BY s.content_scope)
         FROM lexicon.surface_sources s JOIN lexicon.entries e ON e.id=s.entry_id
         WHERE s.language='en' AND s.source_kind='form_variant' AND NOT s.is_deleted
         AND s.content_schema_version=3 AND s.normalized_surface=$1
         AND e.archived_at IS NULL AND e.content_schema_version=3
         AND ($2::uuid IS NULL OR s.entry_id<>$2)
         GROUP BY s.entry_id ORDER BY s.entry_id LIMIT $3",
    )
    .bind(&key)
    .bind(exclude)
    .bind(SAME_SPELLING_LIMIT + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::internal)?;
    let limited = rows.len() as i64 > SAME_SPELLING_LIMIT;
    let service = super::super::handler::service(state);
    let mut matches = Vec::new();
    for (entry_id, scopes) in rows.into_iter().take(SAME_SPELLING_LIMIT as usize) {
        let word = service
            .get_v3(entry_id)
            .await
            .map_err(super::super::handler::map_error)?;
        matches.push(json!({"entry_id":entry_id,"label":word.presentation.label,"status":word.status,
            "pos":word.forms.pos.iter().map(|p|p.pos.clone()).collect::<Vec<_>>(),"matched_in":scopes}));
    }
    Ok(
        json!({"spelling":spelling,"matches":matches,"limited":limited,"search_scope":"exact_spelling"}),
    )
}

pub(super) async fn dictionary(
    state: &AppState,
    q: &str,
) -> Result<(Value, Option<String>), AppError> {
    let normalized = super::super::normalization::normalize_headword(q)
        .map_err(|_| unavailable())?
        .key;
    let repo = LexiconRepository::new(state.pool.clone());
    let term = repo
        .dictionary_term(&normalized)
        .await
        .map_err(AppError::internal)?;
    let contents = repo
        .dictionary_contents_for_terms(&[normalized])
        .await
        .map_err(AppError::internal)?;
    let version = contents
        .first()
        .map(|c| c.provider_version.clone())
        .or_else(|| term.as_ref().map(|t| t.provider_version.clone()));
    let records: Vec<Value> = contents
        .iter()
        .take(12)
        .map(|c| {
            json!({"pos":c.pos,"senses":c.senses,"forms":c.forms,
        "sounds":c.sounds,"provider":c.provider_name,"version":c.provider_version})
        })
        .collect();
    let headword=term.as_ref().map(|t|json!({"term":t.term,"pos":t.pos,"region":t.region_family,"provider":t.provider_name,"version":t.provider_version}));
    Ok((
        json!({"headword":headword,"contents":records,"content_available":!contents.is_empty(),"records_total":contents.len(),"records_limited":contents.len()>12}),
        version,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assistant_uses_publish_semantics_and_keeps_whole_fields() {
        let draft = Draft {
            forms: DraftFormsStepContentV3::default(),
            meanings: DraftMeaningsStepContentV3::default(),
        };
        let checked = check(Uuid::nil(), &draft, &HashMap::new());
        assert!(
            checked["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == "sense_group_required")
        );
        let long = "完整释义".repeat(500);
        let value = json!({"forms":{"pos":[]},"meanings":{"pos":[{"pos":"noun","senses":[{"location":"名词 · 义项 1","definitions":[{"content":long}]}]}]}});
        let compressed = compress(value, WordCreationStep::Meanings, 500);
        assert!(!compressed.to_string().contains("完整释义"));
        assert!(
            compressed["omissions"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().unwrap().contains("义项 1"))
        );
    }
    #[test]
    fn projection_preserves_readable_relationships_without_ids() {
        let ids = HashMap::from([
            (
                "f".into(),
                ("forms.pos[0].forms[0]".into(), "名词 · 词形 1".into()),
            ),
            (
                "g".into(),
                (
                    "forms.pos[0].form_groups[0]".into(),
                    "名词 · 词形组 1".into(),
                ),
            ),
        ]);
        let value = project(
            json!({"id":"g","members":[{"id":"m","form_id":"f"}],"form_group_ids":["g"],"audio_url":"private"}),
            &ids,
        );
        assert_eq!(value["members"][0]["form_location"], "名词 · 词形 1");
        assert_eq!(value["form_group_location"][0], "名词 · 词形组 1");
        assert!(!value.to_string().contains("\"id\"") && !value.to_string().contains("private"));
    }
}
