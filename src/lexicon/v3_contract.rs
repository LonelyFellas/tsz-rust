use std::collections::{HashMap, HashSet};

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::{
        Dialect, DialectVariantRichTextSlotV3, DraftFormsStepContentV3, DraftMeaningsStepContentV3,
        DraftNodeLocation, DraftValidationIssue, EnglishTextV3, PersistedWordStep, RichTextV3,
        StepSaveIntent, V3DraftNodeLocation, V3DraftValidationIssue, V3ValidationIssueCode,
        WordConcreteFormV3, WordDefinitionV3, WordFormTypeV2, WordFormTypeV3,
        WordRegionalVariantsV3,
    },
    lexicon::form_types::allowed_form_types,
    lexicon::normalization::MAX_HEADWORD_CODEPOINTS,
    lexicon::validation::MAX_ENTRY_NODES,
};

pub(crate) fn request_schema_version(value: &Value) -> Result<Option<u8>, AppError> {
    let Some(raw) = value.get("schema_version") else {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "schema_version is required",
        ));
    };
    let Some(number) = raw.as_number() else {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "schema_version must be integer 2 or 3",
        ));
    };
    match number.as_u64() {
        Some(version @ (2 | 3)) => Ok(Some(version as u8)),
        Some(_) => Err(AppError::unprocessable(
            ErrorCode::UnsupportedSchemaVersion,
            "unsupported schema_version",
        )),
        None if number.as_i64().is_some() => Err(AppError::unprocessable(
            ErrorCode::UnsupportedSchemaVersion,
            "unsupported schema_version",
        )),
        _ => Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "schema_version must be integer 2 or 3",
        )),
    }
}

pub(crate) fn request_schema_version_or_legacy(value: &Value) -> Result<Option<u8>, AppError> {
    if value.get("schema_version").is_none() {
        return Ok(None);
    }
    request_schema_version(value)
}

pub(crate) fn decode_request<T: DeserializeOwned>(value: Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::unprocessable(ErrorCode::InvalidRequestBody, "invalid request body"))
}

pub(crate) fn decode_v3_forms_request<T: DeserializeOwned>(value: Value) -> Result<T, AppError> {
    let issues = raw_forms_issues(&value);
    if !issues.is_empty() {
        return Err(contract_validation_error(&issues));
    }
    decode_request(value)
}

pub(crate) fn decode_v3_meanings_request<T: DeserializeOwned>(value: Value) -> Result<T, AppError> {
    let mut issues = Vec::new();
    collect_forbidden_fields(
        &value,
        &[
            "group_id",
            "form_id",
            "headwords",
            "sort_order",
            "associations",
            "associations_state",
            "target_headword",
            "target_gloss",
            "resolved_pos",
            "resolved_form_type",
        ],
        &mut issues,
        None,
    );
    if !issues.is_empty() {
        return Err(contract_validation_error(&issues));
    }
    decode_request(value)
}

pub(crate) fn require_positive_revision(field: &'static str, value: i64) -> Result<(), AppError> {
    if value < 1 {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            field,
            format!("{field} must be at least 1"),
        ));
    }
    Ok(())
}

pub(crate) fn contract_validation_error(issues: &[DraftValidationIssue]) -> AppError {
    AppError::unprocessable(ErrorCode::ValidationFailed, "V3 contract validation failed")
        .with_v3_field_issues(issues.iter().map(v3_issue).collect())
}

pub(crate) fn validate_forms(
    content: &DraftFormsStepContentV3,
    intent: StepSaveIntent,
) -> Vec<DraftValidationIssue> {
    let complete = intent == StepSaveIntent::Complete;
    let mut issues = Vec::new();
    let mut node_roles = HashMap::<Uuid, &'static str>::new();
    let mut pos_codes = HashSet::new();
    let mut form_owners = HashMap::<Uuid, Uuid>::new();
    let mut form_types = HashMap::<Uuid, WordFormTypeV3>::new();
    let mut membership_counts = HashMap::<Uuid, usize>::new();

    if complete && content.pos.is_empty() {
        let node_id = Uuid::nil();
        issues.push(issue(
            V3ValidationIssueCode::PosRequired,
            "pos",
            node_id,
            "a complete entry requires at least one part of speech",
            location_for(node_id, None, None, None, None, None, None),
        ));
    }

    let node_count = forms_node_count(content);
    if node_count > MAX_ENTRY_NODES {
        let node_id = content.pos.first().map_or_else(Uuid::nil, |pos| pos.pos_id);
        issues.push(issue(
            V3ValidationIssueCode::ContentLimitExceeded,
            "content",
            node_id,
            "entry content exceeds the shared 2000-node limit",
            location_for(node_id, None, None, None, None, None, None),
        ));
    }

    for pos in &content.pos {
        register_node(
            &mut node_roles,
            pos.pos_id,
            "forms.pos",
            &mut issues,
            location_for(pos.pos_id, Some(pos.pos_id), None, None, None, None, None),
        );
        if !pos_codes.insert(pos.pos.as_str()) {
            issues.push(issue(
                V3ValidationIssueCode::DuplicatePosCode,
                "pos",
                pos.pos_id,
                "the same POS code cannot appear twice in one entry",
                location_for(pos.pos_id, Some(pos.pos_id), None, None, None, None, None),
            ));
        }
        if complete && pos.form_groups.is_empty() {
            issues.push(issue(
                V3ValidationIssueCode::FormGroupRequired,
                "form_groups",
                pos.pos_id,
                "a complete POS requires at least one form group",
                location_for(pos.pos_id, Some(pos.pos_id), None, None, None, None, None),
            ));
        }
        for form in &pos.forms {
            register_node(
                &mut node_roles,
                form.id,
                "forms.concrete_form",
                &mut issues,
                location_for(
                    form.id,
                    Some(pos.pos_id),
                    None,
                    None,
                    Some(form.id),
                    None,
                    None,
                ),
            );
            if let Some(previous) = form_owners.insert(form.id, pos.pos_id)
                && previous != pos.pos_id
            {
                issues.push(issue(
                    V3ValidationIssueCode::DuplicateNodeId,
                    "id",
                    form.id,
                    "a concrete form ID cannot belong to two POS nodes",
                    location_for(
                        form.id,
                        Some(pos.pos_id),
                        None,
                        None,
                        Some(form.id),
                        None,
                        None,
                    ),
                ));
            }
            let form_type = form_type_name(form.form_type);
            if form.form_type != WordFormTypeV3::Base
                && !allowed_form_types(&pos.pos).contains(&form_type)
            {
                issues.push(issue(
                    V3ValidationIssueCode::InvalidFormTypeForPartOfSpeech,
                    "form_type",
                    form.id,
                    "form_type is not allowed for this part of speech",
                    location_for(
                        form.id,
                        Some(pos.pos_id),
                        None,
                        None,
                        Some(form.id),
                        None,
                        None,
                    ),
                ));
            }
            form_types.insert(form.id, form.form_type);
            membership_counts.entry(form.id).or_default();
            validate_form_content(form, pos.pos_id, complete, &mut node_roles, &mut issues);
        }
    }

    for pos in &content.pos {
        for group in &pos.form_groups {
            let group_location = location_for(
                group.id,
                Some(pos.pos_id),
                Some(group.id),
                None,
                None,
                None,
                None,
            );
            register_node(
                &mut node_roles,
                group.id,
                "forms.form_group",
                &mut issues,
                group_location.clone(),
            );
            if complete && group.members.is_empty() {
                issues.push(issue(
                    V3ValidationIssueCode::EmptyFormGroup,
                    "members",
                    group.id,
                    "a complete entry cannot retain an empty form group",
                    group_location,
                ));
            }
            let mut group_forms = HashSet::new();
            for membership in &group.members {
                let membership_location = location_for(
                    membership.id,
                    Some(pos.pos_id),
                    Some(group.id),
                    Some(membership.id),
                    Some(membership.form_id),
                    None,
                    None,
                );
                register_node(
                    &mut node_roles,
                    membership.id,
                    "forms.group_membership",
                    &mut issues,
                    membership_location.clone(),
                );
                if !group_forms.insert(membership.form_id) {
                    issues.push(issue(
                        V3ValidationIssueCode::FormGroupMembershipInvalid,
                        "form_id",
                        membership.id,
                        "the same group cannot reference one form twice",
                        membership_location,
                    ));
                    continue;
                }
                match form_owners.get(&membership.form_id) {
                    Some(owner) if *owner == pos.pos_id => {
                        *membership_counts.entry(membership.form_id).or_default() += 1;
                    }
                    Some(_) => issues.push(issue(
                        V3ValidationIssueCode::FormGroupMembershipInvalid,
                        "form_id",
                        membership.id,
                        "a membership cannot reference a form owned by another POS",
                        membership_location,
                    )),
                    None => issues.push(issue(
                        V3ValidationIssueCode::FormGroupMembershipInvalid,
                        "form_id",
                        membership.id,
                        "membership form_id does not exist in the submitted entry",
                        membership_location,
                    )),
                }
            }
        }
    }

    for pos in &content.pos {
        for form in &pos.forms {
            if membership_counts.get(&form.id).copied().unwrap_or_default() == 0 {
                issues.push(issue(
                    V3ValidationIssueCode::OrphanForm,
                    "id",
                    form.id,
                    "every saved concrete form requires at least one membership",
                    location_for(
                        form.id,
                        Some(pos.pos_id),
                        None,
                        None,
                        Some(form.id),
                        None,
                        None,
                    ),
                ));
            }
        }
    }

    issues
}

pub(crate) fn validate_meanings(content: &DraftMeaningsStepContentV3) -> Vec<DraftValidationIssue> {
    let mut issues = Vec::new();
    let node_count = meanings_node_count(content);
    if node_count > MAX_ENTRY_NODES {
        let node_id = content
            .pos
            .first()
            .map(|pos| pos.pos_id)
            .or_else(|| content.sense_groups.first().map(|group| group.id))
            .unwrap_or_else(Uuid::nil);
        issues.push(meanings_limit_issue(
            node_id,
            "content",
            "entry content exceeds the shared 2000-node limit",
        ));
    }
    for group in &content.sense_groups {
        for (field, value) in [("name_zh", &group.name_zh), ("name_en", &group.name_en)] {
            if value.chars().count() > MAX_HEADWORD_CODEPOINTS {
                issues.push(meanings_limit_issue(
                    group.id,
                    field,
                    "sense group name exceeds the shared 200-codepoint limit",
                ));
            }
        }
    }
    for pos in &content.pos {
        for grammar in &pos.grammar_structures {
            for variant in &grammar.variants {
                validate_rich_text_limits(&variant.content, variant.id, "content", &mut issues);
            }
        }
        for sense in &pos.senses {
            for definition in &sense.definitions {
                match definition {
                    WordDefinitionV3::ZhDefinition { id, content, .. }
                    | WordDefinitionV3::ZhSentence { id, content, .. } => {
                        validate_rich_text_limits(content, *id, "content", &mut issues);
                    }
                    WordDefinitionV3::EnDefinition { content, .. }
                    | WordDefinitionV3::EnSentence { content, .. } => {
                        validate_english_text_limits(content, &mut issues);
                    }
                }
            }
            for sentence in &sense.sentences {
                validate_english_text_limits(&sentence.en_text, &mut issues);
                validate_rich_text_limits(
                    &sentence.zh_text,
                    sentence.zh_text_id,
                    "zh_text",
                    &mut issues,
                );
            }
        }
    }
    issues
}

pub(crate) fn validate_aggregate_node_limit(
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
) -> Vec<DraftValidationIssue> {
    let form_pos_ids = forms
        .pos
        .iter()
        .map(|pos| pos.pos_id)
        .collect::<HashSet<_>>();
    let shared_pos_nodes = meanings
        .pos
        .iter()
        .filter(|pos| form_pos_ids.contains(&pos.pos_id))
        .count();
    let node_count = forms_node_count(forms)
        .saturating_add(meanings_node_count(meanings))
        .saturating_sub(shared_pos_nodes);
    if node_count <= MAX_ENTRY_NODES {
        return Vec::new();
    }
    let node_id = forms
        .pos
        .first()
        .map(|pos| pos.pos_id)
        .or_else(|| meanings.pos.first().map(|pos| pos.pos_id))
        .or_else(|| meanings.sense_groups.first().map(|group| group.id))
        .unwrap_or_else(Uuid::nil);
    vec![issue(
        V3ValidationIssueCode::ContentLimitExceeded,
        "content",
        node_id,
        "entry content exceeds the shared 2000-node limit",
        location_for(node_id, None, None, None, None, None, None),
    )]
}

fn forms_node_count(content: &DraftFormsStepContentV3) -> usize {
    content
        .pos
        .iter()
        .map(|pos| {
            1 + pos.forms.len()
                + pos
                    .forms
                    .iter()
                    .map(|form| match &form.regional_variants {
                        WordRegionalVariantsV3::Common { common } => {
                            1 + common.pronunciations.len()
                        }
                        WordRegionalVariantsV3::UkUs { uk, us } => {
                            2 + uk.pronunciations.len() + us.pronunciations.len()
                        }
                    })
                    .sum::<usize>()
                + pos.form_groups.len()
                + pos
                    .form_groups
                    .iter()
                    .map(|group| group.members.len())
                    .sum::<usize>()
        })
        .sum::<usize>()
}

fn meanings_node_count(content: &DraftMeaningsStepContentV3) -> usize {
    content.sense_groups.len()
        + content
            .pos
            .iter()
            .map(|pos| {
                1 + pos.grammar_structures.len()
                    + pos
                        .grammar_structures
                        .iter()
                        .map(|grammar| grammar.variants.len())
                        .sum::<usize>()
                    + pos.senses.len()
                    + pos
                        .senses
                        .iter()
                        .map(|sense| {
                            sense.definitions.len()
                                + sense.sentences.len()
                                + sense
                                    .sentences
                                    .iter()
                                    .map(|sentence| {
                                        sentence.links.len() + sentence.associations.len()
                                    })
                                    .sum::<usize>()
                                + sense.relations.len()
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
}

fn validate_english_text_limits(content: &EnglishTextV3, issues: &mut Vec<DraftValidationIssue>) {
    match content {
        EnglishTextV3::Unified { common } => {
            validate_rich_text_limits(&common.value, common.id, "value", issues);
        }
        EnglishTextV3::Distinguish { uk, us, .. } => {
            for slot in [uk, us] {
                if let DialectVariantRichTextSlotV3::Ready { variant } = slot {
                    validate_rich_text_limits(&variant.value, variant.id, "value", issues);
                }
            }
        }
    }
}

fn validate_rich_text_limits(
    content: &RichTextV3,
    node_id: Uuid,
    field: &str,
    issues: &mut Vec<DraftValidationIssue>,
) {
    if content.text().chars().count() > MAX_HEADWORD_CODEPOINTS {
        issues.push(meanings_limit_issue(
            node_id,
            field,
            "rich text exceeds the shared 200-codepoint limit",
        ));
    }
    if content.decoration_count() > MAX_ENTRY_NODES {
        issues.push(meanings_limit_issue(
            node_id,
            field,
            "rich text decorations exceed the shared 2000-node limit",
        ));
    }
}

fn meanings_limit_issue(node_id: Uuid, field: &str, message: &str) -> DraftValidationIssue {
    DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id,
        field: field.to_owned(),
        code: V3ValidationIssueCode::ContentLimitExceeded
            .as_str()
            .to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: Some(DraftNodeLocation {
            node_role: "meanings".to_owned(),
            pos: None,
            pos_id: None,
            form_group_index: None,
            form_group_id: None,
            membership_id: None,
            form_id: None,
            variant_id: None,
            pronunciation_id: None,
            form_type: None,
            dialect: None,
            ancestor_node_ids: Vec::new(),
        }),
    }
}

fn validate_form_content(
    form: &WordConcreteFormV3,
    pos_id: Uuid,
    complete: bool,
    node_roles: &mut HashMap<Uuid, &'static str>,
    issues: &mut Vec<DraftValidationIssue>,
) {
    match &form.regional_variants {
        WordRegionalVariantsV3::Common { common } => validate_variant(
            form.id,
            pos_id,
            common.id,
            Dialect::Common,
            &common.spelling,
            &common.pronunciations,
            complete,
            node_roles,
            issues,
        ),
        WordRegionalVariantsV3::UkUs { uk, us } => {
            validate_variant(
                form.id,
                pos_id,
                uk.id,
                Dialect::Uk,
                &uk.spelling,
                &uk.pronunciations,
                complete,
                node_roles,
                issues,
            );
            validate_variant(
                form.id,
                pos_id,
                us.id,
                Dialect::Us,
                &us.spelling,
                &us.pronunciations,
                complete,
                node_roles,
                issues,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_variant(
    form_id: Uuid,
    pos_id: Uuid,
    variant_id: Uuid,
    dialect: Dialect,
    spelling: &str,
    pronunciations: &[crate::lexicon::dto::WordPronunciationV3],
    complete: bool,
    node_roles: &mut HashMap<Uuid, &'static str>,
    issues: &mut Vec<DraftValidationIssue>,
) {
    let variant_location = location_for(
        variant_id,
        Some(pos_id),
        None,
        None,
        Some(form_id),
        Some((variant_id, dialect)),
        None,
    );
    register_node(
        node_roles,
        variant_id,
        "forms.form_variant",
        issues,
        variant_location.clone(),
    );
    if complete && spelling.trim().is_empty() {
        issues.push(issue(
            V3ValidationIssueCode::VariantSpellingRequired,
            "spelling",
            variant_id,
            "a complete regional variant requires spelling",
            variant_location.clone(),
        ));
    }
    if spelling.chars().count() > MAX_HEADWORD_CODEPOINTS {
        issues.push(issue(
            V3ValidationIssueCode::ContentLimitExceeded,
            "spelling",
            variant_id,
            "variant spelling exceeds the shared 200-codepoint limit",
            variant_location.clone(),
        ));
    }
    if complete && pronunciations.is_empty() {
        issues.push(issue(
            V3ValidationIssueCode::PronunciationRequired,
            "pronunciations",
            variant_id,
            "a complete regional variant requires at least one pronunciation",
            variant_location,
        ));
    }
    let mut complete_triples = HashSet::new();
    for pronunciation in pronunciations {
        let pronunciation_location = location_for(
            pronunciation.id,
            Some(pos_id),
            None,
            None,
            Some(form_id),
            Some((variant_id, dialect)),
            Some(pronunciation.id),
        );
        register_node(
            node_roles,
            pronunciation.id,
            "forms.pronunciation",
            issues,
            pronunciation_location.clone(),
        );
        for (field, value) in [
            ("dict_phonetic", pronunciation.dict_phonetic.as_str()),
            ("actual_pron", pronunciation.actual_pron.as_str()),
        ] {
            if value.chars().count() > MAX_HEADWORD_CODEPOINTS {
                issues.push(issue(
                    V3ValidationIssueCode::ContentLimitExceeded,
                    field,
                    pronunciation.id,
                    "pronunciation field exceeds the shared 200-codepoint limit",
                    pronunciation_location.clone(),
                ));
            }
        }
        let complete_row = !pronunciation.dict_phonetic.trim().is_empty()
            && !pronunciation.actual_pron.trim().is_empty()
            && pronunciation.style.is_some();
        if complete && !complete_row {
            issues.push(issue(
                V3ValidationIssueCode::PronunciationRequired,
                if pronunciation.dict_phonetic.trim().is_empty() {
                    "dict_phonetic"
                } else if pronunciation.actual_pron.trim().is_empty() {
                    "actual_pron"
                } else {
                    "style"
                },
                pronunciation.id,
                "a complete pronunciation requires all fields",
                pronunciation_location.clone(),
            ));
        }
        if complete_row {
            let triple = (
                normalize_pronunciation_text(&pronunciation.dict_phonetic),
                normalize_pronunciation_text(&pronunciation.actual_pron),
                pronunciation.style.expect("complete rows have style"),
            );
            if !complete_triples.insert(triple) {
                issues.push(issue(
                    V3ValidationIssueCode::DuplicatePronunciation,
                    "pronunciations",
                    pronunciation.id,
                    "complete normalized pronunciation triples must be unique per variant",
                    pronunciation_location,
                ));
            }
        }
    }
}

pub(crate) fn v3_issue(issue: &DraftValidationIssue) -> V3DraftValidationIssue {
    let location = issue.node_location.as_ref();
    V3DraftValidationIssue {
        schema_version: 3,
        step: issue.step,
        node_id: issue.node_id,
        field: issue.field.clone(),
        code: V3ValidationIssueCode::from_wire(&issue.code)
            .expect("V3 contract validators only emit the closed V3 issue catalog"),
        message: issue.message.clone(),
        node_location: V3DraftNodeLocation {
            node_role: location
                .map_or_else(|| "entry".to_owned(), |location| location.node_role.clone()),
            ancestor_node_ids: location
                .map_or_else(Vec::new, |location| location.ancestor_node_ids.clone()),
            pos_id: location.and_then(|location| location.pos_id),
            form_group_id: location.and_then(|location| location.form_group_id),
            membership_id: location.and_then(|location| location.membership_id),
            form_id: location.and_then(|location| location.form_id),
            variant_id: location.and_then(|location| location.variant_id),
            pronunciation_id: location.and_then(|location| location.pronunciation_id),
            form_type: location
                .and_then(|location| location.form_type)
                .map(v3_form_type),
            dialect: location.and_then(|location| location.dialect),
        },
    }
}

pub(crate) fn v3_issues(issues: &[DraftValidationIssue]) -> Vec<V3DraftValidationIssue> {
    issues.iter().map(v3_issue).collect()
}

const fn v3_form_type(value: WordFormTypeV2) -> WordFormTypeV3 {
    match value {
        WordFormTypeV2::Base => WordFormTypeV3::Base,
        WordFormTypeV2::ThirdPersonSingular => WordFormTypeV3::ThirdPersonSingular,
        WordFormTypeV2::PresentParticiple => WordFormTypeV3::PresentParticiple,
        WordFormTypeV2::PastTense => WordFormTypeV3::PastTense,
        WordFormTypeV2::PastParticiple => WordFormTypeV3::PastParticiple,
        WordFormTypeV2::Plural => WordFormTypeV3::Plural,
        WordFormTypeV2::Comparative => WordFormTypeV3::Comparative,
        WordFormTypeV2::Superlative => WordFormTypeV3::Superlative,
    }
}

const fn form_type_name(value: WordFormTypeV3) -> &'static str {
    match value {
        WordFormTypeV3::Base => "base",
        WordFormTypeV3::ThirdPersonSingular => "third_person_singular",
        WordFormTypeV3::PresentParticiple => "present_participle",
        WordFormTypeV3::PastTense => "past_tense",
        WordFormTypeV3::PastParticiple => "past_participle",
        WordFormTypeV3::Plural => "plural",
        WordFormTypeV3::Comparative => "comparative",
        WordFormTypeV3::Superlative => "superlative",
    }
}

fn normalize_pronunciation_text(value: &str) -> String {
    value.nfkc().collect::<String>().trim().to_lowercase()
}

fn register_node(
    roles: &mut HashMap<Uuid, &'static str>,
    id: Uuid,
    role: &'static str,
    issues: &mut Vec<DraftValidationIssue>,
    location: DraftNodeLocation,
) {
    if let Some(previous) = roles.insert(id, role) {
        issues.push(issue(
            V3ValidationIssueCode::DuplicateNodeId,
            "id",
            id,
            if previous == role {
                "the same stable UUID appears more than once"
            } else {
                "the same stable UUID is used for different node roles"
            },
            location,
        ));
    }
}

fn issue(
    code: V3ValidationIssueCode,
    field: &str,
    node_id: Uuid,
    message: &str,
    location: DraftNodeLocation,
) -> DraftValidationIssue {
    DraftValidationIssue {
        step: PersistedWordStep::Forms,
        node_id,
        field: field.to_owned(),
        code: code.as_str().to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: Some(location),
    }
}

#[allow(clippy::too_many_arguments)]
fn location_for(
    node_id: Uuid,
    pos_id: Option<Uuid>,
    form_group_id: Option<Uuid>,
    membership_id: Option<Uuid>,
    form_id: Option<Uuid>,
    variant: Option<(Uuid, Dialect)>,
    pronunciation_id: Option<Uuid>,
) -> DraftNodeLocation {
    let mut ancestors = [pos_id, form_group_id, form_id, variant.map(|item| item.0)]
        .into_iter()
        .flatten()
        .filter(|id| *id != node_id)
        .collect::<Vec<_>>();
    ancestors.dedup();
    DraftNodeLocation {
        node_role: if pronunciation_id.is_some() {
            "forms.pronunciation"
        } else if variant.is_some() {
            "forms.form_variant"
        } else if membership_id.is_some() {
            "forms.group_membership"
        } else if form_id.is_some() {
            "forms.concrete_form"
        } else if form_group_id.is_some() {
            "forms.form_group"
        } else {
            "forms.pos"
        }
        .to_owned(),
        pos: None,
        pos_id,
        form_group_index: None,
        form_group_id,
        membership_id,
        form_id,
        variant_id: variant.map(|item| item.0),
        pronunciation_id,
        form_type: None,
        dialect: variant.map(|item| item.1),
        ancestor_node_ids: ancestors,
    }
}

fn raw_forms_issues(value: &Value) -> Vec<DraftValidationIssue> {
    let mut issues = Vec::new();
    collect_forbidden_fields(
        value,
        &[
            "base_form",
            "parent_form_id",
            "derived_from_form_id",
            "sort_order",
            "headwords",
            "presentation",
            "compatibility",
        ],
        &mut issues,
        None,
    );
    let Some(pos_items) = value
        .get("content")
        .and_then(|content| content.get("pos"))
        .and_then(Value::as_array)
    else {
        return issues;
    };
    for pos in pos_items {
        let pos_id = uuid_field(pos, "pos_id");
        let Some(forms) = pos.get("forms").and_then(Value::as_array) else {
            continue;
        };
        for form in forms {
            let form_id = uuid_field(form, "id").unwrap_or_else(Uuid::nil);
            if let Some(form_type) = form.get("form_type").and_then(Value::as_str)
                && !is_known_form_type(form_type)
            {
                issues.push(issue(
                    V3ValidationIssueCode::InvalidFormTypeForPartOfSpeech,
                    "form_type",
                    form_id,
                    "form_type must be a Phase 1 catalog enum value",
                    location_for(form_id, pos_id, None, None, Some(form_id), None, None),
                ));
            }
            let regional = form.get("regional_variants");
            if !valid_regional_shape(regional) {
                issues.push(issue(
                    V3ValidationIssueCode::InvalidRegionalVariantShape,
                    "regional_variants",
                    form_id,
                    "regional_variants must be common xor complete uk_us",
                    location_for(form_id, pos_id, None, None, Some(form_id), None, None),
                ));
            }
        }
    }
    issues
}

fn valid_regional_shape(value: Option<&Value>) -> bool {
    let Some(object) = value.and_then(Value::as_object) else {
        return false;
    };
    match object.get("mode").and_then(Value::as_str) {
        Some("common") => {
            object.get("common").is_some_and(|variant| {
                variant.get("dialect").and_then(Value::as_str) == Some("common")
            }) && !object.contains_key("uk")
                && !object.contains_key("us")
        }
        Some("uk_us") => {
            object
                .get("uk")
                .is_some_and(|variant| variant.get("dialect").and_then(Value::as_str) == Some("uk"))
                && object.get("us").is_some_and(|variant| {
                    variant.get("dialect").and_then(Value::as_str) == Some("us")
                })
                && !object.contains_key("common")
        }
        _ => false,
    }
}

fn is_known_form_type(value: &str) -> bool {
    matches!(
        value,
        "base"
            | "third_person_singular"
            | "present_participle"
            | "past_tense"
            | "past_participle"
            | "plural"
            | "comparative"
            | "superlative"
    )
}

fn collect_forbidden_fields(
    value: &Value,
    forbidden: &[&str],
    issues: &mut Vec<DraftValidationIssue>,
    context: Option<Uuid>,
) {
    match value {
        Value::Object(object) => collect_forbidden_object(object, forbidden, issues, context),
        Value::Array(items) => {
            for item in items {
                collect_forbidden_fields(item, forbidden, issues, context);
            }
        }
        _ => {}
    }
}

fn collect_forbidden_object(
    object: &Map<String, Value>,
    forbidden: &[&str],
    issues: &mut Vec<DraftValidationIssue>,
    context: Option<Uuid>,
) {
    let node_id = object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .or(context);
    for (field, nested) in object {
        if forbidden.contains(&field.as_str()) {
            let node_id = node_id.unwrap_or_else(Uuid::nil);
            issues.push(issue(
                V3ValidationIssueCode::ForbiddenV3Field,
                field,
                node_id,
                "field is not part of the V3 writable contract",
                location_for(node_id, None, None, None, None, None, None),
            ));
        }
        collect_forbidden_fields(nested, forbidden, issues, node_id);
    }
}

fn uuid_field(value: &Value, field: &str) -> Option<Uuid> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lexicon::dto::SaveFormsStepInputV3;

    // C1 selection from the approved matrix:
    // V3-U01/02/03/04/06/06a/06b/06c/07/07b/08/10b/11a/11b/11c.
    // Database migration and successful V3 persistence remain C2 and are not simulated here.
    fn valid_request() -> Value {
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "content": {
                "pos": [{
                    "pos_id": "019d2a80-0000-7000-8000-000000000001",
                    "pos": "noun",
                    "forms": [{
                        "id": "019d2a80-0000-7000-8000-000000000002",
                        "form_type": "base",
                        "regional_variants": {
                            "mode": "common",
                            "common": {
                                "id": "019d2a80-0000-7000-8000-000000000003",
                                "dialect": "common",
                                "spelling": "colour",
                                "origin": "manual",
                                "pronunciations": [{
                                    "id": "019d2a80-0000-7000-8000-000000000004",
                                    "dict_phonetic": "/kala/",
                                    "actual_pron": "kala",
                                    "style": "normal"
                                }]
                            }
                        }
                    }],
                    "form_groups": [{
                        "id": "019d2a80-0000-7000-8000-000000000005",
                        "is_regular": true,
                        "members": [{
                            "id": "019d2a80-0000-7000-8000-000000000006",
                            "form_id": "019d2a80-0000-7000-8000-000000000002"
                        }]
                    }, {
                        "id": "019d2a80-0000-7000-8000-000000000007",
                        "is_regular": false,
                        "members": [{
                            "id": "019d2a80-0000-7000-8000-000000000008",
                            "form_id": "019d2a80-0000-7000-8000-000000000002"
                        }]
                    }]
                }]
            }
        })
    }

    fn decode_valid(value: Value) -> SaveFormsStepInputV3 {
        decode_v3_forms_request(value).expect("valid V3 forms request should decode")
    }

    fn has_code(issues: &[DraftValidationIssue], code: V3ValidationIssueCode) -> bool {
        issues.iter().any(|issue| issue.code == code.as_str())
    }

    #[test]
    fn one_form_can_belong_to_multiple_groups_and_wire_order_is_preserved() {
        let input = decode_valid(valid_request());
        assert!(validate_forms(&input.content, input.intent).is_empty());

        let encoded = serde_json::to_value(&input).expect("V3 request should serialize");
        let members = encoded["content"]["pos"][0]["form_groups"]
            .as_array()
            .expect("groups should remain an ordered array");
        assert_eq!(members[0]["id"], "019d2a80-0000-7000-8000-000000000005");
        assert_eq!(members[1]["id"], "019d2a80-0000-7000-8000-000000000007");
        for forbidden in [
            "base_form",
            "parent_form_id",
            "derived_from_form_id",
            "sort_order",
            "headwords",
        ] {
            assert!(!encoded.to_string().contains(forbidden));
        }

        let mut repeated_type = valid_request();
        let mut second_form = repeated_type["content"]["pos"][0]["forms"][0].clone();
        second_form["id"] = json!("019d2a80-0000-7000-8000-000000000011");
        second_form["regional_variants"]["common"]["id"] =
            json!("019d2a80-0000-7000-8000-000000000012");
        second_form["regional_variants"]["common"]["pronunciations"][0]["id"] =
            json!("019d2a80-0000-7000-8000-000000000013");
        repeated_type["content"]["pos"][0]["forms"]
            .as_array_mut()
            .unwrap()
            .push(second_form);
        repeated_type["content"]["pos"][0]["form_groups"][0]["members"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "019d2a80-0000-7000-8000-000000000014",
                "form_id": "019d2a80-0000-7000-8000-000000000011"
            }));
        let repeated_type = decode_valid(repeated_type);
        assert!(
            validate_forms(&repeated_type.content, repeated_type.intent).is_empty(),
            "同 POS/同 group 内多个 base 和同拼写 form 都应合法"
        );
    }

    #[test]
    fn form_type_must_match_the_authoritative_part_of_speech_catalog() {
        for (pos, form_type) in [
            ("noun", "comparative"),
            ("verb", "plural"),
            ("preposition", "superlative"),
        ] {
            let mut request = valid_request();
            request["content"]["pos"][0]["pos"] = json!(pos);
            request["content"]["pos"][0]["forms"][0]["form_type"] = json!(form_type);
            let input = decode_valid(request);
            let issues = validate_forms(&input.content, StepSaveIntent::Save);
            let issue = issues
                .iter()
                .find(|issue| {
                    issue.code == V3ValidationIssueCode::InvalidFormTypeForPartOfSpeech.as_str()
                })
                .unwrap_or_else(|| panic!("{pos} + {form_type} must be rejected"));
            assert_eq!(issue.field, "form_type");
            assert_eq!(
                issue
                    .node_location
                    .as_ref()
                    .and_then(|location| location.form_id),
                Some(issue.node_id)
            );
        }

        for (pos, form_type) in [
            ("noun", "base"),
            ("noun", "plural"),
            ("verb", "past_tense"),
            ("adjective", "comparative"),
            ("adverb", "superlative"),
        ] {
            let mut request = valid_request();
            request["content"]["pos"][0]["pos"] = json!(pos);
            request["content"]["pos"][0]["forms"][0]["form_type"] = json!(form_type);
            let input = decode_valid(request);
            assert!(
                validate_forms(&input.content, StepSaveIntent::Save).is_empty(),
                "{pos} + {form_type} must remain valid"
            );
        }
    }

    #[test]
    fn duplicate_membership_and_cross_pos_membership_are_rejected_with_stable_locations() {
        let mut duplicate = valid_request();
        duplicate["content"]["pos"][0]["form_groups"][0]["members"] = json!([{
            "id": "019d2a80-0000-7000-8000-000000000006",
            "form_id": "019d2a80-0000-7000-8000-000000000002"
        }, {
            "id": "019d2a80-0000-7000-8000-000000000009",
            "form_id": "019d2a80-0000-7000-8000-000000000002"
        }]);
        let duplicate = decode_valid(duplicate);
        let issues = validate_forms(&duplicate.content, duplicate.intent);
        let issue = issues
            .iter()
            .find(|issue| issue.code == "form_group_membership_invalid")
            .expect("duplicate membership should be rejected");
        let location = issue
            .node_location
            .as_ref()
            .expect("membership issue should include a stable location");
        assert_eq!(location.membership_id, Some(issue.node_id));
        assert!(location.form_id.is_some());
        assert!(location.form_group_id.is_some());

        let mut cross_pos = valid_request();
        let form = cross_pos["content"]["pos"][0]["forms"][0].clone();
        cross_pos["content"]["pos"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "pos_id": "019d2a80-0000-7000-8000-000000000010",
                "pos": "verb",
                "forms": [form],
                "form_groups": []
            }));
        cross_pos["content"]["pos"][0]["forms"] = json!([]);
        let cross_pos = decode_valid(cross_pos);
        let issues = validate_forms(&cross_pos.content, StepSaveIntent::Save);
        assert!(has_code(
            &issues,
            V3ValidationIssueCode::FormGroupMembershipInvalid
        ));
    }

    #[test]
    fn draft_allows_zero_or_empty_groups_but_complete_rejects_orphans_and_empty_groups() {
        let no_pos = DraftFormsStepContentV3 { pos: Vec::new() };
        assert!(validate_forms(&no_pos, StepSaveIntent::Save).is_empty());
        assert!(has_code(
            &validate_forms(&no_pos, StepSaveIntent::Complete),
            V3ValidationIssueCode::PosRequired
        ));

        let empty_draft = DraftFormsStepContentV3 {
            pos: vec![crate::lexicon::dto::WordPosFormsV3 {
                pos_id: Uuid::now_v7(),
                pos: "noun".to_owned(),
                forms: Vec::new(),
                form_groups: Vec::new(),
            }],
        };
        assert!(validate_forms(&empty_draft, StepSaveIntent::Save).is_empty());
        assert!(has_code(
            &validate_forms(&empty_draft, StepSaveIntent::Complete),
            V3ValidationIssueCode::FormGroupRequired
        ));

        let mut orphan = valid_request();
        orphan["content"]["pos"][0]["form_groups"] = json!([{
            "id": "019d2a80-0000-7000-8000-000000000005",
            "is_regular": true,
            "members": []
        }]);
        let orphan = decode_valid(orphan);
        let issues = validate_forms(&orphan.content, orphan.intent);
        assert!(has_code(&issues, V3ValidationIssueCode::EmptyFormGroup));
        assert!(has_code(&issues, V3ValidationIssueCode::OrphanForm));
    }

    #[test]
    fn regional_shape_form_type_and_legacy_fields_fail_closed() {
        let mut missing_us = valid_request();
        missing_us["content"]["pos"][0]["forms"][0]["regional_variants"] = json!({
            "mode": "uk_us",
            "uk": {
                "id": "019d2a80-0000-7000-8000-000000000003",
                "dialect": "uk",
                "spelling": "colour",
                "origin": "manual",
                "pronunciations": []
            }
        });
        let error = decode_v3_forms_request::<SaveFormsStepInputV3>(missing_us)
            .expect_err("draft uk_us must contain both stable nodes");
        assert_eq!(error.code(), ErrorCode::ValidationFailed);

        let mut mixed = valid_request();
        mixed["content"]["pos"][0]["forms"][0]["regional_variants"]["uk"] = json!({});
        assert!(decode_v3_forms_request::<SaveFormsStepInputV3>(mixed).is_err());

        for (field, value) in [
            ("form_type", json!("custom_inflection")),
            ("sort_order", json!(1)),
            ("headwords", json!({"mode": "unified", "common": "colour"})),
        ] {
            let mut request = valid_request();
            request["content"]["pos"][0]["forms"][0][field] = value;
            assert!(
                decode_v3_forms_request::<SaveFormsStepInputV3>(request).is_err(),
                "{field} must fail closed"
            );
        }
    }

    #[test]
    fn duplicate_complete_pronunciation_is_normalized_but_incomplete_draft_rows_are_ignored() {
        let mut duplicate = valid_request();
        duplicate["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"] = json!([{
            "id": "019d2a80-0000-7000-8000-000000000004",
            "dict_phonetic": " /KALA/ ",
            "actual_pron": "ＫＡＬＡ",
            "style": "normal"
        }, {
            "id": "019d2a80-0000-7000-8000-000000000009",
            "dict_phonetic": "/kala/",
            "actual_pron": "kala",
            "style": "normal"
        }]);
        let duplicate = decode_valid(duplicate);
        assert!(has_code(
            &validate_forms(&duplicate.content, duplicate.intent),
            V3ValidationIssueCode::DuplicatePronunciation
        ));

        let mut incomplete = valid_request();
        incomplete["intent"] = json!("save");
        incomplete["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"] = json!([{
            "id": "019d2a80-0000-7000-8000-000000000004",
            "dict_phonetic": "",
            "actual_pron": "",
            "style": "normal"
        }, {
            "id": "019d2a80-0000-7000-8000-000000000009",
            "dict_phonetic": "",
            "actual_pron": "",
            "style": "normal"
        }]);
        let incomplete = decode_valid(incomplete);
        assert!(!has_code(
            &validate_forms(&incomplete.content, incomplete.intent),
            V3ValidationIssueCode::DuplicatePronunciation
        ));
    }

    #[test]
    fn draft_can_omit_style_but_complete_reports_pronunciation_required_at_the_row() {
        let mut request = valid_request();
        request["intent"] = json!("save");
        request["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
            [0]
        .as_object_mut()
        .unwrap()
        .remove("style");
        let draft = decode_valid(request);
        assert!(validate_forms(&draft.content, draft.intent).is_empty());

        let issues = validate_forms(&draft.content, StepSaveIntent::Complete);
        let issue = issues
            .iter()
            .find(|issue| issue.field == "style")
            .expect("complete must locate the missing style");
        assert_eq!(
            issue.code,
            V3ValidationIssueCode::PronunciationRequired.as_str()
        );
        assert_eq!(
            issue
                .node_location
                .as_ref()
                .and_then(|location| location.pronunciation_id),
            Some(issue.node_id)
        );

        let mut null_style = valid_request();
        null_style["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
            [0]["style"] = Value::Null;
        assert!(
            decode_v3_forms_request::<SaveFormsStepInputV3>(null_style).is_err(),
            "formal draft wire represents unselected style by omission, not null"
        );
    }

    #[test]
    fn shared_text_and_node_limits_are_checked_before_the_storage_gate() {
        let mut boundary = valid_request();
        boundary["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["spelling"] =
            json!("a".repeat(MAX_HEADWORD_CODEPOINTS));
        let boundary = decode_valid(boundary);
        assert!(!has_code(
            &validate_forms(&boundary.content, boundary.intent),
            V3ValidationIssueCode::ContentLimitExceeded
        ));

        let mut too_long = valid_request();
        too_long["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["spelling"] =
            json!("a".repeat(MAX_HEADWORD_CODEPOINTS + 1));
        let too_long = decode_valid(too_long);
        assert!(has_code(
            &validate_forms(&too_long.content, too_long.intent),
            V3ValidationIssueCode::ContentLimitExceeded
        ));

        let mut oversized = decode_valid(valid_request());
        let pronunciation = oversized.content.pos[0].forms[0].regional_variants.clone();
        let WordRegionalVariantsV3::Common { common } = pronunciation else {
            unreachable!()
        };
        let template = common.pronunciations[0].clone();
        let WordRegionalVariantsV3::Common { common } =
            &mut oversized.content.pos[0].forms[0].regional_variants
        else {
            unreachable!()
        };
        while common.pronunciations.len() <= MAX_ENTRY_NODES {
            let mut row = template.clone();
            row.id = Uuid::now_v7();
            row.dict_phonetic = format!("/{} /", common.pronunciations.len());
            row.actual_pron = common.pronunciations.len().to_string();
            common.pronunciations.push(row);
        }
        assert!(has_code(
            &validate_forms(&oversized.content, StepSaveIntent::Save),
            V3ValidationIssueCode::ContentLimitExceeded
        ));
    }

    #[test]
    fn v3_meanings_reject_extra_keys_without_tightening_the_v2_dto() {
        let value = json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": {
                "sense_groups": [],
                "pos": [],
                "unexpected": true
            }
        });
        assert!(
            decode_v3_meanings_request::<crate::lexicon::dto::SaveMeaningsStepInputV3>(value)
                .is_err()
        );
        assert!(
            serde_json::from_value::<crate::lexicon::dto::DraftMeaningsStepContent>(json!({
                "sense_groups": [],
                "pos": [],
                "unexpected": true
            }))
            .is_ok(),
            "legacy V2 decoder must remain backward-compatible"
        );

        let mut oversized: crate::lexicon::dto::SaveMeaningsStepInputV3 =
            decode_v3_meanings_request(json!({
                "schema_version": 3,
                "base_revision": 1,
                "intent": "save",
                "content": {"sense_groups": [], "pos": []}
            }))
            .unwrap();
        oversized.content.sense_groups = (0..=MAX_ENTRY_NODES)
            .map(|index| crate::lexicon::dto::SenseGroupV3 {
                id: Uuid::now_v7(),
                name_zh: index.to_string(),
                name_en: index.to_string(),
            })
            .collect();
        assert!(has_code(
            &validate_meanings(&oversized.content),
            V3ValidationIssueCode::ContentLimitExceeded
        ));
    }

    #[test]
    fn schema_version_shape_and_unknown_integers_fail_with_distinct_codes() {
        for raw in [json!("3"), json!(null), json!(true), json!(3.0)] {
            let error = request_schema_version(&json!({"schema_version": raw}))
                .expect_err("non-integer schema version must fail");
            assert_eq!(error.code(), ErrorCode::InvalidRequestBody);
        }
        for raw in [json!(-1), json!(256), json!(1), json!(4)] {
            let value = json!({"schema_version": raw});
            let error = request_schema_version(&value).expect_err("unknown version must fail");
            assert_eq!(error.code(), ErrorCode::UnsupportedSchemaVersion);
        }
        let error = request_schema_version(&json!({}))
            .expect_err("missing schema version must fail strict parsing");
        assert_eq!(error.code(), ErrorCode::InvalidRequestBody);
        assert_eq!(request_schema_version_or_legacy(&json!({})).unwrap(), None);
        assert_eq!(
            request_schema_version(&json!({"schema_version": 2})).unwrap(),
            Some(2)
        );
        assert_eq!(
            request_schema_version(&json!({"schema_version": 3})).unwrap(),
            Some(3)
        );
    }
}
