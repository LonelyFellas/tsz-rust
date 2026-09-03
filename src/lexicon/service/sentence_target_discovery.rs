use super::*;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::lexicon::{
    dto::{
        DraftSentenceTargetCandidateV3, PublishedSentenceTargetCandidateV3,
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

use super::sentence_association::PublishedAssociationTarget;

const MAX_DISCOVERY_TOKENS: usize = 100;
const MAX_CONTIGUOUS_PHRASE_TOKENS: usize = 40;
const DEFAULT_COMPONENT_TARGET_PAGE_SIZE: u32 = 50;
const MAX_COMPONENT_TARGET_PAGE_SIZE: u32 = 200;
/// 关键字检索一次最多取回的词面行数。ILIKE 的前置通配符走不了索引，这个上限只框住返回负载
/// 与组装成本，不减少扫描；命中触顶时响应的 `truncated` 为 true。
const MAX_COMPONENT_TARGET_SURFACE_ROWS: i64 = 2000;

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
                    PublishedAssociationTarget::from_snapshot(
                        record.snapshot,
                        true,
                        record.publication_id,
                    )
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

    /// 短语成分目标的关键字检索：与 resolve 共用发布过滤与候选组装，只把「词面等值」换成
    /// 「词面包含」。只回已发布且未归档的词条——成分关联要存 `target_publication_id`，
    /// 草稿没有发布快照，保存时会被 `validate_phrase_components` 拒掉。
    pub async fn search_component_targets_v3(
        &self,
        input: SearchComponentTargetsV3Input,
        allow_v3: bool,
    ) -> Result<SearchComponentTargetsV3Response, LexiconServiceError> {
        if !allow_v3 {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        let keyword = component_target_keyword(&input.q)?;
        let page_size = component_target_page_size(input.page_size)?;
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
        let surfaces = LexiconRepository::published_component_target_surfaces(
            &mut transaction,
            &dialect_scopes,
            keyword,
            input.kind,
            MAX_COMPONENT_TARGET_SURFACE_ROWS,
        )
        .await
        .map_err(repository_error)?;
        let scan_capped = surfaces.len() as i64 >= MAX_COMPONENT_TARGET_SURFACE_ROWS;
        // 快照是整份发布 JSONB（全部词形、词义、例句、关系）。词面行上限是 2000，直接照单
        // 取快照就会为了回一页而拉回并解析上百份，绝大多数随后被 truncate 丢掉。一页最多
        // MAX_COMPONENT_TARGET_PAGE_SIZE 条候选（去重键含 entry_id，200 个词条至少产出
        // 200 条互不相同的候选），取回的词条数不必超过它。
        // 去重按 surfaces 的首次出现顺序，而不是 entry_id 的 UUID 序：surfaces 已由 SQL 按
        // normalized_surface 排好，与那 2000 行窗口同一口径；换成 UUID 序会让截断轴与展示
        // 排序无关，搜 cat 反而可能丢掉 cat 本身。
        let mut seen = HashSet::new();
        let mut entry_ids = surfaces
            .iter()
            .map(|surface| surface.entry_id)
            .filter(|entry_id| seen.insert(*entry_id))
            .collect::<Vec<_>>();
        let entries_capped = entry_ids.len() > MAX_COMPONENT_TARGET_PAGE_SIZE as usize;
        entry_ids.truncate(MAX_COMPONENT_TARGET_PAGE_SIZE as usize);
        let targets =
            LexiconRepository::current_publication_snapshots(&mut transaction, &entry_ids)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| {
                    PublishedAssociationTarget::from_snapshot(
                        record.snapshot,
                        true,
                        record.publication_id,
                    )
                    .map(|target| (record.entry_id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
        transaction.commit().await.map_err(database_error)?;

        // 没有句子区间就没有「命中了哪一段」，候选的 matches 留空而不是编一条证据。
        let mut matches = published_candidates(&surfaces, &targets, None);
        // published_candidates 按 id 元组分组，顺序对人不可读；稳定排序换成按词面，
        // 同词面内仍保持分组键的确定顺序。
        matches.sort_by(|left, right| left.headword.cmp(&right.headword));
        let total = matches.len() as u64;
        let truncated = scan_capped || entries_capped || matches.len() > page_size;
        matches.truncate(page_size);

        Ok(SearchComponentTargetsV3Response {
            schema_version: 3,
            matches,
            total,
            truncated,
        })
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
    let mut grouped =
        BTreeMap::<(Uuid, Uuid, Uuid, Uuid, Uuid), PublishedSentenceTargetCandidateV3>::new();
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
    use crate::lexicon::dto::WordFormTypeV3;

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
            publication_id: Uuid::now_v7(),
            pos_id: Uuid::now_v7(),
            base_form_id: Uuid::now_v7(),
            kind: EntryKind::Word,
            headword: "word".to_owned(),
            pos: "noun".to_owned(),
            matched_form_id: Uuid::now_v7(),
            matched_variant_id: Uuid::now_v7(),
            matched_dialect: Dialect::Uk,
            matched_form_type: WordFormTypeV3::Base,
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
    fn component_target_page_size_defaults_to_fifty_and_caps_at_two_hundred() {
        assert_eq!(component_target_page_size(None).unwrap(), 50);
        assert_eq!(component_target_page_size(Some(1)).unwrap(), 1);
        assert_eq!(component_target_page_size(Some(200)).unwrap(), 200);
        assert!(component_target_page_size(Some(0)).is_err());
        assert!(component_target_page_size(Some(201)).is_err());
    }
}
