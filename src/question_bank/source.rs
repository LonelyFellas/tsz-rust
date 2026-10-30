use super::{dto::*, invalid, source_changed};
use crate::{
    error::AppError,
    learning_tasks::repository::Tx,
    lexicon::{
        dto::{WordDefinitionV3, WordRegionalVariantsV3},
        published,
    },
    platform::text::key_v1,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn validate_context(level: &str, profile: &str) -> Result<(), AppError> {
    if !["A1", "A2", "B1", "B2", "C1", "C2"].contains(&level) || !["BrE", "AmE"].contains(&profile)
    {
        return Err(invalid("级别或英美档位无效"));
    }
    Ok(())
}
fn rank(level: &str) -> usize {
    ["A1", "A2", "B1", "B2", "C1", "C2"]
        .iter()
        .position(|v| *v == level)
        .unwrap_or(99)
}
pub async fn candidates(
    tx: &mut Tx<'_>,
    ids: &[Uuid],
    level: &str,
    profile: &str,
) -> Result<Vec<QuestionCandidate>, AppError> {
    validate_context(level, profile)?;
    let allowed:Vec<Uuid> = sqlx::query_scalar("SELECT id FROM lexicon.entries WHERE id=ANY($1) AND teaching_eligible AND archived_at IS NULL ORDER BY id FOR SHARE").bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let publications = published::read_current_in(tx, &allowed).await?;
    let mut result = vec![];
    for id in allowed {
        let Some(publication) = publications.get(&id) else {
            continue;
        };
        let content = publication.content_v3()?;
        for pos in &content.forms.pos {
            let Some(meanings) = content.meanings.pos.iter().find(|m| m.pos_id == pos.pos_id)
            else {
                continue;
            };
            for sense in &meanings.senses {
                if sense.depends_on_context {
                    continue;
                }
                let forms: Vec<_> = pos
                    .forms
                    .iter()
                    .filter(|form| {
                        form.form_type.as_str() == "base"
                            && published::allowed_form_senses(pos, &content.meanings, form.id)
                                .any(|s| s.id == sense.id)
                    })
                    .collect();
                let spelling = |form: &crate::lexicon::dto::WordConcreteFormV3| match &form
                    .regional_variants
                {
                    WordRegionalVariantsV3::Common { common } => common.spelling.trim().to_owned(),
                    WordRegionalVariantsV3::UkUs { uk, us } => {
                        if profile == "BrE" {
                            uk.spelling.trim().to_owned()
                        } else {
                            us.spelling.trim().to_owned()
                        }
                    }
                };
                let mut accepted: Vec<String> = forms
                    .iter()
                    .flat_map(|f| match &f.regional_variants {
                        WordRegionalVariantsV3::Common { common } => {
                            vec![common.spelling.trim().to_owned()]
                        }
                        WordRegionalVariantsV3::UkUs { uk, us } => {
                            vec![uk.spelling.trim().to_owned(), us.spelling.trim().to_owned()]
                        }
                    })
                    .collect();
                accepted.retain(|s| !s.is_empty());
                accepted.sort();
                accepted.dedup();
                if accepted.len() > 30 {
                    continue;
                }

                for definition in &sense.definitions {
                    let WordDefinitionV3::ZhDefinition {
                        id: definition_id,
                        content: text,
                        level: definition_level,
                        ..
                    } = definition
                    else {
                        continue;
                    };
                    if rank(definition_level) > rank(level) || text.text().trim().is_empty() {
                        continue;
                    }
                    for form in &forms {
                        let source = QuestionSource {
                            entry_id: id,
                            publication_id: publication.publication_id,
                            pos_id: pos.pos_id,
                            sense_id: sense.id,
                            definition_id: *definition_id,
                            form_id: form.id,
                        };
                        let other_definitions = content
                            .meanings
                            .pos
                            .iter()
                            .flat_map(|p| &p.senses)
                            .flat_map(|s| &s.definitions)
                            .filter_map(|d| match d {
                                WordDefinitionV3::ZhDefinition { id, content, .. }
                                    if id != definition_id =>
                                {
                                    Some(content.text().trim().to_owned())
                                }
                                _ => None,
                            })
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>();
                        if other_definitions.len() > 100 {
                            continue;
                        }
                        // Including the publication prevents rebinding a reference after republish.
                        let candidate_ref = Sha256::digest(
                            serde_json::to_vec(&source).map_err(AppError::internal)?,
                        )
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                        let mut candidate = QuestionCandidate {
                            candidate_ref,
                            source,
                            content_fingerprint: String::new(),
                            spelling: spelling(form),
                            part_of_speech: pos.pos.trim().to_owned(),
                            level: definition_level.clone(),
                            definition: text.text().trim().to_owned(),
                            other_definitions,
                            accepted_spellings: accepted.clone(),
                            answer_profile: profile.into(),
                            form_type: "base".into(),
                            teaching_eligible: true,
                        };
                        candidate.content_fingerprint = Sha256::digest(
                            serde_json::to_vec(&candidate).map_err(AppError::internal)?,
                        )
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                        result.push(candidate);
                    }
                }
            }
        }
    }
    Ok(result)
}
pub async fn read(
    tx: &mut Tx<'_>,
    item: &QuestionEntryRead,
) -> Result<QuestionCandidate, AppError> {
    let items = candidates(tx, &[item.source.entry_id], "C2", &item.answer_profile).await?;
    items
        .into_iter()
        .find(|c| c.source == item.source && c.candidate_ref == item.candidate_ref)
        .ok_or_else(source_changed)
}
pub async fn recheck(tx: &mut Tx<'_>, candidate: &QuestionCandidate) -> Result<(), AppError> {
    let fresh = read(
        tx,
        &QuestionEntryRead {
            candidate_ref: candidate.candidate_ref.clone(),
            source: candidate.source.clone(),
            answer_profile: candidate.answer_profile.clone(),
        },
    )
    .await?;
    if fresh != *candidate {
        return Err(source_changed());
    }
    Ok(())
}
pub async fn validate_question(
    tx: &mut Tx<'_>,
    content: &BankQuestionContent,
) -> Result<(), AppError> {
    validate_context(&content.question_level, &content.answer_profile)?;
    if content.prompt.trim().is_empty()
        || content.prompt.chars().count() > 2000
        || content.explanation.trim().is_empty()
        || content.explanation.chars().count() > 2000
        || content.options.len() != 4
    {
        return Err(invalid("需要题干、解析和四个选项"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut spellings = std::collections::HashSet::new();
    let mut entries = std::collections::HashSet::new();
    let mut accepted_keys = std::collections::HashSet::new();
    for option in &content.options {
        if !["A", "B", "C", "D"].contains(&option.option_id.as_str())
            || !ids.insert(&option.option_id)
            || !spellings.insert(key_v1(&option.candidate.spelling))
            || !entries.insert(option.candidate.source.entry_id)
            || option.candidate.part_of_speech != content.target.part_of_speech
            || option.explanation.trim().is_empty()
            || option.explanation.chars().count() > 2000
            || option.candidate.answer_profile != content.answer_profile
            || rank(&option.candidate.level) > rank(&content.question_level)
        {
            return Err(invalid("四个选项须来自不同词条且词形、档位与级别有效"));
        }
        let keys: std::collections::HashSet<_> = option
            .candidate
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&option.candidate.spelling))
            .map(|s| key_v1(s))
            .collect();
        if keys.iter().any(|key| accepted_keys.contains(key)) {
            return Err(invalid("选项之间存在相同的有效拼写"));
        }
        accepted_keys.extend(keys);
        if option.option_id != content.correct_option_id
            && option
                .candidate
                .other_definitions
                .iter()
                .chain(std::iter::once(&option.candidate.definition))
                .any(|d| key_v1(d) == key_v1(&content.target.definition))
        {
            return Err(invalid("干扰项也具有目标释义"));
        }
        recheck(tx, &option.candidate).await?;
    }
    let correct = content
        .options
        .iter()
        .find(|o| o.option_id == content.correct_option_id)
        .ok_or_else(|| invalid("正确项无效"))?;
    if correct.candidate != content.target {
        return Err(invalid("正确项必须是委托目标"));
    }
    if content
        .target
        .accepted_spellings
        .iter()
        .chain(std::iter::once(&content.target.spelling))
        .any(|s| crate::learning_tasks::question::leaks(&content.prompt, &key_v1(s)))
    {
        return Err(invalid("题干泄露正确词形"));
    }
    Ok(())
}

/// Published synonym links are exclusion evidence, including links from another sense.
pub async fn synonym_refs(
    tx: &mut Tx<'_>,
    entry: Uuid,
) -> Result<
    (
        std::collections::HashSet<Uuid>,
        std::collections::HashSet<String>,
    ),
    AppError,
> {
    let publications = published::read_current_in(tx, &[entry]).await?;
    let mut ids = std::collections::HashSet::new();
    let mut words = std::collections::HashSet::new();
    if let Some(p) = publications.get(&entry) {
        let content = p.content_v3()?;
        for relation in content
            .meanings
            .pos
            .iter()
            .flat_map(|p| &p.senses)
            .flat_map(|s| &s.relations)
            .filter(|r| r.relation == "synonym")
        {
            if let Some(id) = relation.target_word_id {
                ids.insert(id);
            }
            if let Some(word) = relation
                .target_headword
                .as_ref()
                .or(relation.pending_target_headword.as_ref())
            {
                words.insert(key_v1(word));
            }
        }
    }
    Ok((ids, words))
}
