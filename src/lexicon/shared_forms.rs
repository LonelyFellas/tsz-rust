//! Canonical shared authoring content and its binding-scoped reference projection.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::dto::*;

#[cfg(test)]
#[path = "shared_forms_tests.rs"]
mod tests;

pub(crate) fn legacy_slot(kind: &str, id: Uuid) -> Uuid {
    let digest = Sha256::digest(format!("tsz:shared-forms:{kind}:{id}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

pub(crate) fn supported(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-form-contents")
        .is_some_and(|value| value == "v1")
}

pub(crate) fn upgrade_error() -> crate::error::AppError {
    crate::error::AppError::conflict(
        crate::error::ErrorCode::FormContentsClientUpgradeRequired,
        Some("forms"),
        "shared form content requires an updated client",
    )
}

pub(crate) fn ensure_write(
    current: &DraftFormsStepContentV3,
    proposed: &DraftFormsStepContentV3,
) -> Result<(), crate::lexicon::service::LexiconServiceError> {
    if current.shared.is_some() && proposed.shared.is_none() {
        return Err(upgrade_error().into());
    }
    Ok(())
}

pub(crate) fn require_request_capability(
    headers: &axum::http::HeaderMap,
    input: &Value,
) -> Result<(), crate::error::AppError> {
    let shared = input
        .get("content")
        .and_then(|content| content.get("forms_format"))
        .is_some();
    if shared != supported(headers) {
        return Err(upgrade_error());
    }
    Ok(())
}

pub(crate) fn project_response(
    headers: &axum::http::HeaderMap,
    value: &mut Value,
) -> Result<(), crate::error::AppError> {
    match value {
        Value::Object(object) => {
            if !supported(headers) {
                object.remove("retired_content_slots");
                if let Some(Value::Array(nodes)) = object.get_mut("retired_stable_nodes") {
                    for node in nodes {
                        if let Some(node) = node.as_object_mut() {
                            node.remove("content_id");
                            node.remove("content_slot_id");
                        }
                    }
                }
            }
            if object.get("schema_version").and_then(Value::as_u64) == Some(3)
                && object.contains_key("forms")
                && object.contains_key("capabilities")
            {
                let forms_value = object.get_mut("forms").expect("forms exists");
                let mut forms: DraftFormsStepContentV3 =
                    serde_json::from_value(forms_value.clone())
                        .map_err(crate::error::AppError::internal)?;
                if supported(headers) {
                    *forms_value =
                        serde_json::to_value(forms.shared_representation().map_err(|error| {
                            crate::error::AppError::internal(anyhow::anyhow!(error))
                        })?)
                        .map_err(crate::error::AppError::internal)?;
                    object.get_mut("capabilities").expect("capabilities exists")["shared_form_contents"] =
                        serde_json::json!(true);
                } else {
                    if !forms.legacy_readable() {
                        return Err(upgrade_error());
                    }
                    forms.shared = None;
                    *forms_value =
                        serde_json::to_value(forms).map_err(crate::error::AppError::internal)?;
                    object
                        .get_mut("capabilities")
                        .expect("capabilities exists")
                        .as_object_mut()
                        .expect("capabilities object")
                        .remove("shared_form_contents");
                }
                return Ok(());
            }
            for nested in object.values_mut() {
                project_response(headers, nested)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                project_response(headers, item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn variants(regional: &WordRegionalVariantsV3) -> Vec<(&str, Value)> {
    match regional {
        WordRegionalVariantsV3::Common { common } => vec![(
            "common",
            serde_json::to_value(common).expect("serializable variant"),
        )],
        WordRegionalVariantsV3::UkUs { uk, us } => vec![
            (
                "uk",
                serde_json::to_value(uk).expect("serializable variant"),
            ),
            (
                "us",
                serde_json::to_value(us).expect("serializable variant"),
            ),
        ],
    }
}

fn extract_variant(
    value: &Value,
    previous: Option<&FormVariantBindingV3>,
    legacy_regular: bool,
) -> Result<(FormContentVariantV3, FormVariantBindingV3), String> {
    let mut value = value.clone();
    let object = value.as_object_mut().ok_or("invalid variant")?;
    let id: Uuid = serde_json::from_value(object.remove("id").ok_or("missing variant ID")?)
        .map_err(|e| e.to_string())?;
    object.remove("dialect");
    let is_regular = object
        .remove("is_regular")
        .and_then(|value| value.as_bool())
        .unwrap_or(legacy_regular);
    let component_usages = serde_json::from_value(
        object
            .remove("component_usages")
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|e| e.to_string())?;
    let content_slot_id = previous.map_or_else(
        || legacy_slot("variant", id),
        |binding| binding.content_slot_id,
    );
    object.insert("id".into(), serde_json::json!(content_slot_id));
    let rows = object
        .get_mut("pronunciations")
        .and_then(Value::as_array_mut)
        .ok_or("missing pronunciations")?;
    let previous_rows: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|binding| &binding.pronunciations)
        .map(|row| (row.id, row.content_slot_id))
        .collect();
    let mut pronunciations = Vec::new();
    for row in rows {
        let id: Uuid = serde_json::from_value(row["id"].clone()).map_err(|e| e.to_string())?;
        let slot = previous_rows
            .get(&id)
            .copied()
            .unwrap_or_else(|| legacy_slot("pronunciation", id));
        row["id"] = serde_json::json!(slot);
        pronunciations.push(FormPronunciationBindingV3 {
            id,
            content_slot_id: slot,
        });
    }
    let content = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok((
        content,
        FormVariantBindingV3 {
            id,
            content_slot_id,
            is_regular,
            pronunciations,
            component_usages,
        },
    ))
}

fn binding_variants(regional: &FormRegionalBindingsV3) -> Vec<(&str, &FormVariantBindingV3)> {
    match regional {
        FormRegionalBindingsV3::Common { common } => vec![("common", common)],
        FormRegionalBindingsV3::UkUs { uk, us } => vec![("uk", uk), ("us", us)],
    }
}

impl DraftFormsStepContentV3 {
    /// Freeze the canonical authoring graph, preserving binding IDs and content slots.
    pub(crate) fn shared_representation(&self) -> Result<SharedFormsStepContentV3, String> {
        let old_bindings: HashMap<_, _> = self
            .shared
            .as_ref()
            .into_iter()
            .flat_map(|shared| &shared.pos)
            .flat_map(|pos| &pos.forms)
            .map(|binding| (binding.id, binding))
            .collect();
        let old_contents: HashMap<_, _> = self
            .shared
            .as_ref()
            .into_iter()
            .flat_map(|shared| &shared.contents)
            .map(|content| (content.id, content))
            .collect();
        let mut contents = Vec::<FormContentV3>::new();
        let mut content_indices = HashMap::new();
        let mut positions = Vec::new();
        for pos in &self.pos {
            let mut bindings = Vec::new();
            for form in &pos.forms {
                let group = pos
                    .form_groups
                    .iter()
                    .find(|group| group.members.iter().any(|member| member.form_id == form.id))
                    .ok_or("form has no group")?;
                let previous = old_bindings.get(&form.id).copied();
                let content_id = previous.map_or_else(
                    || legacy_slot("content", form.id),
                    |binding| binding.content_id,
                );
                let rules = old_contents
                    .get(&content_id)
                    .map_or(group.dialect_rules, |content| content.dialect_rules);
                let previous_variants: HashMap<_, _> = previous
                    .into_iter()
                    .flat_map(|binding| binding_variants(&binding.regional_bindings))
                    .collect();
                let extracted = variants(&form.regional_variants)
                    .into_iter()
                    .map(|(dialect, value)| {
                        extract_variant(
                            &value,
                            previous_variants.get(dialect).copied(),
                            group.is_regular,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let (regional_variants, regional_bindings) = match extracted.as_slice() {
                    [(content, binding)] => (
                        FormContentRegionalVariantsV3::Common {
                            common: content.clone(),
                        },
                        FormRegionalBindingsV3::Common {
                            common: binding.clone(),
                        },
                    ),
                    [(uk, uk_binding), (us, us_binding)] => (
                        FormContentRegionalVariantsV3::UkUs {
                            uk: uk.clone(),
                            us: us.clone(),
                        },
                        FormRegionalBindingsV3::UkUs {
                            uk: uk_binding.clone(),
                            us: us_binding.clone(),
                        },
                    ),
                    _ => return Err("invalid regional shape".into()),
                };
                let content = FormContentV3 {
                    id: content_id,
                    dialect_rules: rules,
                    regional_variants,
                    pronunciation_slots: old_contents
                        .get(&content_id)
                        .map(|content| content.pronunciation_slots.clone())
                        .unwrap_or_default(),
                };
                if let Some(index) = content_indices.get(&content_id).copied() {
                    if contents[index] != content {
                        return Err("shared content projection diverged".into());
                    }
                } else {
                    content_indices.insert(content_id, contents.len());
                    contents.push(content);
                }
                bindings.push(FormContentBindingV3 {
                    id: form.id,
                    form_type: form.form_type.clone(),
                    content_id,
                    regional_bindings,
                });
            }
            positions.push(SharedPosFormsV3 {
                pos_id: pos.pos_id,
                pos: pos.pos.clone(),
                forms: bindings,
                form_groups: pos
                    .form_groups
                    .iter()
                    .map(|group| SharedFormGroupV3 {
                        id: group.id,
                        is_regular: group.is_regular,
                        scope: group.scope,
                        members: group.members.clone(),
                    })
                    .collect(),
            });
        }
        Ok(SharedFormsStepContentV3 {
            forms_format: SharedFormsFormatV3::SharedV1,
            contents,
            pos: positions,
        })
    }

    pub(crate) fn content_rules(&self, form_id: Uuid) -> Option<DialectRulesV3> {
        let shared = self.shared.as_ref()?;
        let binding = shared
            .pos
            .iter()
            .flat_map(|pos| &pos.forms)
            .find(|binding| binding.id == form_id)?;
        shared
            .contents
            .iter()
            .find(|content| content.id == binding.content_id)
            .map(|content| content.dialect_rules)
    }

    pub(crate) fn enable_shared(&mut self) -> Result<(), String> {
        self.shared = Some(Box::new(self.shared_representation()?));
        Ok(())
    }

    pub(crate) fn legacy_readable(&self) -> bool {
        self.pos.iter().all(|pos| {
            pos.form_groups.iter().all(|group| {
                group.members.iter().all(|member| {
                    self.content_rules(member.form_id)
                        .is_none_or(|rules| rules == group.dialect_rules)
                })
            })
        })
    }
}

fn expand_variant(
    content: &FormContentVariantV3,
    binding: &FormVariantBindingV3,
    dialect: &str,
    all_nodes: &mut HashSet<Uuid>,
) -> Result<Value, String> {
    if content.id != binding.content_slot_id || !all_nodes.insert(binding.id) {
        return Err(format!("invalid variant binding {}", binding.id));
    }
    let slots: HashMap<_, _> = content
        .pronunciations
        .iter()
        .map(|row| (row.id, row))
        .collect();
    if binding.pronunciations.len() != slots.len() {
        return Err(format!("incomplete pronunciation mapping {}", binding.id));
    }
    let mut mapped = HashSet::new();
    let mut references = HashMap::new();
    for row in &binding.pronunciations {
        if !mapped.insert(row.content_slot_id) || !all_nodes.insert(row.id) {
            return Err(format!("duplicate pronunciation binding {}", row.id));
        }
        if !slots.contains_key(&row.content_slot_id) {
            return Err(format!(
                "unknown pronunciation slot {}",
                row.content_slot_id
            ));
        }
        references.insert(row.content_slot_id, row.id);
    }
    let rows: Vec<_> = content
        .pronunciations
        .iter()
        .map(|source| {
            let mut pronunciation = source.clone();
            pronunciation.id = references[&source.id];
            pronunciation
        })
        .collect();
    let mut value = serde_json::to_value(content).map_err(|e| e.to_string())?;
    let object = value.as_object_mut().ok_or("invalid content")?;
    object.insert("id".into(), serde_json::json!(binding.id));
    object.insert("dialect".into(), serde_json::json!(dialect));
    object.insert("is_regular".into(), serde_json::json!(binding.is_regular));
    object.insert(
        "pronunciations".into(),
        serde_json::to_value(rows).map_err(|e| e.to_string())?,
    );
    object.insert(
        "component_usages".into(),
        serde_json::to_value(&binding.component_usages).map_err(|e| e.to_string())?,
    );
    Ok(value)
}

#[derive(Debug)]
pub(crate) struct MappingError(Box<V3DraftValidationIssue>);

impl std::fmt::Display for MappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0.message)
    }
}

impl MappingError {
    pub(crate) fn into_app_error(self) -> crate::error::AppError {
        crate::error::AppError::unprocessable(
            crate::error::ErrorCode::ValidationFailed,
            "invalid shared form mapping",
        )
        .with_v3_field_issues(vec![*self.0])
    }
}

fn mapping_error(
    message: String,
    content_id: Option<Uuid>,
    pos: Option<&SharedPosFormsV3>,
    binding: Option<&FormContentBindingV3>,
    variant: Option<&FormVariantBindingV3>,
) -> MappingError {
    let group = pos.zip(binding).and_then(|(pos, binding)| {
        pos.form_groups.iter().find(|group| {
            group
                .members
                .iter()
                .any(|member| member.form_id == binding.id)
        })
    });
    let node_id = variant
        .map(|variant| variant.id)
        .or_else(|| binding.map(|binding| binding.id))
        .or(content_id)
        .unwrap_or_default();
    MappingError(Box::new(V3DraftValidationIssue {
        schema_version: 3,
        step: PersistedWordStep::Forms,
        node_id,
        field: if variant.is_some() {
            "regional_bindings"
        } else {
            "content_id"
        }
        .into(),
        code: V3ValidationIssueCode::FormContentMappingInvalid,
        message,
        node_location: V3DraftNodeLocation {
            content_id,
            content_slot_id: variant.map(|variant| variant.content_slot_id),
            node_role: "forms.content".into(),
            ancestor_node_ids: [
                pos.map(|pos| pos.pos_id),
                group.map(|group| group.id),
                binding.map(|binding| binding.id),
            ]
            .into_iter()
            .flatten()
            .filter(|id| *id != node_id)
            .collect(),
            pos_id: pos.map(|pos| pos.pos_id),
            form_group_id: group.map(|group| group.id),
            membership_id: group
                .zip(binding)
                .and_then(|(group, binding)| {
                    group
                        .members
                        .iter()
                        .find(|member| member.form_id == binding.id)
                })
                .map(|member| member.id),
            form_id: binding.map(|binding| binding.id),
            variant_id: variant.map(|variant| variant.id),
            pronunciation_id: None,
            form_type: binding.map(|binding| binding.form_type.clone()),
            dialect: None,
        },
    }))
}

fn pronunciation_slots(
    mapping: &FormPronunciationSlotsV3,
) -> impl Iterator<Item = (&'static str, &FormPronunciationSlotV3)> {
    [
        ("common", mapping.common.as_ref()),
        ("uk", mapping.uk.as_ref()),
        ("us", mapping.us.as_ref()),
    ]
    .into_iter()
    .filter_map(|(side, slot)| slot.map(|slot| (side, slot)))
}

impl SharedFormsStepContentV3 {
    pub(crate) fn resolve(self) -> Result<DraftFormsStepContentV3, MappingError> {
        let content_error = |content_id: Uuid, message: String| {
            let context = self.pos.iter().find_map(|pos| {
                pos.forms
                    .iter()
                    .find(|binding| binding.content_id == content_id)
                    .map(|binding| (pos, binding))
            });
            mapping_error(
                message,
                Some(content_id),
                context.map(|item| item.0),
                context.map(|item| item.1),
                None,
            )
        };
        let mut slots = HashSet::new();
        let mut contents = HashMap::new();
        for content in &self.contents {
            if !slots.insert(content.id) || !content.dialect_rules.is_valid() {
                return Err(content_error(
                    content.id,
                    format!("invalid content {}", content.id),
                ));
            }
            let variants = match &content.regional_variants {
                FormContentRegionalVariantsV3::Common { common } => vec![common],
                FormContentRegionalVariantsV3::UkUs { uk, us } => vec![uk, us],
            };
            for variant in variants {
                if !slots.insert(variant.id)
                    || variant
                        .pronunciations
                        .iter()
                        .any(|row| !slots.insert(row.id))
                {
                    return Err(content_error(
                        content.id,
                        format!("duplicate content slot {}", variant.id),
                    ));
                }
            }
            contents.insert(content.id, content);
        }
        let mut identities = HashMap::new();
        for content in &self.contents {
            identities.insert(content.id, (content.id, None, "content"));
            let variants = match &content.regional_variants {
                FormContentRegionalVariantsV3::Common { common } => vec![("common", common)],
                FormContentRegionalVariantsV3::UkUs { uk, us } => vec![("uk", uk), ("us", us)],
            };
            for (side, variant) in variants {
                identities.insert(variant.id, (content.id, Some(content.id), side));
                for row in &variant.pronunciations {
                    identities.insert(row.id, (content.id, Some(variant.id), "pronunciation"));
                }
            }
        }
        let mut mapping_slots = HashSet::new();
        let mut mapping_ids = HashSet::new();
        for content in &self.contents {
            if content.pronunciation_slots.len() > 2000 {
                return Err(content_error(
                    content.id,
                    "too many pronunciation mappings".into(),
                ));
            }
            let mut regions = match &content.regional_variants {
                FormContentRegionalVariantsV3::Common { common } => {
                    HashMap::from([("common", common.id)])
                }
                FormContentRegionalVariantsV3::UkUs { uk, us } => {
                    HashMap::from([("uk", uk.id), ("us", us.id)])
                }
            };
            for mapping in &content.pronunciation_slots {
                if !mapping_ids.insert(mapping.id) {
                    return Err(content_error(
                        content.id,
                        "duplicate pronunciation mapping ID".into(),
                    ));
                }
                let mut sides = 0;
                for (side, slot) in pronunciation_slots(mapping) {
                    sides += 1;
                    if !mapping_slots.insert(slot.id)
                        || regions
                            .insert(side, slot.variant_slot_id)
                            .is_some_and(|id| id != slot.variant_slot_id)
                    {
                        return Err(content_error(
                            content.id,
                            "invalid pronunciation slot correspondence".into(),
                        ));
                    }
                    for (id, identity) in [
                        (slot.variant_slot_id, (content.id, Some(content.id), side)),
                        (
                            slot.id,
                            (content.id, Some(slot.variant_slot_id), "pronunciation"),
                        ),
                    ] {
                        if identities
                            .insert(id, identity)
                            .is_some_and(|previous| previous != identity)
                        {
                            return Err(content_error(
                                content.id,
                                "pronunciation slot identity differs".into(),
                            ));
                        }
                    }
                }
                if sides < 2 {
                    return Err(content_error(
                        content.id,
                        "pronunciation correspondence requires two regions".into(),
                    ));
                }
            }
        }
        if mapping_ids.iter().any(|id| identities.contains_key(id)) {
            return Err(mapping_error(
                "pronunciation mapping ID collides with a content slot".into(),
                None,
                None,
                None,
                None,
            ));
        }
        if identities.len() + mapping_ids.len() > super::validation::MAX_ENTRY_NODES {
            return Err(mapping_error(
                "shared content exceeds node limit".into(),
                None,
                None,
                None,
                None,
            ));
        }
        let mut used = HashSet::new();
        let mut nodes = HashSet::new();
        let mut positions = Vec::new();
        for pos in &self.pos {
            let mut forms = Vec::new();
            let mut rules_by_form = HashMap::new();
            for binding in &pos.forms {
                let error = |message| {
                    mapping_error(
                        message,
                        Some(binding.content_id),
                        Some(pos),
                        Some(binding),
                        None,
                    )
                };
                let content = contents.get(&binding.content_id).ok_or_else(|| {
                    error(format!(
                        "unknown content {} for form {}",
                        binding.content_id, binding.id
                    ))
                })?;
                let expand = |content,
                              reference: &FormVariantBindingV3,
                              dialect,
                              nodes: &mut HashSet<Uuid>| {
                    expand_variant(content, reference, dialect, nodes).map_err(|message| {
                        mapping_error(
                            message,
                            Some(binding.content_id),
                            Some(pos),
                            Some(binding),
                            Some(reference),
                        )
                    })
                };
                used.insert(content.id);
                rules_by_form.insert(binding.id, content.dialect_rules);
                let (mode, values) = match (&content.regional_variants, &binding.regional_bindings)
                {
                    (
                        FormContentRegionalVariantsV3::Common { common },
                        FormRegionalBindingsV3::Common { common: reference },
                    ) => (
                        "common",
                        vec![("common", expand(common, reference, "common", &mut nodes)?)],
                    ),
                    (
                        FormContentRegionalVariantsV3::UkUs { uk, us },
                        FormRegionalBindingsV3::UkUs {
                            uk: uk_ref,
                            us: us_ref,
                        },
                    ) => (
                        "uk_us",
                        vec![
                            ("uk", expand(uk, uk_ref, "uk", &mut nodes)?),
                            ("us", expand(us, us_ref, "us", &mut nodes)?),
                        ],
                    ),
                    _ => {
                        return Err(error(format!(
                            "regional mapping differs for form {}",
                            binding.id
                        )));
                    }
                };
                let mut regional = serde_json::json!({"mode": mode});
                for (dialect, value) in values {
                    regional[dialect] = value;
                }
                forms.push(WordConcreteFormV3 {
                    id: binding.id,
                    form_type: binding.form_type.clone(),
                    regional_variants: serde_json::from_value(regional)
                        .map_err(|e| error(e.to_string()))?,
                });
            }
            let mut groups = Vec::new();
            for group in &pos.form_groups {
                let mut labels = HashSet::new();
                for member in &group.members {
                    if let Some(binding) = pos
                        .forms
                        .iter()
                        .find(|binding| binding.id == member.form_id)
                        && !labels.insert((binding.content_id, binding.form_type.as_str()))
                    {
                        return Err(mapping_error(
                            format!("duplicate content label in group {}", group.id),
                            Some(binding.content_id),
                            Some(pos),
                            Some(binding),
                            None,
                        ));
                    }
                }
                groups.push(WordFormGroupV3 {
                    id: group.id,
                    is_regular: group.is_regular,
                    scope: group.scope,
                    dialect_rules: group
                        .members
                        .first()
                        .and_then(|member| rules_by_form.get(&member.form_id))
                        .copied()
                        .unwrap_or(DialectRulesV3::UNIFIED),
                    members: group.members.clone(),
                });
            }
            positions.push(WordPosFormsV3 {
                pos_id: pos.pos_id,
                pos: pos.pos.clone(),
                forms,
                form_groups: groups,
            });
        }
        if let Some(content) = self
            .contents
            .iter()
            .find(|content| !used.contains(&content.id))
        {
            return Err(content_error(
                content.id,
                "content requires at least one binding".into(),
            ));
        }
        Ok(DraftFormsStepContentV3 {
            pos: positions,
            shared: Some(Box::new(self)),
        })
    }
}

pub(crate) async fn preserve_pronunciation_slots(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_id: Uuid,
    forms: &mut DraftFormsStepContentV3,
) -> Result<(), crate::lexicon::service::LexiconServiceError> {
    use crate::lexicon::repository::LexiconRepositoryError;
    use crate::lexicon::service::LexiconServiceError;
    let Some(shared) = &mut forms.shared else {
        return Ok(());
    };
    let content_ids: Vec<_> = shared.contents.iter().map(|content| content.id).collect();
    let contents: HashMap<_, _> = shared
        .contents
        .iter()
        .enumerate()
        .map(|(index, content)| (content.id, index))
        .collect();
    let mut mappings: HashMap<_, _> = shared
        .contents
        .iter()
        .flat_map(|content| {
            content
                .pronunciation_slots
                .iter()
                .enumerate()
                .map(move |(index, mapping)| ((content.id, mapping.id), index))
        })
        .collect();
    let proposed_slots: HashMap<_, _> = shared
        .contents
        .iter()
        .flat_map(|content| {
            content.pronunciation_slots.iter().flat_map(|mapping| {
                pronunciation_slots(mapping).map(move |(side, slot)| (slot.id, (mapping.id, side)))
            })
        })
        .collect();
    let slots: Vec<(Uuid, Uuid, String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT m.content_id,m.mapping_id,m.region,m.slot_id,s.parent_slot_id FROM lexicon.v3_form_pronunciation_mapping_slots m JOIN lexicon.v3_form_content_slots s ON s.id=m.slot_id WHERE m.entry_id=$1 AND m.content_id=ANY($2) ORDER BY m.content_id,m.mapping_id,m.region",
    )
    .bind(entry_id).bind(&content_ids).fetch_all(&mut **tx).await
    .map_err(|error| LexiconServiceError::Repository(LexiconRepositoryError::Database(error)))?;
    for (content_id, mapping_id, side, id, variant_slot_id) in slots {
        if proposed_slots
            .get(&id)
            .is_some_and(|identity| *identity != (mapping_id, side.as_str()))
        {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
        let content = &mut shared.contents[contents[&content_id]];
        let index = *mappings.entry((content_id, mapping_id)).or_insert_with(|| {
            content.pronunciation_slots.push(FormPronunciationSlotsV3 {
                id: mapping_id,
                common: None,
                uk: None,
                us: None,
            });
            content.pronunciation_slots.len() - 1
        });
        let mapping = &mut content.pronunciation_slots[index];
        let slot = match side.as_str() {
            "common" => &mut mapping.common,
            "uk" => &mut mapping.uk,
            "us" => &mut mapping.us,
            _ => unreachable!("database region constraint"),
        };
        slot.get_or_insert(FormPronunciationSlotV3 {
            id,
            variant_slot_id,
        });
    }
    Ok(())
}

pub(crate) async fn preflight_identities(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
) -> Result<(), crate::lexicon::service::LexiconServiceError> {
    use crate::lexicon::repository::LexiconRepositoryError;
    use crate::lexicon::service::LexiconServiceError;
    let db = |error| LexiconServiceError::Repository(LexiconRepositoryError::Database(error));
    let shared = forms.shared_representation().map_err(|error| {
        LexiconServiceError::Authorization(crate::error::AppError::internal(anyhow::anyhow!(error)))
    })?;
    let mut slots = HashMap::new();
    let mut mappings = HashMap::new();
    let mut mapping_slots = HashMap::new();
    let mut nodes = HashMap::new();
    let content_ids: Vec<_> = shared.contents.iter().map(|content| content.id).collect();
    for content in &shared.contents {
        slots.insert(
            content.id,
            (entry_id, content.id, None, "content".to_owned()),
        );
        let variants = match &content.regional_variants {
            FormContentRegionalVariantsV3::Common { common } => vec![("common", common)],
            FormContentRegionalVariantsV3::UkUs { uk, us } => vec![("uk", uk), ("us", us)],
        };
        for (side, variant) in variants {
            slots.insert(
                variant.id,
                (entry_id, content.id, Some(content.id), side.to_owned()),
            );
            for row in &variant.pronunciations {
                slots.insert(
                    row.id,
                    (
                        entry_id,
                        content.id,
                        Some(variant.id),
                        "pronunciation".to_owned(),
                    ),
                );
            }
        }
        for mapping in &content.pronunciation_slots {
            mappings.insert(mapping.id, (entry_id, content.id));
            for (side, slot) in pronunciation_slots(mapping) {
                slots.insert(
                    slot.variant_slot_id,
                    (entry_id, content.id, Some(content.id), side.to_owned()),
                );
                slots.insert(
                    slot.id,
                    (
                        entry_id,
                        content.id,
                        Some(slot.variant_slot_id),
                        "pronunciation".to_owned(),
                    ),
                );
                mapping_slots.insert(slot.id, (mapping.id, entry_id, content.id, side.to_owned()));
            }
        }
    }
    for binding in shared.pos.iter().flat_map(|pos| &pos.forms) {
        for (_, variant) in binding_variants(&binding.regional_bindings) {
            nodes.insert(
                variant.id,
                (entry_id, binding.content_id, variant.content_slot_id),
            );
            for row in &variant.pronunciations {
                nodes.insert(row.id, (entry_id, binding.content_id, row.content_slot_id));
            }
        }
    }
    let slot_ids: Vec<_> = slots.keys().copied().collect();
    let existing: Vec<(Uuid, Uuid, Uuid, Option<Uuid>, String)> = sqlx::query_as("SELECT id,entry_id,content_id,parent_slot_id,role FROM lexicon.v3_form_content_slots WHERE id=ANY($1) OR (content_id=ANY($2) AND role IN ('common','uk','us'))")
        .bind(&slot_ids).bind(&content_ids).fetch_all(&mut **tx).await.map_err(db)?;
    let regions: HashMap<_, _> = slots
        .iter()
        .filter(|(_, (_, _, _, role))| matches!(role.as_str(), "common" | "uk" | "us"))
        .map(|(id, (_, content, _, role))| ((*content, role.clone()), *id))
        .collect();
    for (id, entry, content, parent, role) in existing {
        if slots
            .get(&id)
            .is_some_and(|identity| identity != &(entry, content, parent, role.clone()))
            || regions
                .get(&(content, role))
                .is_some_and(|proposed| *proposed != id)
        {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    let mapping_ids: Vec<_> = mappings.keys().copied().collect();
    let existing: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as("SELECT id,entry_id,content_id FROM lexicon.v3_form_pronunciation_mappings WHERE id=ANY($1)")
        .bind(&mapping_ids).fetch_all(&mut **tx).await.map_err(db)?;
    for (id, entry, content) in existing {
        if mappings.get(&id) != Some(&(entry, content)) {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    let existing: Vec<(Uuid, Uuid, Uuid, Uuid, String)> = sqlx::query_as("SELECT slot_id,mapping_id,entry_id,content_id,region FROM lexicon.v3_form_pronunciation_mapping_slots WHERE slot_id=ANY($1) OR mapping_id=ANY($2)")
        .bind(&slot_ids).bind(&mapping_ids).fetch_all(&mut **tx).await.map_err(db)?;
    let sides: HashMap<_, _> = mapping_slots
        .iter()
        .map(|(id, (mapping, _, _, side))| ((*mapping, side.clone()), *id))
        .collect();
    for (slot, mapping, entry, content, side) in existing {
        if mapping_slots
            .get(&slot)
            .is_some_and(|identity| identity != &(mapping, entry, content, side.clone()))
            || sides
                .get(&(mapping, side))
                .is_some_and(|proposed| *proposed != slot)
        {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    let node_ids: Vec<_> = nodes.keys().copied().collect();
    let existing: Vec<(Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as("SELECT node_id,entry_id,content_id,content_slot_id FROM lexicon.v3_form_content_node_slots WHERE node_id=ANY($1)")
        .bind(&node_ids).fetch_all(&mut **tx).await.map_err(db)?;
    for (id, entry, content, slot) in existing {
        if nodes.get(&id) != Some(&(entry, content, slot)) {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    shared
        .resolve()
        .map_err(|error| LexiconServiceError::Authorization(error.into_app_error()))?;
    Ok(())
}

pub(crate) async fn persist(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
) -> Result<(), crate::lexicon::service::LexiconServiceError> {
    use crate::lexicon::repository::LexiconRepositoryError;
    use crate::lexicon::service::LexiconServiceError;
    let db = |error| LexiconServiceError::Repository(LexiconRepositoryError::Database(error));
    preflight_identities(tx, entry_id, forms).await?;
    let shared = forms.shared_representation().map_err(|error| {
        LexiconServiceError::Authorization(crate::error::AppError::internal(anyhow::anyhow!(error)))
    })?;
    sqlx::query("UPDATE lexicon.v3_form_content_slots SET retired_at = COALESCE(retired_at, now()) WHERE entry_id=$1")
        .bind(entry_id).execute(&mut **tx).await.map_err(db)?;
    sqlx::query("DELETE FROM lexicon.v3_form_content_bindings WHERE entry_id=$1")
        .bind(entry_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    sqlx::query("DELETE FROM lexicon.v3_form_contents WHERE entry_id=$1")
        .bind(entry_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    for content in &shared.contents {
        sqlx::query("INSERT INTO lexicon.v3_form_contents(id,entry_id,content) VALUES($1,$2,$3)")
            .bind(content.id)
            .bind(entry_id)
            .bind(serde_json::to_value(content).map_err(|error| {
                LexiconServiceError::Authorization(crate::error::AppError::internal(error))
            })?)
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        let mut slots = vec![(content.id, None, "content")];
        let variants = match &content.regional_variants {
            FormContentRegionalVariantsV3::Common { common } => vec![("common", common)],
            FormContentRegionalVariantsV3::UkUs { uk, us } => vec![("uk", uk), ("us", us)],
        };
        for (dialect, variant) in variants {
            slots.push((variant.id, Some(content.id), dialect));
            slots.extend(
                variant
                    .pronunciations
                    .iter()
                    .map(|row| (row.id, Some(variant.id), "pronunciation")),
            );
        }
        let active: HashSet<_> = slots.iter().map(|(id, _, _)| *id).collect();
        for mapping in &content.pronunciation_slots {
            for (side, slot) in pronunciation_slots(mapping) {
                slots.push((slot.variant_slot_id, Some(content.id), side));
                slots.push((slot.id, Some(slot.variant_slot_id), "pronunciation"));
            }
        }
        for (id, parent, role) in slots {
            let result = sqlx::query("INSERT INTO lexicon.v3_form_content_slots(id,entry_id,content_id,parent_slot_id,role,retired_at) VALUES($1,$2,$3,$4,$5,CASE WHEN $6 THEN NULL ELSE now() END) ON CONFLICT(id) DO UPDATE SET retired_at=CASE WHEN EXCLUDED.retired_at IS NULL THEN NULL ELSE COALESCE(lexicon.v3_form_content_slots.retired_at,EXCLUDED.retired_at) END WHERE lexicon.v3_form_content_slots.entry_id=EXCLUDED.entry_id AND lexicon.v3_form_content_slots.content_id=EXCLUDED.content_id AND lexicon.v3_form_content_slots.parent_slot_id IS NOT DISTINCT FROM EXCLUDED.parent_slot_id AND lexicon.v3_form_content_slots.role=EXCLUDED.role")
                .bind(id).bind(entry_id).bind(content.id).bind(parent).bind(role).bind(active.contains(&id)).execute(&mut **tx).await.map_err(db)?;
            if result.rows_affected() != 1 {
                return Err(LexiconServiceError::StableNodeIdChanged);
            }
        }
        let existing: Vec<(Uuid, String, Uuid)> = sqlx::query_as("SELECT mapping_id,region,slot_id FROM lexicon.v3_form_pronunciation_mapping_slots WHERE entry_id=$1 AND content_id=$2")
            .bind(entry_id).bind(content.id).fetch_all(&mut **tx).await.map_err(db)?;
        let existing: HashMap<_, _> = existing
            .into_iter()
            .map(|(mapping, side, slot)| ((mapping, side), slot))
            .collect();
        for mapping in &content.pronunciation_slots {
            let result = sqlx::query("INSERT INTO lexicon.v3_form_pronunciation_mappings(id,entry_id,content_id) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET content_id=EXCLUDED.content_id WHERE lexicon.v3_form_pronunciation_mappings.entry_id=EXCLUDED.entry_id AND lexicon.v3_form_pronunciation_mappings.content_id=EXCLUDED.content_id")
                .bind(mapping.id).bind(entry_id).bind(content.id).execute(&mut **tx).await.map_err(db)?;
            if result.rows_affected() != 1 {
                return Err(LexiconServiceError::StableNodeIdChanged);
            }
            for (side, slot) in pronunciation_slots(mapping) {
                if existing
                    .get(&(mapping.id, side.to_owned()))
                    .is_some_and(|id| *id != slot.id)
                {
                    return Err(LexiconServiceError::StableNodeIdChanged);
                }
                let result = sqlx::query("INSERT INTO lexicon.v3_form_pronunciation_mapping_slots(slot_id,mapping_id,entry_id,content_id,region) VALUES($1,$2,$3,$4,$5) ON CONFLICT(slot_id) DO UPDATE SET mapping_id=EXCLUDED.mapping_id WHERE lexicon.v3_form_pronunciation_mapping_slots.mapping_id=EXCLUDED.mapping_id AND lexicon.v3_form_pronunciation_mapping_slots.entry_id=EXCLUDED.entry_id AND lexicon.v3_form_pronunciation_mapping_slots.content_id=EXCLUDED.content_id AND lexicon.v3_form_pronunciation_mapping_slots.region=EXCLUDED.region")
                    .bind(slot.id).bind(mapping.id).bind(entry_id).bind(content.id).bind(side).execute(&mut **tx).await.map_err(db)?;
                if result.rows_affected() != 1 {
                    return Err(LexiconServiceError::StableNodeIdChanged);
                }
            }
        }
    }
    for binding in shared.pos.iter().flat_map(|pos| &pos.forms) {
        sqlx::query("INSERT INTO lexicon.v3_form_content_bindings(form_id,entry_id,content_id,regional_bindings) VALUES($1,$2,$3,$4)")
            .bind(binding.id).bind(entry_id).bind(binding.content_id).bind(serde_json::to_value(&binding.regional_bindings).map_err(|error| LexiconServiceError::Authorization(crate::error::AppError::internal(error)))?).execute(&mut **tx).await.map_err(db)?;
        for (_, variant) in binding_variants(&binding.regional_bindings) {
            let nodes = std::iter::once((variant.id, variant.content_slot_id)).chain(
                variant
                    .pronunciations
                    .iter()
                    .map(|row| (row.id, row.content_slot_id)),
            );
            for (node_id, slot_id) in nodes {
                let result = sqlx::query("INSERT INTO lexicon.v3_form_content_node_slots(node_id,entry_id,content_id,content_slot_id) VALUES($1,$2,$3,$4) ON CONFLICT(node_id) DO UPDATE SET content_slot_id=EXCLUDED.content_slot_id WHERE lexicon.v3_form_content_node_slots.entry_id=EXCLUDED.entry_id AND lexicon.v3_form_content_node_slots.content_id=EXCLUDED.content_id AND lexicon.v3_form_content_node_slots.content_slot_id=EXCLUDED.content_slot_id")
                    .bind(node_id).bind(entry_id).bind(binding.content_id).bind(slot_id).execute(&mut **tx).await.map_err(db)?;
                if result.rows_affected() != 1 {
                    return Err(LexiconServiceError::StableNodeIdChanged);
                }
            }
        }
    }
    Ok(())
}

impl Serialize for DraftFormsStepContentV3 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.shared.is_some() {
            self.shared_representation()
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer)
        } else {
            ExpandedFormsStepContentV3 {
                pos: self.pos.clone(),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for DraftFormsStepContentV3 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let representation = FormsStepRepresentationV3::deserialize(deserializer)?;
        match representation {
            FormsStepRepresentationV3::Shared(shared) => {
                shared.resolve().map_err(serde::de::Error::custom)
            }
            FormsStepRepresentationV3::Expanded(expanded) => Ok(Self {
                pos: expanded.pos,
                shared: None,
            }),
        }
    }
}
