use super::*;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use crate::lexicon::{
    dto::{
        ComponentTargetMatchV3, DraftSentenceTargetCandidateV3, PublishedSentenceTargetCandidateV3,
        SearchComponentTargetsV3Input, SearchComponentTargetsV3Response,
        SentenceTargetDiscoveryCompletenessV3, SentenceTargetDraftLinkabilityV3,
        SentenceTargetDraftStateV3, SentenceTargetMatchEvidenceV3, SentenceTargetMatchKindV3,
        SentenceTargetRangeResultV3,
    },
    sentence_target_discovery::{
        AliasPosting, CodepointRange, ContiguousPattern, DiscoveryCatalog, DiscoveryEngine,
        DiscoveryMatch, MatchKind, SourceSegment, codepoint_slice, source_fingerprint, tokenize,
    },
};

use super::sentence_association::{PublishedAssociationCandidateKey, PublishedAssociationTarget};
use super::v3::load_draft_component_targets;

const MAX_DISCOVERY_TOKENS: usize = 100;
const MAX_CONTIGUOUS_PHRASE_TOKENS: usize = 40;
const DEFAULT_COMPONENT_TARGET_PAGE_SIZE: u32 = 50;
const MAX_COMPONENT_TARGET_PAGE_SIZE: u32 = 200;
/// 每批快照的词条数；只限制峰值，不限制可遍历的结果集。
const COMPONENT_TARGET_SNAPSHOT_BATCH_SIZE: usize = 200;

#[derive(Debug, Clone)]
struct ComponentTargetCandidateIndex {
    key: PublishedAssociationCandidateKey,
    match_rank: i32,
    draft: bool,
    headword: Arc<str>,
}

fn published_candidate_key(
    candidate: &PublishedSentenceTargetCandidateV3,
) -> PublishedAssociationCandidateKey {
    PublishedAssociationCandidateKey {
        entry_id: candidate.entry_id,
        publication_id: candidate.publication_id,
        pos_id: candidate.pos_id,
        base_form_id: candidate.base_form_id,
        matched_variant_id: candidate.matched_variant_id,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl LexiconService {
    /// 句子专用发现入口：HTTP 只暴露例句语义，匹配事实由内部中性 core 提供。
    pub async fn resolve_sentence_targets_v3(
        &self,
        actor_id: Uuid,
        input: ResolveSentenceTargetsV3Input,
        allow_v3: bool,
    ) -> Result<ResolveSentenceTargetsV3Response, LexiconServiceError> {
        if !allow_v3 {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        let sentence_text = input.sentence_text();
        let text_len = sentence_text.chars().count();
        if text_len == 0 || text_len > 1000 {
            return Err(LexiconServiceError::UnprocessableField {
                field: "sentence_text",
                message: "sentence_text must contain between 1 and 1000 codepoints",
            });
        }
        let tokens = tokenize(sentence_text);
        if tokens.len() > MAX_DISCOVERY_TOKENS {
            return Err(LexiconServiceError::UnprocessableField {
                field: "sentence_text",
                message: "sentence_text must contain at most 100 tokens",
            });
        }
        let page_size = page_size(&input)?;
        let dialect_scopes = discovery_scopes(input.source_dialect());
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let generation = LexiconRepository::sentence_discovery_generation(&mut transaction)
            .await
            .map_err(repository_error)?;

        let (lookup_surfaces, selected) = match &input {
            ResolveSentenceTargetsV3Input::AllPublishedTargets { .. } => {
                (automatic_lookup_surfaces(&tokens), None)
            }
            ResolveSentenceTargetsV3Input::SelectedSegments {
                selected_segments, ..
            } => {
                let (segments, normalized) =
                    validate_selected_segments(sentence_text, selected_segments)?;
                let mut surfaces = BTreeSet::new();
                surfaces.insert(normalized.clone());
                (surfaces, Some((segments, normalized)))
            }
        };
        let lookup_surfaces = lookup_surfaces.into_iter().collect::<Vec<_>>();
        let surfaces = LexiconRepository::published_sentence_discovery_surfaces(
            &mut transaction,
            &dialect_scopes,
            &lookup_surfaces,
        )
        .await
        .map_err(repository_error)?;
        let entry_ids = surfaces
            .iter()
            .map(|surface| surface.entry_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let targets =
            LexiconRepository::current_publication_snapshots(&mut transaction, &entry_ids)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| {
                    PublishedAssociationTarget::from_snapshot(record.snapshot, true)
                        .map(|target| (record.entry_id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;

        let draft_matches = match &input {
            ResolveSentenceTargetsV3Input::SelectedSegments {
                include_drafts: true,
                ..
            } => {
                let normalized = selected
                    .as_ref()
                    .expect("selected mode has canonical segments")
                    .1
                    .as_str();
                LexiconRepository::draft_sentence_discovery_targets(
                    &mut transaction,
                    &dialect_scopes,
                    normalized,
                    actor_id,
                )
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(draft_candidate)
                .collect()
            }
            _ => Vec::new(),
        };

        let mut range_results = if let Some((segments, normalized)) = selected {
            let fingerprint =
                source_fingerprint(sentence_text, &segments).map_err(|_| invariant_record())?;
            let offset = selected_cursor_offset(&input, generation, &fingerprint)?;
            let kind = if segments.len() == 1 && tokenize(&normalized).len() == 1 {
                SentenceTargetMatchKindV3::Word
            } else if segments.len() == 1 {
                SentenceTargetMatchKindV3::ContiguousPhrase
            } else {
                SentenceTargetMatchKindV3::SeparablePhrase
            };
            let evidence = match_evidence(sentence_text, &segments, &normalized, kind);
            vec![range_result(
                sentence_text,
                segments,
                normalized,
                published_candidates(&surfaces, &targets, Some(evidence)),
                draft_matches,
                RangeResultPagination {
                    page_size,
                    offset,
                    cursor_context: Some((generation, input.source_dialect(), fingerprint)),
                },
            )?]
        } else {
            automatic_range_results(
                sentence_text,
                surfaces,
                &targets,
                page_size,
                generation,
                input.source_dialect(),
            )?
        };
        range_results.sort_by(|left, right| {
            left.source_segments[0]
                .start
                .cmp(&right.source_segments[0].start)
                .then(left.normalized_surface.cmp(&right.normalized_surface))
        });
        transaction.commit().await.map_err(database_error)?;

        Ok(ResolveSentenceTargetsV3Response {
            schema_version: 3,
            sentence_hash: hex_digest(&sha256_json(&sentence_text).map_err(serialization_error)?),
            discovery_generation: generation,
            completeness: SentenceTargetDiscoveryCompletenessV3::Complete,
            range_results,
        })
    }

    /// 成分用词 / 正文关联目标的关键字检索：与 resolve 共用候选组装，把「词面等值」换成
    /// `match` 指定的匹配方式（默认包含，`exact` 为归一化后等值）。默认只回已发布且未归档的词条；
    /// `include_drafts` 为真时再加上**从未发布**的 V3 草稿（不限创建者，候选没有 `publication_id`），
    /// 已发布词条只按其当前发布版本出现一次。
    ///
    /// 顺序：词面等于 q 的词条最前、以 q 开头的其次、其余按 headword；同档位已发布优先于草稿
    /// （SQL 与 Rust 两侧同一规则）。分页照 resolve 的游标：绑定 discovery generation 与
    /// q/kind/match/include_drafts 摘要，词库变动或换任一条件即失效。
    pub async fn search_component_targets_v3(
        &self,
        input: SearchComponentTargetsV3Input,
        allow_v3: bool,
    ) -> Result<SearchComponentTargetsV3Response, LexiconServiceError> {
        if !allow_v3 {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        let keyword = component_target_keyword(&input.q)?;
        let match_mode = input.match_mode.unwrap_or_default();
        let exact = match_mode == ComponentTargetMatchV3::Exact;
        // exact 与词面投影用同一套归一化，比的是 normalized_surface；contains 原样做包含匹配。
        let lookup = if exact {
            crate::lexicon::normalization::normalize_headword(keyword)
                .map_err(|_| LexiconServiceError::UnprocessableField {
                    field: "q",
                    message: "q must normalize to a valid word or phrase surface",
                })?
                .key
        } else {
            keyword.to_owned()
        };
        let page_size = component_target_page_size(input.page_size)?;
        let cursor_digest = component_target_cursor_digest(
            keyword,
            input.kind,
            match_mode,
            input.include_drafts,
            input.entry_id,
        )?;
        // 关键字检索没有句子，也就没有 source dialect 可依；英美两个 scope 都查。
        let dialect_scopes = discovery_scopes(Dialect::Common);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let generation = LexiconRepository::sentence_discovery_generation(&mut transaction)
            .await
            .map_err(repository_error)?;
        let published_entries = LexiconRepository::component_target_entry_matches(
            &mut transaction,
            &dialect_scopes,
            &lookup,
            input.kind,
            input.entry_id,
            exact,
            false,
        )
        .await
        .map_err(repository_error)?;
        let draft_entries = if input.include_drafts {
            LexiconRepository::component_target_entry_matches(
                &mut transaction,
                &dialect_scopes,
                &lookup,
                input.kind,
                input.entry_id,
                exact,
                true,
            )
            .await
            .map_err(repository_error)?
        } else {
            Vec::new()
        };
        // 精确 total 需要遍历完整匹配集，但只积累轻量身份；forms / senses 等富 DTO
        // 留到分页后、只为当前页物化。entry 查询已按词条去重，快照再分批读取。
        let entry_ids = published_entries
            .iter()
            .chain(&draft_entries)
            .map(|entry| entry.entry_id)
            .collect::<Vec<_>>();
        let entry_rank = published_entries
            .iter()
            .chain(&draft_entries)
            .map(|entry| (entry.entry_id, entry.match_rank))
            .collect::<HashMap<_, _>>();
        let published_entry_ids = published_entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<HashSet<_>>();
        let draft_entry_ids = draft_entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<HashSet<_>>();
        // 归档不会推进 discovery generation，但会移走候选；绑定可用词条集合避免 offset 跳漏。
        let available_ids = entry_ids.iter().copied().collect::<BTreeSet<_>>();
        let cursor_digest =
            hex_digest(&sha256_json(&(cursor_digest, available_ids)).map_err(serialization_error)?);
        let offset =
            component_target_cursor_offset(input.cursor.as_deref(), generation, &cursor_digest)?;
        let mut candidate_index =
            HashMap::<PublishedAssociationCandidateKey, ComponentTargetCandidateIndex>::new();
        for batch in entry_ids.chunks(COMPONENT_TARGET_SNAPSHOT_BATCH_SIZE) {
            let published_ids = batch
                .iter()
                .copied()
                .filter(|entry_id| published_entry_ids.contains(entry_id))
                .collect::<Vec<_>>();
            let draft_ids = batch
                .iter()
                .copied()
                .filter(|entry_id| draft_entry_ids.contains(entry_id))
                .collect::<Vec<_>>();
            let published = LexiconRepository::component_target_surfaces(
                &mut transaction,
                &dialect_scopes,
                &lookup,
                input.kind,
                &published_ids,
                exact,
                false,
            )
            .await
            .map_err(repository_error)?;
            let drafts = if input.include_drafts {
                LexiconRepository::component_target_surfaces(
                    &mut transaction,
                    &dialect_scopes,
                    &lookup,
                    input.kind,
                    &draft_ids,
                    exact,
                    true,
                )
                .await
                .map_err(repository_error)?
            } else {
                Vec::new()
            };
            let mut targets =
                LexiconRepository::current_publication_snapshots(&mut transaction, &published_ids)
                    .await
                    .map_err(repository_error)?
                    .into_iter()
                    .map(|record| {
                        PublishedAssociationTarget::from_snapshot(record.snapshot, true)
                            .map(|target| (record.entry_id, target))
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?;
            for target in load_draft_component_targets(&mut transaction, &draft_ids).await? {
                targets.insert(
                    target.id,
                    PublishedAssociationTarget::from_component_target(target)?,
                );
            }
            let headwords = targets
                .iter()
                .map(|(entry_id, target)| (*entry_id, Arc::<str>::from(target.headword())))
                .collect::<HashMap<_, _>>();
            for surface in published.iter().chain(&drafts) {
                let Some(target) = targets.get(&surface.entry_id) else {
                    continue;
                };
                let Some(headword) = headwords.get(&surface.entry_id) else {
                    continue;
                };
                for key in target.sentence_discovery_candidate_keys(
                    surface.publication_id,
                    surface.pos_id,
                    surface.matched_form_id,
                    surface.matched_variant_id,
                ) {
                    candidate_index
                        .entry(key)
                        .or_insert_with(|| ComponentTargetCandidateIndex {
                            key,
                            match_rank: entry_rank.get(&key.entry_id).copied().unwrap_or(2),
                            draft: key.publication_id.is_none(),
                            headword: headword.clone(),
                        });
                }
            }
        }
        let mut candidate_index = candidate_index.into_values().collect::<Vec<_>>();
        candidate_index.sort_by(|left, right| {
            left.match_rank
                .cmp(&right.match_rank)
                .then_with(|| left.draft.cmp(&right.draft))
                .then_with(|| left.headword.cmp(&right.headword))
                .then_with(|| left.key.cmp(&right.key))
        });
        let total = candidate_index.len();
        let has_more = offset.saturating_add(page_size) < total;
        let page_keys = candidate_index
            .iter()
            .skip(offset)
            .take(page_size)
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>();

        // 当前页至多 200 条轻量身份；此处才加载对应快照并生成包含 forms / senses 的 DTO。
        let page_entry_ids = page_keys
            .iter()
            .map(|key| key.entry_id)
            .collect::<BTreeSet<_>>();
        let page_published_ids = page_keys
            .iter()
            .filter(|key| key.publication_id.is_some())
            .map(|key| key.entry_id)
            .collect::<Vec<_>>();
        let page_draft_ids = page_keys
            .iter()
            .filter(|key| key.publication_id.is_none())
            .map(|key| key.entry_id)
            .collect::<Vec<_>>();
        let page_published = LexiconRepository::component_target_surfaces(
            &mut transaction,
            &dialect_scopes,
            &lookup,
            input.kind,
            &page_published_ids,
            exact,
            false,
        )
        .await
        .map_err(repository_error)?;
        let page_drafts = if input.include_drafts {
            LexiconRepository::component_target_surfaces(
                &mut transaction,
                &dialect_scopes,
                &lookup,
                input.kind,
                &page_draft_ids,
                exact,
                true,
            )
            .await
            .map_err(repository_error)?
        } else {
            Vec::new()
        };
        let mut page_targets =
            LexiconRepository::current_publication_snapshots(&mut transaction, &page_published_ids)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| {
                    PublishedAssociationTarget::from_snapshot(record.snapshot, true)
                        .map(|target| (record.entry_id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
        for target in load_draft_component_targets(&mut transaction, &page_draft_ids).await? {
            page_targets.insert(
                target.id,
                PublishedAssociationTarget::from_component_target(target)?,
            );
        }
        let requested_keys = page_keys.iter().copied().collect::<HashSet<_>>();
        let mut materialized = published_candidates(&page_published, &page_targets, None);
        materialized.extend(published_candidates(&page_drafts, &page_targets, None));
        let mut materialized = materialized
            .into_iter()
            .filter_map(|candidate| {
                let key = published_candidate_key(&candidate);
                requested_keys.contains(&key).then_some((key, candidate))
            })
            .collect::<HashMap<_, _>>();
        let page = page_keys
            .iter()
            .filter_map(|key| materialized.remove(key))
            .collect::<Vec<_>>();
        if page.len() != page_keys.len()
            || page
                .iter()
                .any(|candidate| !page_entry_ids.contains(&candidate.entry_id))
        {
            return Err(invariant_record());
        }
        transaction.commit().await.map_err(database_error)?;
        let next_cursor =
            has_more.then(|| format!("{generation}:{}:{cursor_digest}", offset + page_size));

        Ok(SearchComponentTargetsV3Response {
            schema_version: 3,
            matches: page,
            total: total as u64,
            truncated: has_more,
            next_cursor,
        })
    }
}

/// 与 `component_target_surfaces` 的 `ORDER BY CASE` 同一规则，两处必须同步改。
#[cfg(test)]
fn component_target_rank(normalized_surface: &str, lowered_keyword: &str) -> u8 {
    if normalized_surface == lowered_keyword {
        0
    } else if normalized_surface.starts_with(lowered_keyword) {
        1
    } else {
        2
    }
}

fn component_target_cursor_digest(
    q: &str,
    kind: Option<EntryKind>,
    match_mode: ComponentTargetMatchV3,
    include_drafts: bool,
    entry_id: Option<Uuid>,
) -> Result<String, LexiconServiceError> {
    Ok(hex_digest(
        &sha256_json(&(q, kind, match_mode, include_drafts, entry_id))
            .map_err(serialization_error)?,
    ))
}

fn component_target_cursor_offset(
    cursor: Option<&str>,
    generation: i64,
    digest: &str,
) -> Result<usize, LexiconServiceError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.splitn(3, ':');
    let cursor_generation = parts.next().and_then(|value| value.parse::<i64>().ok());
    let offset = parts.next().and_then(|value| value.parse::<usize>().ok());
    let cursor_digest = parts.next();
    match (cursor_generation, offset, cursor_digest) {
        (Some(cursor_generation), Some(offset), Some(cursor_digest))
            if cursor_generation == generation && cursor_digest == digest =>
        {
            Ok(offset)
        }
        _ => Err(LexiconServiceError::InvalidField {
            field: "cursor",
            message: "cursor is invalid or stale for this search",
        }),
    }
}

fn component_target_keyword(q: &str) -> Result<&str, LexiconServiceError> {
    let invalid = |message| LexiconServiceError::UnprocessableField {
        field: "q",
        message,
    };
    if q.contains('\0') {
        return Err(invalid("q must not contain NUL characters"));
    }
    if q.trim() != q {
        return Err(invalid("q must not have leading or trailing whitespace"));
    }
    if !(1..=100).contains(&q.chars().count()) {
        return Err(invalid("q must contain between 1 and 100 codepoints"));
    }
    Ok(q)
}

fn component_target_page_size(page_size: Option<u32>) -> Result<usize, LexiconServiceError> {
    let value = page_size.unwrap_or(DEFAULT_COMPONENT_TARGET_PAGE_SIZE);
    if !(1..=MAX_COMPONENT_TARGET_PAGE_SIZE).contains(&value) {
        return Err(LexiconServiceError::InvalidField {
            field: "page_size",
            message: "page_size must be between 1 and 200",
        });
    }
    Ok(value as usize)
}

fn page_size(input: &ResolveSentenceTargetsV3Input) -> Result<usize, LexiconServiceError> {
    let value = match input {
        ResolveSentenceTargetsV3Input::AllPublishedTargets {
            page_size_per_range,
            ..
        }
        | ResolveSentenceTargetsV3Input::SelectedSegments {
            page_size_per_range,
            ..
        } => *page_size_per_range,
    }
    .unwrap_or(20);
    if !(1..=100).contains(&value) {
        return Err(LexiconServiceError::InvalidField {
            field: "page_size",
            message: "page_size_per_range must be between 1 and 100",
        });
    }
    Ok(value as usize)
}

fn selected_cursor_offset(
    input: &ResolveSentenceTargetsV3Input,
    generation: i64,
    fingerprint: &str,
) -> Result<usize, LexiconServiceError> {
    let ResolveSentenceTargetsV3Input::SelectedSegments { cursor, .. } = input else {
        return Ok(0);
    };
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.splitn(4, ':');
    let cursor_generation = parts.next().and_then(|value| value.parse::<i64>().ok());
    let offset = parts.next().and_then(|value| value.parse::<usize>().ok());
    let cursor_dialect = parts.next();
    let cursor_fingerprint = parts.next();
    match (
        cursor_generation,
        offset,
        cursor_dialect,
        cursor_fingerprint,
    ) {
        (Some(cursor_generation), Some(offset), Some(cursor_dialect), Some(cursor_fingerprint))
            if cursor_generation == generation
                && cursor_dialect
                    == crate::lexicon::node_identity::dialect_name(input.source_dialect())
                && cursor_fingerprint == fingerprint
                && offset <= 100_000 =>
        {
            Ok(offset)
        }
        _ => Err(LexiconServiceError::InvalidField {
            field: "cursor",
            message: "cursor is invalid or stale for the selected sentence range",
        }),
    }
}

fn discovery_scopes(dialect: Dialect) -> Vec<String> {
    match dialect {
        Dialect::Common => vec!["uk".to_owned(), "us".to_owned()],
        Dialect::Uk => vec!["uk".to_owned()],
        Dialect::Us => vec!["us".to_owned()],
    }
}

fn automatic_lookup_surfaces(
    tokens: &[crate::lexicon::sentence_target_discovery::Token],
) -> BTreeSet<String> {
    let mut surfaces = tokens
        .iter()
        .map(|token| token.normalized.clone())
        .collect::<BTreeSet<_>>();
    for start in 0..tokens.len() {
        let max_end = (start + MAX_CONTIGUOUS_PHRASE_TOKENS).min(tokens.len());
        let mut literals = Vec::new();
        for (offset, token) in tokens[start..max_end].iter().enumerate() {
            if offset > 0 && token.hard_boundary_before {
                break;
            }
            literals.push(token.normalized.as_str());
            if literals.len() >= 2 {
                surfaces.insert(literals.join(" "));
            }
        }
    }
    surfaces
}

fn validate_selected_segments(
    sentence_text: &str,
    selected_segments: &[SentenceSourceRangeV1],
) -> Result<(Vec<SourceSegment>, String), LexiconServiceError> {
    let mut core_segments = Vec::with_capacity(selected_segments.len());
    let mut previous_end = None;
    for segment in selected_segments {
        if previous_end.is_some_and(|end| segment.start < end)
            || codepoint_slice(
                sentence_text,
                CodepointRange {
                    start: segment.start,
                    end: segment.end,
                },
            ) != Some(segment.surface.as_str())
        {
            return Err(LexiconServiceError::UnprocessableField {
                field: "selected_segments",
                message: "selected_segments must be ordered, non-overlapping and match sentence_text",
            });
        }
        previous_end = Some(segment.end);
        core_segments.push(SourceSegment {
            range: CodepointRange {
                start: segment.start,
                end: segment.end,
            },
        });
    }
    let normalized = selected_segments
        .iter()
        .map(|segment| segment.surface.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize_headword(&normalized)
        .map_err(|_| LexiconServiceError::UnprocessableField {
            field: "selected_segments",
            message: "selected_segments must form a valid word or phrase",
        })?
        .key;
    Ok((core_segments, normalized))
}

fn automatic_range_results(
    sentence_text: &str,
    surfaces: Vec<SentenceDiscoverySurfaceRecord>,
    targets: &HashMap<Uuid, PublishedAssociationTarget>,
    page_size: usize,
    generation: i64,
    source_dialect: Dialect,
) -> Result<Vec<SentenceTargetRangeResultV3>, LexiconServiceError> {
    let aliases = surfaces
        .iter()
        .filter(|surface| surface.entry_kind == "word")
        .cloned()
        .map(|surface| AliasPosting {
            surface: surface.normalized_surface.clone(),
            candidates: vec![surface],
        })
        .collect();
    let contiguous_patterns = surfaces
        .iter()
        .filter(|surface| surface.entry_kind == "phrase")
        .filter_map(|surface| {
            let literals = tokenize(&surface.normalized_surface)
                .into_iter()
                .map(|token| token.normalized)
                .collect::<Vec<_>>();
            (literals.len() >= 2).then(|| ContiguousPattern {
                literals,
                candidates: vec![surface.clone()],
            })
        })
        .collect();
    let catalog = DiscoveryCatalog::build(aliases, contiguous_patterns, Vec::new())
        .map_err(|_| invariant_record())?;
    let metrics = catalog.index_metrics();
    tracing::debug!(
        alias_key_count = metrics.alias_key_count,
        alias_fst_bytes = metrics.alias_fst_bytes,
        contiguous_pattern_count = metrics.contiguous_pattern_count,
        contiguous_automaton_bytes = metrics.contiguous_automaton_bytes,
        "built bounded sentence discovery query indexes"
    );
    catalog
        .discover(sentence_text)
        .matches
        .into_iter()
        .map(|matched| {
            automatic_range_result(
                sentence_text,
                matched,
                targets,
                page_size,
                generation,
                source_dialect,
            )
        })
        .collect()
}

fn automatic_range_result(
    sentence_text: &str,
    matched: DiscoveryMatch<SentenceDiscoverySurfaceRecord>,
    targets: &HashMap<Uuid, PublishedAssociationTarget>,
    page_size: usize,
    generation: i64,
    source_dialect: Dialect,
) -> Result<SentenceTargetRangeResultV3, LexiconServiceError> {
    let kind = match matched.kind {
        MatchKind::Word => SentenceTargetMatchKindV3::Word,
        MatchKind::ContiguousPhrase => SentenceTargetMatchKindV3::ContiguousPhrase,
        MatchKind::SeparablePhrase => SentenceTargetMatchKindV3::SeparablePhrase,
    };
    let evidence = match_evidence(
        sentence_text,
        &matched.source_segments,
        &matched.normalized_surface,
        kind,
    );
    let fingerprint = source_fingerprint(sentence_text, &matched.source_segments)
        .map_err(|_| invariant_record())?;
    range_result(
        sentence_text,
        matched.source_segments,
        matched.normalized_surface,
        published_candidates(&matched.candidates, targets, Some(evidence)),
        Vec::new(),
        RangeResultPagination {
            page_size,
            offset: 0,
            cursor_context: Some((generation, source_dialect, fingerprint)),
        },
    )
}

fn match_evidence(
    sentence_text: &str,
    segments: &[SourceSegment],
    normalized_surface: &str,
    match_kind: SentenceTargetMatchKindV3,
) -> SentenceTargetMatchEvidenceV3 {
    SentenceTargetMatchEvidenceV3 {
        surface: segments
            .iter()
            .filter_map(|segment| codepoint_slice(sentence_text, segment.range))
            .collect::<Vec<_>>()
            .join(" "),
        normalized_surface: normalized_surface.to_owned(),
        match_kind,
    }
}

fn published_candidates(
    surfaces: &[SentenceDiscoverySurfaceRecord],
    targets: &HashMap<Uuid, PublishedAssociationTarget>,
    evidence: Option<SentenceTargetMatchEvidenceV3>,
) -> Vec<PublishedSentenceTargetCandidateV3> {
    let mut grouped = BTreeMap::<
        (Uuid, Option<Uuid>, Uuid, Uuid, Uuid),
        PublishedSentenceTargetCandidateV3,
    >::new();
    for surface in surfaces {
        let Some(target) = targets.get(&surface.entry_id) else {
            continue;
        };
        for candidate in target.sentence_discovery_candidates(
            surface.publication_id,
            surface.pos_id,
            surface.matched_form_id,
            surface.matched_variant_id,
            evidence.clone(),
        ) {
            let key = (
                candidate.entry_id,
                candidate.publication_id,
                candidate.pos_id,
                candidate.base_form_id,
                candidate.matched_variant_id,
            );
            grouped.entry(key).or_insert(candidate);
        }
    }
    grouped.into_values().collect()
}

struct RangeResultPagination {
    page_size: usize,
    offset: usize,
    cursor_context: Option<(i64, Dialect, String)>,
}

fn range_result(
    sentence_text: &str,
    segments: Vec<SourceSegment>,
    normalized_surface: String,
    mut published_matches: Vec<PublishedSentenceTargetCandidateV3>,
    draft_matches: Vec<DraftSentenceTargetCandidateV3>,
    pagination: RangeResultPagination,
) -> Result<SentenceTargetRangeResultV3, LexiconServiceError> {
    let source_segments = segments
        .iter()
        .filter_map(|segment| {
            Some(SentenceSourceRangeV1 {
                start: segment.range.start,
                end: segment.range.end,
                surface: codepoint_slice(sentence_text, segment.range)?.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    let published_total = published_matches.len() as u64;
    published_matches = published_matches
        .into_iter()
        .skip(pagination.offset)
        .collect();
    let has_more = published_matches.len() > pagination.page_size;
    published_matches.truncate(pagination.page_size);
    let next_cursor = has_more
        .then(|| {
            pagination
                .cursor_context
                .as_ref()
                .map(|(generation, source_dialect, fingerprint)| {
                    format!(
                        "{generation}:{}:{}:{fingerprint}",
                        pagination.offset + pagination.page_size,
                        crate::lexicon::node_identity::dialect_name(*source_dialect)
                    )
                })
        })
        .flatten();
    Ok(SentenceTargetRangeResultV3 {
        source_segments,
        segments_fingerprint: source_fingerprint(sentence_text, &segments)
            .map_err(|_| invariant_record())?,
        normalized_surface,
        published_total,
        published_matches,
        next_cursor,
        draft_matches,
    })
}

fn draft_candidate(record: SentenceDiscoveryDraftRecord) -> DraftSentenceTargetCandidateV3 {
    DraftSentenceTargetCandidateV3 {
        entry_id: record.entry_id,
        entry_revision: record.entry_revision,
        headword: record.headword,
        target_state: SentenceTargetDraftStateV3::Draft,
        linkability: SentenceTargetDraftLinkabilityV3::PendingOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_input(
        source_dialect: Dialect,
        cursor: Option<String>,
    ) -> ResolveSentenceTargetsV3Input {
        ResolveSentenceTargetsV3Input::SelectedSegments {
            schema_version: 3,
            sentence_text: "word".to_owned(),
            source_dialect,
            selected_segments: vec![SentenceSourceRangeV1 {
                start: 0,
                end: 4,
                surface: "word".to_owned(),
            }],
            include_drafts: false,
            page_size_per_range: Some(1),
            cursor,
        }
    }

    fn candidate() -> PublishedSentenceTargetCandidateV3 {
        PublishedSentenceTargetCandidateV3 {
            entry_id: Uuid::now_v7(),
            publication_id: Some(Uuid::now_v7()),
            pos_id: Uuid::now_v7(),
            base_form_id: Uuid::now_v7(),
            kind: EntryKind::Word,
            headword: "word".to_owned(),
            pos: "noun".to_owned(),
            matched_form_id: Uuid::now_v7(),
            matched_variant_id: Uuid::now_v7(),
            matched_dialect: Dialect::Uk,
            matched_form_type: "base".to_owned(),
            forms: Vec::new(),
            component_usages: Vec::new(),
            matches: Vec::new(),
            senses: Vec::new(),
        }
    }

    #[test]
    fn automatic_cursor_drives_selected_second_page_and_binds_generation_and_dialect() {
        let segments = vec![SourceSegment {
            range: CodepointRange { start: 0, end: 4 },
        }];
        let fingerprint = source_fingerprint("word", &segments).unwrap();
        let page = range_result(
            "word",
            segments,
            "word".to_owned(),
            vec![candidate(), candidate()],
            Vec::new(),
            RangeResultPagination {
                page_size: 1,
                offset: 0,
                cursor_context: Some((7, Dialect::Uk, fingerprint.clone())),
            },
        )
        .unwrap();
        assert_eq!(page.published_total, 2);
        assert_eq!(page.published_matches.len(), 1);
        let cursor = page
            .next_cursor
            .expect("truncated automatic range needs a cursor");
        assert_eq!(
            selected_cursor_offset(
                &selected_input(Dialect::Uk, Some(cursor.clone())),
                7,
                &fingerprint
            )
            .unwrap(),
            1
        );
        assert!(
            selected_cursor_offset(
                &selected_input(Dialect::Us, Some(cursor.clone())),
                7,
                &fingerprint
            )
            .is_err(),
            "UK cursor must not be reusable for US text"
        );
        assert!(
            selected_cursor_offset(&selected_input(Dialect::Uk, Some(cursor)), 8, &fingerprint)
                .is_err(),
            "cursor from an older discovery generation must be stale"
        );
    }

    #[test]
    fn component_target_keyword_rejects_untrimmed_empty_and_overlong_input() {
        assert_eq!(component_target_keyword("give").unwrap(), "give");
        // 通配符字面量本身是合法关键字；不当通配符用是 escape_like_literal 的职责。
        assert_eq!(component_target_keyword("100%_off").unwrap(), "100%_off");
        assert_eq!(
            component_target_keyword(&"字".repeat(100))
                .unwrap()
                .chars()
                .count(),
            100
        );
        for rejected in ["", " give", "give ", "gi\0ve"] {
            assert!(
                component_target_keyword(rejected).is_err(),
                "{rejected:?} 应被拒"
            );
        }
        assert!(component_target_keyword(&"字".repeat(101)).is_err());
    }

    #[test]
    fn component_target_rank_orders_exact_then_prefix_then_contains() {
        assert_eq!(component_target_rank("give", "give"), 0);
        assert_eq!(component_target_rank("give up", "give"), 1);
        assert_eq!(component_target_rank("forgive", "give"), 2);
        // 点 me：acme 只是包含，me 本身必须是 0 档，不能被字典序压到后面。
        assert_eq!(component_target_rank("acme", "me"), 2);
        assert_eq!(component_target_rank("me", "me"), 0);
    }

    #[test]
    fn component_target_cursor_binds_generation_query_and_kind() {
        let digest = component_target_cursor_digest(
            "give",
            None,
            ComponentTargetMatchV3::Contains,
            false,
            None,
        )
        .unwrap();
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "give",
                Some(EntryKind::Phrase),
                ComponentTargetMatchV3::Contains,
                false,
                None
            )
            .unwrap(),
            "kind 不同的搜索不能共用游标"
        );
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "gave",
                None,
                ComponentTargetMatchV3::Contains,
                false,
                None
            )
            .unwrap()
        );
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "give",
                None,
                ComponentTargetMatchV3::Exact,
                false,
                None
            )
            .unwrap(),
            "匹配方式不同的搜索不能共用游标"
        );
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "give",
                None,
                ComponentTargetMatchV3::Contains,
                true,
                None
            )
            .unwrap(),
            "是否含草稿不同的搜索不能共用游标"
        );
        assert_eq!(component_target_cursor_offset(None, 7, &digest).unwrap(), 0);
        let cursor = format!("7:50:{digest}");
        assert_eq!(
            component_target_cursor_offset(Some(&cursor), 7, &digest).unwrap(),
            50
        );
        for (stale, generation) in [
            (cursor.clone(), 8),                     // 词库变了
            (format!("7:50:{}", "0".repeat(64)), 7), // 换了关键字/kind
            (format!("7:-1:{digest}"), 7),           // offset 非法
            ("garbage".to_owned(), 7),
            (String::new(), 7),
        ] {
            assert!(
                component_target_cursor_offset(Some(&stale), generation, &digest).is_err(),
                "{stale:?} @ generation {generation} 应被拒"
            );
        }
    }

    #[test]
    fn component_target_page_size_defaults_to_fifty_and_caps_at_two_hundred() {
        assert_eq!(component_target_page_size(None).unwrap(), 50);
        assert_eq!(component_target_page_size(Some(1)).unwrap(), 1);
        assert_eq!(component_target_page_size(Some(200)).unwrap(), 200);
        assert!(component_target_page_size(Some(0)).is_err());
        assert!(component_target_page_size(Some(201)).is_err());
    }
}
