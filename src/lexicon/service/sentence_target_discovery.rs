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

const DEFAULT_COMPONENT_TARGET_PAGE_SIZE: u32 = 50;
const MAX_COMPONENT_TARGET_PAGE_SIZE: u32 = 200;
/// 每批快照的词条数；只限制峰值，不限制可遍历的结果集。
const COMPONENT_TARGET_SNAPSHOT_BATCH_SIZE: usize = 200;

#[derive(Debug, Clone)]
struct ComponentTargetCandidateIndex {
    key: PublishedAssociationCandidateKey,
    match_rank: i32,
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
        candidate.key.publication_id.is_none(),
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
    /// 成分目标检索按请求包含草稿，游标绑定查询范围和稳定节点排序键。
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
            input.entry_id,
            input.include_drafts,
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
        let after: Option<ComponentPageKey> =
            decode_discovery_cursor(input.cursor.as_deref(), &cursor_digest)?;
        let mut candidate_index =
            HashMap::<PublishedAssociationCandidateKey, ComponentTargetCandidateIndex>::new();
        for drafts in [false, true]
            .into_iter()
            .filter(|drafts| !drafts || input.include_drafts)
        {
            let entries = LexiconRepository::component_target_entry_matches(
                &mut transaction,
                &dialect_scopes,
                &lookup,
                input.kind,
                input.entry_id,
                exact,
                drafts,
            )
            .await
            .map_err(repository_error)?;
            let entry_ids = entries
                .iter()
                .map(|entry| entry.entry_id)
                .collect::<Vec<_>>();
            let entry_rank = entries
                .iter()
                .map(|entry| (entry.entry_id, entry.match_rank))
                .collect::<HashMap<_, _>>();
            for batch in entry_ids.chunks(COMPONENT_TARGET_SNAPSHOT_BATCH_SIZE) {
                let surfaces = LexiconRepository::component_target_surfaces(
                    &mut transaction,
                    &dialect_scopes,
                    &lookup,
                    input.kind,
                    batch,
                    exact,
                    drafts,
                )
                .await
                .map_err(repository_error)?;
                let targets = component_target_snapshots(&mut transaction, batch, drafts).await?;
                for surface in &surfaces {
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
                                match_rank: entry_rank.get(&key.entry_id).copied().unwrap_or(2),
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
        let mut materialized = Vec::new();
        for drafts in [false, true]
            .into_iter()
            .filter(|drafts| !drafts || input.include_drafts)
        {
            let ids = page_keys
                .iter()
                .filter(|key| key.publication_id.is_none() == drafts)
                .map(|key| key.entry_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if ids.is_empty() {
                continue;
            }
            let surfaces = LexiconRepository::component_target_surfaces(
                &mut transaction,
                &dialect_scopes,
                &lookup,
                input.kind,
                &ids,
                exact,
                drafts,
            )
            .await
            .map_err(repository_error)?;
            let targets = component_target_snapshots(&mut transaction, &ids, drafts).await?;
            materialized.extend(published_candidates(&surfaces, &targets, None));
        }
        let requested_keys = page_keys.iter().copied().collect::<HashSet<_>>();
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

async fn component_target_snapshots(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_ids: &[Uuid],
    drafts: bool,
) -> Result<HashMap<Uuid, PublishedAssociationTarget>, LexiconServiceError> {
    if drafts {
        LexiconRepository::component_target_drafts(tx, entry_ids)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|row| {
                let id = row.id;
                PublishedAssociationTarget::from_draft(row).map(|target| (id, target))
            })
            .collect()
    } else {
        LexiconRepository::current_publication_snapshots(tx, entry_ids)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|row| {
                PublishedAssociationTarget::from_snapshot(row.snapshot, true)
                    .map(|target| (row.entry_id, target))
            })
            .collect()
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
    entry_id: Option<Uuid>,
    include_drafts: bool,
) -> Result<String, LexiconServiceError> {
    Ok(hex_digest(
        &sha256_json(&(q, kind, match_mode, entry_id, include_drafts))
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
            None,
            false,
        )
        .unwrap();
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "give",
                Some(EntryKind::Phrase),
                ComponentTargetMatchV3::Contains,
                None,
                false
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
                None,
                false
            )
            .unwrap()
        );
        assert_ne!(
            digest,
            component_target_cursor_digest(
                "give",
                None,
                ComponentTargetMatchV3::Exact,
                None,
                false
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
                Some(Uuid::now_v7()),
                false
            )
            .unwrap(),
            "限定词条不同的搜索不能共用游标"
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
