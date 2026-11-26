//! Reuse the V1 authoring contract, never its entry IDs or storage.
use std::collections::HashSet;

use sqlx::PgConnection;
use uuid::Uuid;

use super::{language, model::*};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::*,
};

pub(super) fn validate(
    editor: &SharedFormsStepContentV3,
    ids: &mut HashSet<Uuid>,
) -> Result<(), AppError> {
    let forms = editor
        .clone()
        .resolve()
        .map_err(|error| error.into_app_error())?;
    let issues = crate::lexicon::v3_contract::validate_forms(&forms, StepSaveIntent::Save);
    if !issues.is_empty() {
        return Err(crate::lexicon::v3_contract::contract_validation_error(
            &issues,
        ));
    }
    for (id, _, _) in nodes(editor) {
        language::identity(ids, id)?;
    }
    Ok(())
}

fn nodes(editor: &SharedFormsStepContentV3) -> Vec<(Uuid, &'static str, Option<Uuid>)> {
    let mut nodes = Vec::new();
    for content in &editor.contents {
        nodes.push((content.id, "content", None));
        for (_, variant) in content.regional_variants.variants() {
            nodes.push((variant.id, "content_variant", Some(content.id)));
            for pronunciation in &variant.pronunciations {
                nodes.push((pronunciation.id, "content_pronunciation", Some(variant.id)));
            }
        }
    }
    for pos in &editor.pos {
        nodes.push((pos.pos_id, "pos", None));
        for group in &pos.form_groups {
            nodes.push((group.id, "group", Some(pos.pos_id)));
            for member in &group.members {
                nodes.push((member.id, "membership", Some(group.id)));
            }
        }
        for form in &pos.forms {
            nodes.push((form.id, "form", Some(pos.pos_id)));
            for (_, variant) in form.regional_bindings.variants() {
                nodes.push((variant.id, "variant", Some(form.id)));
                for pronunciation in &variant.pronunciations {
                    nodes.push((pronunciation.id, "pronunciation", Some(variant.id)));
                }
            }
        }
    }
    nodes
}

/// Only new bindings inherit the catalog. A legacy write may omit all settings.
pub(super) async fn prepare_question_eligibility(
    conn: &mut PgConnection,
    draft: &mut NextDraft,
    previous: &NextDraft,
) -> Result<(), AppError> {
    let saved_ids: HashSet<_> = previous
        .form_editor
        .iter()
        .flat_map(|e| &e.pos)
        .flat_map(|p| &p.forms)
        .map(|f| f.id)
        .collect();
    let saved_disabled: HashSet<_> = previous
        .form_question_eligibility
        .iter()
        .filter(|setting| !setting.eligible)
        .map(|setting| setting.form_id)
        .collect();
    let mut submitted = std::collections::HashMap::new();
    for setting in std::mem::take(&mut draft.form_question_eligibility) {
        if submitted
            .insert(setting.form_id, setting.eligible)
            .is_some()
        {
            return Err(language::invalid());
        }
    }
    let mut pending = Vec::new();
    let mut disabled_ids = Vec::new();
    for form in draft
        .form_editor
        .iter()
        .flat_map(|e| &e.pos)
        .flat_map(|p| &p.forms)
    {
        match submitted.remove(&form.id) {
            Some(false) => disabled_ids.push(form.id),
            Some(true) => {}
            None if saved_disabled.contains(&form.id) => disabled_ids.push(form.id),
            None if !saved_ids.contains(&form.id) => pending.push((form.id, &form.form_type)),
            None => {}
        }
    }
    if !submitted.is_empty() {
        return Err(language::invalid());
    }
    if !pending.is_empty() {
        let disabled: HashSet<String> = sqlx::query_scalar(
            "SELECT code FROM catalog.form_types WHERE NOT default_question_eligible",
        )
        .fetch_all(conn)
        .await
        .map_err(AppError::internal)?
        .into_iter()
        .collect();
        disabled_ids.extend(
            pending
                .into_iter()
                .filter(|(_, kind)| disabled.contains(*kind))
                .map(|(id, _)| id),
        );
    }
    disabled_ids.sort_unstable();
    draft.form_question_eligibility = disabled_ids
        .into_iter()
        .map(|form_id| NextFormQuestionEligibility {
            form_id,
            eligible: false,
        })
        .collect();
    Ok(())
}

pub(super) fn projection(draft: &NextDraft) -> Result<Vec<NextForm>, AppError> {
    let mut rows = draft.forms.clone();
    if let Some(editor) = &draft.form_editor {
        for content in &editor.contents {
            for (region, variant) in content.regional_variants.variants() {
                if !variant.spelling.trim().is_empty() {
                    rows.push(NextForm {
                        id: variant.id,
                        locale: match region {
                            "uk" => "en-GB",
                            "us" => "en-US",
                            _ => "en",
                        }
                        .into(),
                        text: variant.spelling.clone(),
                    });
                }
            }
        }
    }
    let mut ids = HashSet::new();
    for row in &rows {
        language::identity(&mut ids, row.id)?;
    }
    Ok(rows)
}

async fn identity(
    conn: &mut PgConnection,
    entry: Uuid,
    id: Uuid,
    role: &str,
    parent: Option<Uuid>,
) -> Result<(), AppError> {
    if id.is_nil() {
        return Err(language::invalid());
    }
    sqlx::query("INSERT INTO lexicon_next.form_editor_nodes(id,entry_id,role,parent_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(id).bind(entry).bind(role).bind(parent).execute(&mut *conn).await.map_err(AppError::internal)?;
    let current: (Uuid, String, Option<Uuid>) = sqlx::query_as(
        "SELECT entry_id,role,parent_id FROM lexicon_next.form_editor_nodes WHERE id=$1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if current != (entry, role.into(), parent) {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "form editor node identity changed",
        ));
    }
    Ok(())
}

pub(super) async fn persist(
    conn: &mut PgConnection,
    entry: Uuid,
    draft: &mut NextDraft,
) -> Result<(), AppError> {
    let Some(editor) = &mut draft.form_editor else {
        return Ok(());
    };
    let resolved = editor
        .clone()
        .resolve()
        .map_err(|error| error.into_app_error())?;
    let expanded = serde_json::json!({"pos": resolved.pos});
    crate::speech::generated::validate_bindings_with_forms(
        conn,
        &expanded,
        Some((entry, &resolved)),
        None,
    )
    .await?;
    let editor_nodes = nodes(editor);
    let node_ids = editor_nodes
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    let grammar_claimed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon_next.nodes WHERE id=ANY($1) AND kind IN ('grammar','grammar_en','grammar_en_gb','grammar_en_us'))")
        .bind(&node_ids).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
    if grammar_claimed {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "forms identity already belongs to grammar",
        ));
    }
    for (id, role, parent) in editor_nodes {
        identity(conn, entry, id, role, parent).await?;
    }
    Ok(())
}
