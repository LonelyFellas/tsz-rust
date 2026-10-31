use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::{AppError, ErrorCode};

use super::dto::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairLink {
    pub id: Uuid,
    pub uk_id: Uuid,
    pub us_id: Uuid,
}

pub(super) fn into_legacy(
    request: SavePairedDubbingGroupRequest,
) -> Result<(SaveDubbingGroupRequest, Vec<PairLink>), AppError> {
    let input = request.group;
    let mut ids = BTreeSet::from([input.id]);
    let mut links = vec![];
    let mut uk = vec![];
    let mut us = vec![];
    if input.pairs.len() > 20 {
        return Err(invalid());
    }
    for pair in input.pairs {
        for id in [pair.id, pair.uk.id, pair.us.id] {
            if id.is_nil() || !ids.insert(id) {
                return Err(invalid());
            }
        }
        links.push(PairLink {
            id: pair.id,
            uk_id: pair.uk.id,
            us_id: pair.us.id,
        });
        for (side, target) in [(pair.uk, &mut uk), (pair.us, &mut us)] {
            target.push(DubbingRecommendation {
                id: side.id,
                name: pair.name.clone(),
                selected: pair.selected,
                voice_alias: side.voice_alias,
                parameters: side.parameters,
            });
        }
    }
    Ok((
        SaveDubbingGroupRequest {
            group: DubbingGroup {
                id: input.id,
                name: input.name,
                areas: input.areas,
                uk,
                us,
                revision: request.base_revision,
                is_global: input.is_global,
            },
            base_revision: request.base_revision,
        },
        links,
    ))
}

fn invalid() -> AppError {
    AppError::bad_request(ErrorCode::InvalidRequestBody, "推荐套身份不完整或重复")
}

// The singleton row is locked by the caller, so all identity checks and writes serialize.
pub(super) async fn validate_identities(
    conn: &mut PgConnection,
    groups: &[DubbingGroup],
    input: &DubbingGroup,
    links: Option<&[PairLink]>,
) -> Result<(), AppError> {
    let rows = sqlx::query("SELECT group_id,pairs FROM speech.dubbing_group_pairings")
        .fetch_all(conn)
        .await
        .map_err(AppError::internal)?;
    let mut pair_ids = BTreeSet::new();
    let mut other_pair_ids = BTreeSet::new();
    for row in rows {
        let stored: Vec<PairLink> =
            serde_json::from_value(row.get("pairs")).map_err(AppError::internal)?;
        for link in stored {
            pair_ids.insert(link.id);
            if row.get::<Uuid, _>("group_id") != input.id {
                other_pair_ids.insert(link.id);
            }
        }
    }
    if pair_ids.contains(&input.id)
        || input
            .uk
            .iter()
            .chain(&input.us)
            .any(|side| pair_ids.contains(&side.id))
    {
        return Err(invalid());
    }
    if let Some(links) = links {
        let existing_ids: BTreeSet<_> = groups
            .iter()
            .flat_map(|group| {
                std::iter::once(group.id)
                    .chain(group.uk.iter().chain(&group.us).map(|side| side.id))
            })
            .collect();
        if links
            .iter()
            .any(|link| existing_ids.contains(&link.id) || other_pair_ids.contains(&link.id))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) async fn read(conn: &mut PgConnection) -> Result<DubbingGroupsV3Response, AppError> {
    // One statement gives side settings and pairing metadata the same MVCC snapshot.
    let row = sqlx::query("SELECT groups,revision,order_revision,COALESCE((SELECT jsonb_object_agg(group_id::text,pairs) FROM speech.dubbing_group_pairings),'{}'::jsonb) AS pairings FROM speech.dubbing_groups WHERE singleton")
        .fetch_one(conn).await.map_err(AppError::internal)?;
    let mut groups: Vec<DubbingGroup> =
        serde_json::from_value(row.get("groups")).map_err(AppError::internal)?;
    groups.sort_by_key(|group| group.is_global);
    let pairings: serde_json::Value = row.get("pairings");
    let groups = groups
        .into_iter()
        .map(|group| {
            let recommendations = match pairings.get(group.id.to_string()) {
                None => DubbingRecommendationsV3::Legacy {
                    uk: group.uk.clone(),
                    us: group.us.clone(),
                },
                Some(value) => {
                    let links: Vec<PairLink> =
                        serde_json::from_value(value.clone()).map_err(AppError::internal)?;
                    let mut identities = BTreeSet::from([group.id]);
                    if links.len() != group.uk.len()
                        || links.len() != group.us.len()
                        || links.iter().any(|link| {
                            [link.id, link.uk_id, link.us_id]
                                .into_iter()
                                .any(|id| id.is_nil() || !identities.insert(id))
                        })
                    {
                        return Err(AppError::internal(anyhow::anyhow!(
                            "incomplete dubbing pairing"
                        )));
                    }
                    let pairs = links
                        .into_iter()
                        .map(|link| {
                            let uk = group.uk.iter().find(|item| item.id == link.uk_id);
                            let us = group.us.iter().find(|item| item.id == link.us_id);
                            match (uk, us) {
                                (Some(uk), Some(us))
                                    if uk.name == us.name && uk.selected == us.selected =>
                                {
                                    let side = |item: &DubbingRecommendation| DubbingPairVoice {
                                        id: item.id,
                                        voice_alias: item.voice_alias.clone(),
                                        parameters: item.parameters.clone(),
                                    };
                                    Ok(DubbingRecommendationPair {
                                        id: link.id,
                                        name: uk.name.clone(),
                                        selected: uk.selected,
                                        uk: side(uk),
                                        us: side(us),
                                    })
                                }
                                _ => Err(AppError::internal(anyhow::anyhow!(
                                    "inconsistent dubbing pairing"
                                ))),
                            }
                        })
                        .collect::<Result<Vec<_>, AppError>>()?;
                    DubbingRecommendationsV3::Paired { pairs }
                }
            };
            Ok(DubbingGroupV3 {
                id: group.id,
                name: group.name,
                areas: group.areas,
                revision: group.revision,
                is_global: group.is_global,
                recommendations,
            })
        })
        .collect::<Result<_, AppError>>()?;
    Ok(DubbingGroupsV3Response {
        groups,
        revision: row.get("revision"),
        order_revision: row.get("order_revision"),
    })
}
