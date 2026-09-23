use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use crate::lexicon::dto::{
    ComponentTargetMatchV3, PublishedSentenceTargetCandidateV3, SearchComponentTargetsV3Input,
    SearchComponentTargetsV3Response, SentenceTargetMatchEvidenceV3,
};

use super::sentence_association::{PublishedAssociationCandidateKey, PublishedAssociationTarget};
use super::v3::load_draft_component_targets;

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

// Revisions and publication IDs are not stable pagination identities.
type TargetNodeKey = (Uuid, Uuid, Uuid, Uuid);
type ComponentPageKey = (i32, bool, String, TargetNodeKey);

fn target_node_key(key: PublishedAssociationCandidateKey) -> TargetNodeKey {
    (
        key.entry_id,
        key.pos_id,
        key.base_form_id,
        key.matched_variant_id,
    )
}

fn component_page_key(candidate: &ComponentTargetCandidateIndex) -> ComponentPageKey {
    (
        candidate.match_rank,
        candidate.draft,
        candidate.headword.to_string(),
        target_node_key(candidate.key),
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryCursor<K> {
    context: String,
    after: K,
}

fn encode_discovery_cursor<K: Serialize>(context: &str, after: K) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&DiscoveryCursor {
            context: context.to_owned(),
            after,
        })
        .expect("discovery cursor serialization cannot fail"),
    )
}

fn decode_discovery_cursor<K: serde::de::DeserializeOwned>(
    cursor: Option<&str>,
    context: &str,
) -> Result<Option<K>, LexiconServiceError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DiscoveryCursor<K>>(&bytes).ok())
        .filter(|cursor| cursor.context == context);
    decoded
        .map(|cursor| Some(cursor.after))
        .ok_or(LexiconServiceError::InvalidField {
            field: "cursor",
            message: "cursor is invalid for this search",
        })
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
    /// 成分用词 / 正文关联目标的关键字检索：与 resolve 共用候选组装，把「词面等值」换成
    /// `match` 指定的匹配方式（默认包含，`exact` 为归一化后等值）。默认只回已发布且未归档的词条；
    /// `include_drafts` 为真时再加上当前 V3 草稿（不限创建者，候选没有 `publication_id`），
    /// 包括已发布词条的新增节点；发布与草稿分别组装，不能按 entry_id 覆盖快照。
    ///
    /// 顺序：词面等于 q 的词条最前、以 q 开头的其次、其余按 headword；同档位已发布优先于草稿
    /// （SQL 与 Rust 两侧同一规则）。游标绑定查询摘要及稳定节点排序键，不绑定词库版本；
    /// total 是当前请求可见的弱一致计数，不控制分页结束。
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
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let entry_rank = published_entries
            .iter()
            .map(|entry| ((entry.entry_id, false), entry.match_rank))
            .chain(
                draft_entries
                    .iter()
                    .map(|entry| ((entry.entry_id, true), entry.match_rank)),
            )
            .collect::<HashMap<_, _>>();
        let published_entry_ids = published_entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<HashSet<_>>();
        let draft_entry_ids = draft_entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<HashSet<_>>();
        let after: Option<ComponentPageKey> =
            decode_discovery_cursor(input.cursor.as_deref(), &cursor_digest)?;
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
            let published_targets =
                LexiconRepository::current_publication_snapshots(&mut transaction, &published_ids)
                    .await
                    .map_err(repository_error)?
                    .into_iter()
                    .map(|record| {
                        PublishedAssociationTarget::from_snapshot(record.snapshot, true)
                            .map(|target| (record.entry_id, target))
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?;
            let draft_targets = load_draft_component_targets(&mut transaction, &draft_ids)
                .await?
                .into_iter()
                .map(|target| {
                    let id = target.id;
                    PublishedAssociationTarget::from_component_target(target)
                        .map(|target| (id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            for (surfaces, targets) in [(&published, &published_targets), (&drafts, &draft_targets)]
            {
                for surface in surfaces {
                    let Some(target) = targets.get(&surface.entry_id) else {
                        continue;
                    };
                    let headword = Arc::<str>::from(target.headword());
                    for key in target.sentence_discovery_candidate_keys(
                        surface.publication_id,
                        surface.pos_id,
                        surface.matched_form_id,
                        surface.matched_variant_id,
                    ) {
                        candidate_index.entry(key).or_insert_with(|| {
                            ComponentTargetCandidateIndex {
                                key,
                                match_rank: entry_rank
                                    .get(&(key.entry_id, key.publication_id.is_none()))
                                    .copied()
                                    .unwrap_or(2),
                                draft: key.publication_id.is_none(),
                                headword: headword.clone(),
                            }
                        });
                    }
                }
            }
        }
        let mut candidate_index = candidate_index.into_values().collect::<Vec<_>>();
        candidate_index.sort_by_cached_key(component_page_key);
        let total = candidate_index.len();
        candidate_index.retain(|candidate| {
            after
                .as_ref()
                .is_none_or(|after| component_page_key(candidate) > *after)
        });
        let has_more = candidate_index.len() > page_size;
        candidate_index.truncate(page_size);
        let next_cursor = has_more.then(|| {
            encode_discovery_cursor(
                &cursor_digest,
                component_page_key(candidate_index.last().expect("nonempty page")),
            )
        });
        let page_keys = candidate_index
            .iter()
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
        let page_published_targets =
            LexiconRepository::current_publication_snapshots(&mut transaction, &page_published_ids)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| {
                    PublishedAssociationTarget::from_snapshot(record.snapshot, true)
                        .map(|target| (record.entry_id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
        let page_draft_targets = load_draft_component_targets(&mut transaction, &page_draft_ids)
            .await?
            .into_iter()
            .map(|target| {
                let id = target.id;
                PublishedAssociationTarget::from_component_target(target).map(|target| (id, target))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let requested_keys = page_keys.iter().copied().collect::<HashSet<_>>();
        let mut materialized = published_candidates(&page_published, &page_published_targets, None);
        materialized.extend(published_candidates(
            &page_drafts,
            &page_draft_targets,
            None,
        ));
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

fn discovery_scopes(dialect: Dialect) -> Vec<String> {
    match dialect {
        Dialect::Common => vec!["uk".to_owned(), "us".to_owned()],
        Dialect::Uk => vec!["uk".to_owned()],
        Dialect::Us => vec!["us".to_owned()],
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn component_target_cursor_binds_query_and_kind_not_dataset_generation() {
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
        let key: ComponentPageKey = (
            0,
            false,
            "give".to_owned(),
            (
                Uuid::now_v7(),
                Uuid::now_v7(),
                Uuid::now_v7(),
                Uuid::now_v7(),
            ),
        );
        assert_eq!(
            decode_discovery_cursor::<ComponentPageKey>(None, &digest).unwrap(),
            None
        );
        let cursor = encode_discovery_cursor(&digest, &key);
        assert_eq!(
            decode_discovery_cursor::<ComponentPageKey>(Some(&cursor), &digest).unwrap(),
            Some(key)
        );
        assert!(
            decode_discovery_cursor::<ComponentPageKey>(Some(&cursor), "different query").is_err()
        );
        for invalid in ["7:50:old-offset-cursor", "garbage", ""] {
            assert!(decode_discovery_cursor::<ComponentPageKey>(Some(invalid), &digest).is_err());
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
