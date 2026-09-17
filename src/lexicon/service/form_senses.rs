use uuid::Uuid;

use crate::lexicon::dto::{
    DraftMeaningsStepContentV3, FormGroupScopeV3, WordPosFormsV3, WordSenseV3,
};

/// 词义始终限定在词形所属基本词性内；通用组可用全部词义，专用组只用绑定词义。
pub(super) fn allowed_form_senses<'a>(
    pos: &WordPosFormsV3,
    meanings: &'a DraftMeaningsStepContentV3,
    form_id: Uuid,
) -> impl Iterator<Item = &'a WordSenseV3> {
    let group = pos
        .form_groups
        .iter()
        .find(|group| group.members.iter().any(|member| member.form_id == form_id))
        .map(|group| (group.id, group.scope));
    meanings
        .pos
        .iter()
        .find(|meanings| meanings.pos_id == pos.pos_id)
        .into_iter()
        .flat_map(|meanings| &meanings.senses)
        .filter(move |sense| {
            group.is_some_and(|(id, scope)| {
                scope == FormGroupScopeV3::General || sense.bound_form_group_ids().contains(&id)
            })
        })
}

pub(super) fn apply_sense_bindings(
    forms: &crate::lexicon::dto::DraftFormsStepContentV3,
    meanings: &mut DraftMeaningsStepContentV3,
    bindings: &[crate::lexicon::dto::SenseFormGroupBindingV3],
) -> Result<(), super::LexiconServiceError> {
    use crate::lexicon::{
        dto::StepSaveIntent,
        v3_contract::{v3_issues, validate_sense_form_groups},
    };
    use std::collections::HashSet;
    let invalid = || super::LexiconServiceError::InvalidField {
        field: "sense_bindings",
        message: "bindings must reference distinct existing senses",
    };
    if bindings.len() > 2000 {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    let mut touched: HashSet<Uuid> = HashSet::new();
    for binding in bindings {
        if !seen.insert(binding.sense_id) {
            return Err(invalid());
        }
        let sense = meanings
            .pos
            .iter_mut()
            .flat_map(|pos| &mut pos.senses)
            .find(|sense| sense.id == binding.sense_id)
            .ok_or_else(invalid)?;
        if sense.form_group_ids.is_some() && binding.form_group_ids.is_none() {
            return Err(super::LexiconServiceError::InvalidField {
                field: "form_group_ids",
                message: "refresh the editor before saving multi-group sense bindings",
            });
        }
        touched.extend(sense.bound_form_group_ids());
        touched.extend(binding.bound_form_group_ids());
        sense.form_group_id = binding.form_group_id;
        sense.form_group_ids = binding.form_group_ids.clone();
    }
    if bindings.is_empty() {
        return Ok(());
    }
    let mut issues = validate_sense_form_groups(forms, meanings, StepSaveIntent::Save);
    issues.extend(
        validate_sense_form_groups(forms, meanings, StepSaveIntent::Complete)
            .into_iter()
            .filter(|issue| {
                issue.code == "dedicated_form_group_unused" && touched.contains(&issue.node_id)
            }),
    );
    if !issues.is_empty() {
        return Err(super::LexiconServiceError::ValidationFailedV3(v3_issues(
            &issues,
        )));
    }
    Ok(())
}

pub(super) fn forms_command_hash(
    forms: &crate::lexicon::dto::DraftFormsStepContentV3,
    bindings: &[crate::lexicon::dto::SenseFormGroupBindingV3],
) -> Result<Vec<u8>, super::LexiconServiceError> {
    if bindings.is_empty() {
        crate::lexicon::normalization::sha256_json(forms)
    } else {
        crate::lexicon::normalization::sha256_json(&(forms, bindings))
    }
    .map_err(super::serialization_error)
}
