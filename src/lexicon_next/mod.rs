//! Independent language-aware drafts; no legacy identities.
pub mod assessment;
pub(crate) mod assessment_document;
mod audio;
mod definitions;
pub mod detection;
pub mod dialect_pairs;
pub mod entry_matches;
mod equivalents;
mod forms;
pub mod generation;
mod grammar;
pub mod language;
pub mod list_model;
pub mod model;
pub mod publication_model;
pub mod publications;
mod relation_candidates;
mod relations;
mod repository;
mod sentence_details;
mod sentence_drafts;
pub mod sentence_model;
pub mod sentences;
pub mod test_fixtures;
pub(crate) mod test_purge;
mod workspace;

use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    lexicon::normalization::normalize_headword,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::get,
};
use list_model::*;
use model::*;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    let vary = axum::middleware::map_response(async |mut response: axum::response::Response| {
        response.headers_mut().append(
            axum::http::header::VARY,
            axum::http::HeaderValue::from_static("X-TSZ-Next-Form-Questions"),
        );
        response
    });
    Router::new()
        .route("/workspace/entries", get(workspace_list))
        .route("/workspace/entries/{id}", get(workspace_read))
        .route(
            "/workspace/entries/{id}/annotation",
            axum::routing::patch(workspace_annotation),
        )
        .route("/workspace/stats", get(workspace_stats))
        .route("/workspace/creators", get(workspace_creators))
        .route("/workspace/updaters", get(workspace_updaters))
        .route(
            "/workspace/commands",
            axum::routing::post(workspace_command),
        )
        .route("/capabilities", get(capabilities).layer(vary.clone()))
        .route("/draft-schema", get(draft_schema))
        .route(
            "/test-fixtures/dialect-pair",
            axum::routing::post(test_fixtures::create),
        )
        .route(
            "/test-resources/purge",
            axum::routing::post(test_purge::purge),
        )
        .route("/equivalent-catalog", get(equivalent_catalog))
        .route("/sentences", get(sentences::list).post(sentences::create))
        .route("/sentences/{id}", get(sentences::read).put(sentences::save))
        .route("/detections", axum::routing::post(detect))
        .route("/entry-matches", axum::routing::post(match_entries))
        .route("/entries", get(list).post(create).layer(vary.clone()))
        .route("/entries/{id}", get(read).put(save).layer(vary.clone()))
        .route(
            "/entries/{id}/generation",
            axum::routing::post(generation::advance),
        )
        .route(
            "/entries/{id}/generation/candidates",
            axum::routing::post(generation::candidates),
        )
        .route(
            "/entries/{id}/generation/dictionary-reference",
            axum::routing::post(generation::dictionary_reference),
        )
        .route(
            "/entries/{id}/assessment",
            get(assessment::read).put(assessment::decide),
        )
        .route("/entries/{id}/view", get(view))
        .route(
            "/entries/{id}/publication",
            get(publications::read).layer(vary.clone()),
        )
        .route(
            "/entries/{id}/publication-preview",
            axum::routing::post(publications::preview),
        )
        .route(
            "/entries/{id}/publications",
            axum::routing::post(publications::publish).layer(vary),
        )
        .layer(DefaultBodyLimit::max(
            crate::lexicon::validation::MAX_STEP_CONTENT_BODY_BYTES + 512 * 1024,
        ))
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entry-matches", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=entry_matches::NextEntryMatchInput, params(("x-tsz-next-levels"=Option<String>,Header,description="v1 returns current saved sense levels in matches."),("x-tsz-next-publications"=Option<String>,Header,description="v1 returns published status in matched entries; omitted maps published to draft."),("x-tsz-next-annotation-rule"=Option<String>,Header,description="v1 returns whether an exact headword conflict requires an annotation."),("x-tsz-next-annotation-kind"=Option<String>,Header,description="word or phrase scope for the exact headword annotation conflict.")), responses((status=200, body=entry_matches::NextEntryMatches), (status=401), (status=403), (status=422)))]
pub async fn match_entries(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<entry_matches::NextEntryMatchInput>,
) -> Result<Json<entry_matches::NextEntryMatches>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    let annotation_rule = headers
        .get("x-tsz-next-annotation-rule")
        .is_some_and(|value| value == "v1");
    let annotation_kind =
        headers
            .get("x-tsz-next-annotation-kind")
            .and_then(|value| match value.to_str().ok()? {
                "word" => Some(NextEntryKind::Word),
                "phrase" => Some(NextEntryKind::Phrase),
                _ => None,
            });
    let mut result =
        entry_matches::find(&state.pool, input, annotation_rule, annotation_kind).await?;
    publication_statuses(&headers, &mut result.items);
    if let Some(annotation_items) = result.annotation_items.as_mut() {
        publication_statuses(&headers, annotation_items);
    }
    Ok(Json(result))
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/detections", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=detection::NextDetectionInput, params(("x-tsz-next-dictionary-pos"=Option<String>,Header,description="v1 resolves dictionary labels against the word or phrase part-of-speech catalog.")), responses((status=200, body=detection::NextDetectionResult), (status=401), (status=403), (status=422), (status=503)))]
pub async fn detect(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<detection::NextDetectionInput>,
) -> Result<Json<detection::NextDetectionResult>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    let include_pos_mappings = headers
        .get("x-tsz-next-dictionary-pos")
        .is_some_and(|value| value == "v1");
    Ok(Json(
        detection::detect(&state.pool, input, include_pos_mappings).await?,
    ))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/capabilities", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("x-tsz-next-generation"=Option<String>,Header,description="v1 probes the generation service; omission keeps generation false."),("x-tsz-next-generation-candidates"=Option<String>,Header,description="v1 negotiates candidate lookup and refresh."),("x-tsz-next-section-candidates"=Option<String>,Header,description="v1 negotiates additional section candidate lookup and refresh."),("x-tsz-next-dictionary-refresh"=Option<String>,Header,description="v1 negotiates the dictionary-reference refresh endpoint."),("x-tsz-next-creation-snapshot"=Option<String>,Header,description="v1 negotiates the immutable create-step detection snapshot"),("x-tsz-next-dialect-pairs"=Option<String>,Header,description="v1: reciprocal sense pairs and atomic dialect_changes"),("x-tsz-next-relations"=Option<String>, Header, description="v1 negotiates V1 relation editing; omission hides and preserves relations on existing senses."), ("x-tsz-next-sentence-drafts"=Option<String>, Header, description="v1 negotiates inline sentence drafts: optional sentence_resources and atomic sentence_changes saved with the complete word draft. Requires sentences/details v1."),("x-tsz-next-sentence-associations"=Option<String>, Header, description="v1 negotiates normalized sentence word/phrase associations."),("x-tsz-next-grammar-form-links"=Option<String>, Header, description="v1 negotiates normalized grammar form links."),("x-tsz-next-definitions"=Option<String>, Header, description="v1 negotiates definition_details: per-definition level, explicit unbound grammar, English rich content and audio. Omission returns legacy fields and preserves hidden metadata by stable ID on save.")), responses((status=200, body=NextCapabilities), (status=401), (status=403)))]
pub async fn capabilities(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
) -> Result<Json<NextCapabilities>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut capability = language::capabilities();
    if headers
        .get("x-tsz-next-generation")
        .is_some_and(|v| v == "v1")
    {
        let availability = generation::capability(&state).await;
        capability.generation = availability.generation;
        if headers
            .get("x-tsz-next-dictionary-refresh")
            .is_some_and(|value| value == "v1")
        {
            capability.dictionary_refresh =
                Some(availability.generation && availability.dictionary_refresh);
        }
        if headers
            .get("x-tsz-next-generation-candidates")
            .is_some_and(|v| v == "v1")
        {
            capability.generation_candidates =
                Some(availability.generation && availability.generation_candidates);
        }
        if generation::supports_section_candidates(&headers) {
            capability.section_generation_candidates = Some(
                availability.generation
                    && availability.generation_candidates
                    && availability.section_generation_candidates,
            );
        }
    }
    if supports_form_questions(&headers) {
        capability.form_question_eligibility = Some(true);
    }
    if supports_creation_snapshot(&headers) {
        capability.creation_snapshot = Some(true);
    }
    if supports_sentence_drafts(&headers) {
        capability.sentence_drafts = Some(true);
    }
    if supports_pairs(&headers) {
        capability.dialect_pairs = Some(true);
    }
    if supports_relations(&headers) {
        capability.relations = Some(true);
    }
    if sentences::supports_details(&headers) {
        capability.sentence_details = Some(true);
    }
    if supports_sentences(&headers) {
        capability.sentences = Some(true);
    }
    if supports_definitions(&headers) {
        capability.definition_details = Some(true);
    }
    if supports_equivalent_metadata(&headers) {
        capability.equivalent_metadata = Some(true);
    }
    if supports_sense_metadata(&headers) {
        capability.sense_metadata = Some(true);
    }
    if headers
        .get("x-tsz-next-forms")
        .is_some_and(|value| value == "v1")
    {
        capability.form_editor = Some(true);
    }
    if supports_grammar(&headers) {
        capability.grammar_structures = Some(true);
        if supports_grammar_audio(&headers) {
            capability.grammar_audio = Some(true);
        }
        if supports_grammar_form_links(&headers) {
            capability.grammar_form_links = Some(true);
        }
    }
    if supports_sentence_associations(&headers) {
        capability.sentence_associations = Some(true);
    }
    if supports_sense_groups(&headers) {
        capability.sense_groups = Some(true);
    }
    if supports_medium_ownership(&headers) {
        capability.medium_ownership = Some(true);
    }
    Ok(Json(capability))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/draft-schema", tag="admin-lexicon-next", security(("bearer_auth"=[])), responses((status=200, body=NextDraftSchemaCompatibility), (status=401), (status=403)))]
pub async fn draft_schema(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<NextDraftSchemaCompatibility>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(language::draft_schema_compatibility()))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(NextListQuery), responses((status=200, body=NextEntryList), (status=401), (status=403), (status=422)))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextListQuery>,
) -> Result<Json<NextEntryList>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(repository::list(&state.pool, query).await?))
}
#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entries", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=CreateNextEntry, params(("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("X-TSZ-Test-Run"=Option<String>, Header, description="仅测试资源白名单账号可用的独占批次；词条会登记到该批次"),("x-tsz-next-creation-snapshot"=Option<String>,Header,description="v1 negotiates the immutable create-step detection snapshot"),("x-tsz-next-dialect-pairs"=Option<String>,Header,description="v1: reciprocal sense pairs and atomic dialect_changes"),("x-tsz-next-relations"=Option<String>, Header, description="v1 negotiates V1 relation editing; omission hides and preserves relations on existing senses."), ("x-tsz-next-sentence-drafts"=Option<String>, Header, description="v1 negotiates inline sentence drafts: optional sentence_resources and atomic sentence_changes saved with the complete word draft. Requires sentences/details v1."),("x-tsz-next-definitions"=Option<String>, Header, description="v1 negotiates definition_details: per-definition level, explicit unbound grammar, English rich content and audio. Omission returns legacy fields and preserves hidden metadata by stable ID on save.")), responses((status=201, body=NextEntry), (status=401), (status=403), (status=422)))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<CreateNextEntry>,
) -> Result<(StatusCode, Json<NextEntry>), AppError> {
    let (_, test_run) = crate::test_resources::scoped_write(&state, &headers, auth.subject)?;
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    if let Some(snapshot) = &input.creation_detection
        && (!supports_creation_snapshot(&headers) || snapshot.kind != input.kind)
    {
        return Err(language::invalid());
    }
    if let Some(snapshot) = &input.creation_detection {
        let input_key = normalize_headword(&input.headword)
            .map_err(|_| language::invalid())?
            .key;
        let snapshot_key = normalize_headword(&snapshot.surface)
            .map_err(|_| language::invalid())?
            .key;
        if input_key != snapshot_key {
            return Err(language::invalid());
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(entry_for_client(
            repository::create(&state.pool, &auth, input, test_run.as_deref()).await?,
            &headers,
        )),
    ))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("x-tsz-next-creation-snapshot"=Option<String>,Header,description="v1 negotiates the immutable create-step detection snapshot"),("x-tsz-next-dialect-pairs"=Option<String>,Header,description="v1: reciprocal sense pairs and atomic dialect_changes"),("x-tsz-next-relations"=Option<String>, Header, description="v1 negotiates V1 relation editing; omission hides and preserves relations on existing senses."), ("x-tsz-next-sentence-drafts"=Option<String>, Header, description="v1 negotiates inline sentence drafts: optional sentence_resources and atomic sentence_changes saved with the complete word draft. Requires sentences/details v1."),("x-tsz-next-definitions"=Option<String>, Header, description="v1 negotiates definition_details: per-definition level, explicit unbound grammar, English rich content and audio. Omission returns legacy fields and preserves hidden metadata by stable ID on save."), ("id"=Uuid, Path)), responses((status=200, body=NextEntry), (status=401), (status=403), (status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<NextEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut entry = repository::get(&state.pool, id).await?;
    if supports_pairs(&headers) {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        entry.dialect_senses =
            Some(dialect_pairs::project(&mut conn, id, entry.revision, false).await?);
    }
    if supports_sentence_drafts(&headers)
        && permissions::load(&state, &auth)
            .await?
            .has("sentences.access")
    {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        entry.sentence_resources = Some(sentence_drafts::resources(&mut conn, &entry.draft).await?);
    }
    Ok(Json(entry_for_client(entry, &headers)))
}
#[utoipa::path(put, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("X-TSZ-Test-Run"=Option<String>, Header, description="专用测试管理员的独占批次"),("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("x-tsz-next-creation-snapshot"=Option<String>,Header,description="v1 negotiates the immutable create-step detection snapshot"),("x-tsz-next-dialect-pairs"=Option<String>,Header,description="v1: reciprocal sense pairs and atomic dialect_changes"),("x-tsz-next-relations"=Option<String>, Header, description="v1 negotiates V1 relation editing; omission hides and preserves relations on existing senses."), ("x-tsz-next-sentence-drafts"=Option<String>, Header, description="v1 negotiates inline sentence drafts: optional sentence_resources and atomic sentence_changes saved with the complete word draft. Requires sentences/details v1."),("x-tsz-next-definitions"=Option<String>, Header, description="v1 negotiates definition_details: per-definition level, explicit unbound grammar, English rich content and audio. Omission returns legacy fields and preserves hidden metadata by stable ID on save."), ("id"=Uuid, Path)), request_body=SaveNextDraft, responses((status=200, body=NextEntry), (status=401), (status=403), (status=404), (status=409, description="Draft conflict, including stable_node_id_changed and entry_archived. For revision_conflict, field expected_revision identifies a stale entry revision, and meta.code speech_recommendation_changed means voice recommendations must be resolved and audio regenerated."), (status=422)))]
pub async fn save(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<SaveNextDraft>,
) -> Result<Json<NextEntry>, AppError> {
    if !supports_form_questions(&headers) && !input.draft.form_question_eligibility.is_empty() {
        return Err(language::unavailable());
    }
    if !input.dialect_changes.is_empty()
        && (!supports_pairs(&headers) || input.dialect_changes.len() > 1000)
    {
        return Err(language::invalid());
    }
    if !supports_relations(&headers)
        && input
            .draft
            .senses
            .iter()
            .any(|sense| sense.relations.is_some())
    {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "relations require x-tsz-next-relations: v1",
        ));
    }
    if !supports_sentences(&headers)
        && input
            .draft
            .senses
            .iter()
            .any(|sense| sense.sentence_ids.is_some())
    {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "sentence references require x-tsz-next-sentences: v1",
        ));
    }
    if !supports_sentence_associations(&headers)
        && sentence_drafts::has_associations(&input.sentence_changes)
    {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "sentence associations require x-tsz-next-sentence-associations: v1",
        ));
    }
    if !supports_definitions(&headers) && definitions::has_details(&input.draft) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "definition details require x-tsz-next-definitions: v1",
        ));
    }
    if !supports_equivalent_metadata(&headers) && equivalents::has_metadata(&input.draft) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "equivalent metadata requires x-tsz-next-equivalents: v1",
        ));
    }
    if !supports_sense_metadata(&headers)
        && input.draft.senses.iter().any(|sense| {
            sense.level.is_some() || sense.frequency.is_some() || sense.depends_on_context.is_some()
        })
    {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "sense metadata requires x-tsz-next-sense-metadata: v1",
        ));
    }
    if input.draft.grammar_structures.is_some() && !supports_grammar(&headers) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "grammar structures require x-tsz-next-grammar: v1",
        ));
    }
    if !supports_grammar_form_links(&headers) && grammar::has_form_links(&input.draft) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "grammar form links require x-tsz-next-grammar-form-links: v1",
        ));
    }
    if !supports_grammar_audio(&headers) && grammar::has_audio(&input.draft) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "grammar audio requires x-tsz-next-grammar-audio: v1",
        ));
    }
    permissions::load(&state, &auth)
        .await?
        .require("words.edit")?;
    let (_, run) = crate::test_resources::scoped_write(&state, &headers, auth.subject)?;
    Ok(Json(entry_for_client(
        repository::save(
            &state.pool,
            &auth,
            id,
            input,
            run.as_deref(),
            repository::SaveCapabilities {
                sentence_drafts: supports_sentence_drafts(&headers),
                pairs: supports_pairs(&headers),
                relations: supports_relations(&headers),
                grammar_audio: supports_grammar_audio(&headers),
                grammar_form_links: supports_grammar_form_links(&headers),
                sense_metadata: supports_sense_metadata(&headers),
                equivalent_metadata: supports_equivalent_metadata(&headers),
                definition_details: supports_definitions(&headers),
                sentences: supports_sentences(&headers),
            },
        )
        .await?,
        &headers,
    )))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}/view", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("x-tsz-next-dialect-pairs"=Option<String>,Header,description="v1: reciprocal sense pairs in the draft view"),("x-tsz-next-relations"=Option<String>, Header, description="v1 negotiates V1 relation editing; omission hides and preserves relations on existing senses."), ("x-tsz-next-sentence-drafts"=Option<String>, Header, description="v1 negotiates inline sentence drafts: optional sentence_resources and atomic sentence_changes saved with the complete word draft. Requires sentences/details v1."),("x-tsz-next-definitions"=Option<String>, Header, description="v1 negotiates definition_details: per-definition level, explicit unbound grammar, English rich content and audio. Omission returns legacy fields and preserves hidden metadata by stable ID on save."), ("id"=Uuid, Path), NextViewQuery), responses((status=200, body=NextEntryView), (status=401), (status=403), (status=404), (status=422, description="capability_unavailable")))]
pub async fn view(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
    ApiQuery(query): ApiQuery<NextViewQuery>,
) -> Result<Json<NextEntryView>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    language::require_medium(&query.medium_locale)?;
    let entry = repository::get(&state.pool, id).await?;
    if supports_medium_ownership(&headers)
        && entry.medium_locale.as_deref() != Some(query.medium_locale.as_str())
    {
        return Err(AppError::not_found(
            "entry belongs to another medium locale",
        ));
    }
    let parents = if supports_grammar(&headers) && entry.draft.grammar_structures.is_some() {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        grammar::catalog(&mut conn).await?
    } else {
        Default::default()
    };
    let mut view = language::view(&entry, query, &parents, supports_definitions(&headers))?;
    if supports_pairs(&headers) {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        view.dialect_senses =
            Some(dialect_pairs::project(&mut conn, id, entry.revision, false).await?);
    }
    if !supports_relations(&headers) {
        for sense in &mut view.senses {
            sense.relations = None;
        }
    }
    if !supports_sentences(&headers) {
        for sense in &mut view.senses {
            sense.sentence_ids = None;
        }
    }
    if supports_grammar(&headers) {
        view.grammar_issues = entry
            .draft
            .grammar_structures
            .as_ref()
            .map(|_| grammar::issues(&entry.draft, &parents));
    } else {
        view.grammar_structures = None;
        for sense in &mut view.senses {
            for definition in sense
                .target_definitions
                .iter_mut()
                .chain(sense.medium_definitions.iter_mut())
            {
                definition.grammar_structure_id = None;
            }
        }
    }
    if !supports_grammar_audio(&headers) {
        grammar::strip_audio(view.grammar_structures.as_mut());
    }
    if !supports_sense_groups(&headers) {
        view.sense_groups = None;
        for sense in &mut view.senses {
            sense.sense_group_id = None;
        }
    }
    if !supports_sense_metadata(&headers) {
        for sense in &mut view.senses {
            sense.level = None;
            sense.frequency = None;
            sense.depends_on_context = None;
        }
    }
    if !supports_equivalent_metadata(&headers) {
        for sense in &mut view.senses {
            sense.no_equivalents = None;
            sense.missing_equivalents = sense.equivalents.is_empty();
            for row in &mut sense.equivalents {
                row.metadata = None;
            }
        }
    }
    if !supports_definitions(&headers) {
        for sense in &mut view.senses {
            for row in sense
                .target_definitions
                .iter_mut()
                .chain(&mut sense.medium_definitions)
            {
                definitions::strip(row);
            }
        }
    }
    if supports_sentence_drafts(&headers)
        && permissions::load(&state, &auth)
            .await?
            .has("sentences.access")
    {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        view.sentence_resources = Some(sentence_drafts::resources(&mut conn, &entry.draft).await?);
    }
    Ok(Json(view))
}

fn supports_sentence_drafts(headers: &axum::http::HeaderMap) -> bool {
    supports_sentences(headers)
        && sentences::supports_details(headers)
        && headers
            .get("x-tsz-next-sentence-drafts")
            .is_some_and(|value| value == "v1")
}

fn supports_definitions(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-definitions")
        .is_some_and(|value| value == "v1")
}

fn supports_grammar_audio(headers: &axum::http::HeaderMap) -> bool {
    supports_grammar(headers)
        && headers
            .get("x-tsz-next-grammar-audio")
            .is_some_and(|value| value == "v1")
}

fn supports_grammar_form_links(headers: &axum::http::HeaderMap) -> bool {
    supports_grammar(headers)
        && headers
            .get("x-tsz-next-grammar-form-links")
            .is_some_and(|value| value == "v1")
}

fn supports_sentence_associations(headers: &axum::http::HeaderMap) -> bool {
    supports_sentences(headers)
        && headers
            .get("x-tsz-next-sentence-associations")
            .is_some_and(|value| value == "v1")
}

fn supports_grammar(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-grammar")
        .is_some_and(|value| value == "v1")
}

fn supports_medium_ownership(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-medium-ownership")
        .is_some_and(|value| value == "v1")
}

fn supports_sense_groups(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-sense-groups")
        .is_some_and(|value| value == "v1")
}

fn supports_form_questions(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-form-questions")
        .is_some_and(|v| v == "v1")
}

fn supports_sense_metadata(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-sense-metadata")
        .is_some_and(|value| value == "v1")
}

fn supports_pairs(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-dialect-pairs")
        .is_some_and(|v| v == "v1")
}

fn supports_relations(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-relations")
        .is_some_and(|value| value == "v1")
}

fn entry_for_client(mut entry: NextEntry, headers: &axum::http::HeaderMap) -> NextEntry {
    if !supports_form_questions(headers) {
        entry.draft.form_question_eligibility.clear();
    }
    if !supports_creation_snapshot(headers) {
        entry.creation_detection = None;
    }
    if !supports_sentence_drafts(headers) {
        entry.sentence_resources = None;
    }
    if !supports_relations(headers) {
        for sense in &mut entry.draft.senses {
            sense.relations = None;
        }
    }
    if !supports_sentences(headers) {
        for sense in &mut entry.draft.senses {
            sense.sentence_ids = None;
        }
    }
    if !supports_sentence_associations(headers) {
        for sentence in entry.sentence_resources.iter_mut().flatten() {
            for variant in &mut sentence.draft.variants {
                variant.associations = None;
            }
        }
    }
    if !supports_definitions(headers) {
        for row in entry
            .draft
            .senses
            .iter_mut()
            .flat_map(|s| &mut s.definitions)
        {
            definitions::strip(row);
        }
    }
    if !supports_equivalent_metadata(headers) {
        equivalents::strip(&mut entry.draft);
    }
    if !supports_sense_metadata(headers) {
        for sense in &mut entry.draft.senses {
            sense.level = None;
            sense.frequency = None;
            sense.depends_on_context = None;
        }
    }
    if !supports_grammar(headers) {
        entry.draft.grammar_structures = None;
        for sense in &mut entry.draft.senses {
            sense.sub_pos = None;
            for definition in &mut sense.definitions {
                definition.grammar_structure_id = None;
            }
        }
    }
    if !supports_grammar_form_links(headers) {
        for variant in entry
            .draft
            .grammar_structures
            .iter_mut()
            .flatten()
            .flat_map(|grammar| &mut grammar.variants)
        {
            variant.form_links = None;
        }
    }
    if !supports_grammar_audio(headers) {
        grammar::strip_audio(entry.draft.grammar_structures.as_mut());
    }
    if !supports_medium_ownership(headers) {
        entry.medium_locale = None;
    }
    if !supports_sense_groups(headers) {
        entry.draft.sense_groups = None;
        for sense in &mut entry.draft.senses {
            sense.sense_group_id = None;
        }
    }
    entry
}

fn supports_creation_snapshot(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-creation-snapshot")
        .is_some_and(|value| value == "v1")
}

fn supports_sentences(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-sentences")
        .is_some_and(|value| value == "v1")
}

#[cfg(test)]
mod tests;

#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/entries",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery,("x-tsz-next-name-variants"=Option<String>,Header,description="v1 returns workspace list display names; omission preserves old strict responses."),("x-tsz-next-levels"=Option<String>,Header,description="v1 returns current saved sense levels; omission preserves old strict responses."),("x-tsz-next-publications"=Option<String>,Header,description="v1 returns published status; omitted maps published to draft for old strict clients.")),responses((status=200,body=NextWorkspaceList),(status=401),(status=403),(status=422)))]
pub async fn workspace_list(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<NextWorkspaceList>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let include_name_variants = headers
        .get("x-tsz-next-name-variants")
        .is_some_and(|value| value == "v1");
    let mut result = workspace::list(&state.pool, query, include_name_variants).await?;
    publication_statuses(&headers, &mut result.items);
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/entries/{id}",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("x-tsz-next-levels"=Option<String>,Header,description="v1 returns current saved sense levels."),("x-tsz-next-publications"=Option<String>,Header,description="v1 returns published status; omitted maps published to draft for old strict clients.")),responses((status=200,body=NextWorkspaceEntry),(status=401),(status=403),(status=404)))]
pub async fn workspace_read(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NextWorkspaceEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut result = workspace::get(&state.pool, id).await?;
    publication_statuses(&headers, std::slice::from_mut(&mut result));
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/stats",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=NextWorkspaceStats),(status=401),(status=403),(status=422)))]
pub async fn workspace_stats(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<NextWorkspaceStats>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::stats(&state.pool, query).await?))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/creators",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=Vec<NextWorkspaceCreator>),(status=401),(status=403),(status=422)))]
pub async fn workspace_creators(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<Vec<NextWorkspaceCreator>>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::creators(&state.pool, query).await?))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/updaters",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=Vec<NextWorkspaceCreator>),(status=401),(status=403),(status=422)))]
pub async fn workspace_updaters(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<Vec<NextWorkspaceCreator>>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::updaters(&state.pool, query).await?))
}
#[utoipa::path(patch,path="/api/v1/admin/lexicon-next/workspace/entries/{id}/annotation",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=NextAnnotationInput,responses((status=200,body=NextAnnotationResult),(status=401),(status=403),(status=404),(status=409),(status=422)))]
pub async fn workspace_annotation(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<NextAnnotationInput>,
) -> Result<Json<NextAnnotationResult>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.edit")?;
    Ok(Json(
        workspace::annotation(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/lexicon-next/workspace/commands",tag="admin-lexicon-next",security(("bearer_auth"=[])),request_body=NextListCommand,responses((status=200,body=NextListCommandResult),(status=401),(status=403),(status=404),(status=409),(status=422)))]
pub async fn workspace_command(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<NextListCommand>,
) -> Result<Json<NextListCommandResult>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::command(&state.pool, &auth, input).await?))
}

fn supports_equivalent_metadata(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-equivalents")
        .is_some_and(|value| value == "v1")
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEquivalentCatalog {
    pub registers: Vec<crate::catalog::taxonomies::model::LexiconTaxonomy>,
    pub domains: Vec<crate::catalog::taxonomies::model::LexiconTaxonomy>,
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/equivalent-catalog", tag="admin-lexicon-next", security(("bearer_auth"=[])), responses((status=200, body=NextEquivalentCatalog), (status=401), (status=403)))]
pub async fn equivalent_catalog(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<NextEquivalentCatalog>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    // Both complete directories are read in one statement/snapshot; no first-page truncation.
    let rows = sqlx::query_as::<_, EquivalentCatalogRow>(
        "SELECT kind,id,code,name_zh,name_en,sort_order,revision,created_at,updated_at FROM catalog.lexicon_taxonomies ORDER BY kind,sort_order,created_at,id"
    ).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    let mut result = NextEquivalentCatalog {
        registers: vec![],
        domains: vec![],
    };
    for row in rows {
        if row.kind == "registers" {
            result.registers.push(row.item);
        } else {
            result.domains.push(row.item);
        }
    }
    Ok(Json(result))
}

#[derive(sqlx::FromRow)]
struct EquivalentCatalogRow {
    kind: String,
    #[sqlx(flatten)]
    item: crate::catalog::taxonomies::model::LexiconTaxonomy,
}

fn publication_statuses(headers: &axum::http::HeaderMap, items: &mut [NextWorkspaceEntry]) {
    for entry in items.iter_mut() {
        if !headers.get("x-tsz-next-levels").is_some_and(|v| v == "v1") {
            entry.levels = None;
        }
    }
    if !headers
        .get("x-tsz-next-publications")
        .is_some_and(|v| v == "v1")
    {
        for entry in items {
            if entry.status == "published" {
                entry.status = "draft".into();
            }
        }
    }
}
