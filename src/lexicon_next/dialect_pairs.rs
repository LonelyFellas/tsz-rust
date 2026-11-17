//! Pair identities are immutable. Under the existing list-writer lock, one
//! save advances the source and all affected peer revisions atomically.
use super::{language, model::NextDraft, repository};
use crate::{
    admin::permissions::AdminAuthorization,
    error::{AppError, ErrorCode},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row, types::Json};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum NextDialectUsage {
    #[serde(rename = "general")]
    General,
    #[serde(rename = "en-GB")]
    British,
    #[serde(rename = "en-US")]
    American,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDialectTarget {
    pub entry_id: Uuid,
    pub sense_id: Uuid,
    pub revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDialectChange {
    pub sense_id: Uuid,
    pub usage: NextDialectUsage,
    #[serde(default)]
    pub no_equivalent: bool,
    #[serde(default)]
    pub note: String,
    pub target: Option<NextDialectTarget>,
    pub previous_target_revision: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NextDialectPeer {
    pub entry_id: Uuid,
    pub sense_id: Uuid,
    pub revision: i64,
    pub headword: String,
    pub gloss: String,
    pub part_of_speech: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NextDialectSense {
    pub sense_id: Uuid,
    pub usage: NextDialectUsage,
    pub no_equivalent: bool,
    pub note: String,
    pub paired: bool,
    pub target: Option<NextDialectPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Binding {
    sense_id: Uuid,
    usage: NextDialectUsage,
    no_equivalent: bool,
    note: String,
    pair_id: Option<Uuid>,
}
type Bindings = BTreeMap<Uuid, Binding>;
fn changed() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        Some("dialect_changes"),
        "配对或关联词条已变化，请重新打开词条并选择词义",
    )
}
fn invalid(msg: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, msg)
}

async fn bindings(
    conn: &mut PgConnection,
    entry: Uuid,
    revision: i64,
) -> Result<Bindings, AppError> {
    let items: Json<Vec<Binding>> = sqlx::query_scalar(
        "SELECT dialect_senses FROM lexicon_next.draft_revisions WHERE entry_id=$1 AND revision=$2",
    )
    .bind(entry)
    .bind(revision)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    Ok(items.0.into_iter().map(|s| (s.sense_id, s)).collect())
}

// This projection uses only immutable published revisions in published mode.
// A later draft or a one-sided publication cannot provide a peer label or URL.
pub(super) async fn project(
    conn: &mut PgConnection,
    entry: Uuid,
    revision: i64,
    published: bool,
) -> Result<Vec<NextDialectSense>, AppError> {
    // Group before joining documents: even if all senses point into the same
    // entry, its chosen revision crosses the DB boundary only once.
    let mut rows=sqlx::query(r#"WITH endpoints AS (
        SELECT s.item, CASE WHEN p.gb_entry=$1 THEN p.us_entry ELSE p.gb_entry END AS peer,
          CASE WHEN p.gb_entry=$1 THEN p.us_sense ELSE p.gb_sense END AS sid
        FROM lexicon_next.draft_revisions r
        CROSS JOIN LATERAL jsonb_array_elements(r.dialect_senses) s(item)
        LEFT JOIN lexicon_next.dialect_pairs p ON p.id=(s.item->>'pair_id')::uuid
        WHERE r.entry_id=$1 AND r.revision=$2
    ), grouped AS MATERIALIZED (
        SELECT peer,jsonb_agg(jsonb_build_array(item,sid)) AS members FROM endpoints GROUP BY peer
    )
    SELECT g.peer,g.members,e.medium_locale,r.revision,r.document,r.dialect_senses
    FROM grouped g LEFT JOIN lexicon_next.entries e ON e.id=g.peer AND (NOT $3 OR e.status='published')
    LEFT JOIN LATERAL (SELECT revision FROM lexicon_next.publications WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1) pub ON $3
    LEFT JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=CASE WHEN $3 THEN pub.revision ELSE e.revision END"#)
      .bind(entry).bind(revision).bind(published).fetch(conn);
    let mut result = Vec::new();
    while let Some(row) = futures_util::TryStreamExt::try_next(&mut rows)
        .await
        .map_err(AppError::internal)?
    {
        let doc = row.get::<Option<Json<NextDraft>>, _>("document");
        let bindings: Bindings = row
            .get::<Option<Json<Vec<Binding>>>, _>("dialect_senses")
            .into_iter()
            .flat_map(|j| j.0)
            .map(|s| (s.sense_id, s))
            .collect();
        let senses: BTreeMap<_, _> = doc
            .iter()
            .flat_map(|d| &d.senses)
            .map(|s| (s.id, s))
            .collect();
        let Json(members): Json<Vec<(Binding, Option<Uuid>)>> = row.get("members");
        for (b, sid) in members {
            let target = sid.and_then(|id| {
                let doc = doc.as_ref()?;
                if bindings.get(&id)?.pair_id != b.pair_id {
                    return None;
                }
                let s = senses.get(&id)?;
                let medium: &str = row.get("medium_locale");
                let gloss = s
                    .equivalents
                    .iter()
                    .map(|v| (&v.locale, &v.text))
                    .chain(s.definitions.iter().map(|v| (&v.locale, &v.text)))
                    .find(|(l, t)| l.as_str() == medium && !t.is_empty())
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                Some(NextDialectPeer {
                    entry_id: row.get("peer"),
                    sense_id: id,
                    revision: row.get("revision"),
                    headword: base_spelling(
                        doc,
                        s.part_of_speech.as_deref(),
                        if b.usage == NextDialectUsage::British {
                            "us"
                        } else {
                            "uk"
                        },
                    ),
                    gloss,
                    part_of_speech: s.part_of_speech.clone(),
                })
            });
            result.push(NextDialectSense {
                sense_id: b.sense_id,
                usage: b.usage,
                no_equivalent: b.no_equivalent,
                note: b.note,
                paired: b.pair_id.is_some(),
                target,
            });
        }
    }
    result.sort_unstable_by_key(|r| r.sense_id);
    Ok(result)
}

// Do not substitute a plural or inflection just because it occurs first.
fn base_spelling(draft: &NextDraft, pos: Option<&str>, side: &str) -> String {
    if let Some(editor) = &draft.form_editor {
        let forms = editor
            .pos
            .iter()
            .filter(|p| Some(p.pos.as_str()) == pos)
            .flat_map(|p| &p.forms)
            .filter(|f| f.form_type == "base");
        for form in forms {
            if let Some(content) = editor.contents.iter().find(|v| v.id == form.content_id) {
                let variants = content.regional_variants.variants();
                if let Some((_, v)) = variants.iter().find(|(region, v)| {
                    (*region == side || *region == "common") && !v.spelling.trim().is_empty()
                }) {
                    return v.spelling.clone();
                }
            }
        }
    } else {
        let locale = if side == "us" { "en-US" } else { "en-GB" };
        if let Some(form) = draft
            .forms
            .iter()
            .find(|v| v.locale == locale)
            .or_else(|| draft.forms.iter().find(|v| v.locale == "en"))
        {
            return form.text.clone();
        }
    }
    draft.headword.clone()
}

struct Peer {
    revision: i64,
    values: Bindings,
}
async fn load_peer(
    conn: &mut PgConnection,
    access: &AdminAuthorization,
    medium: &str,
    id: Uuid,
    expected: i64,
) -> Result<Peer, AppError> {
    let (owner,revision,status,lang,locale):(Uuid,i64,String,String,String)=sqlx::query_as("SELECT owner_id,revision,status,target_language,medium_locale FROM lexicon_next.entries WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(changed)?;
    access.require_owned_action("words.edit", owner)?;
    if revision != expected || status == "archived" {
        return Err(changed());
    }
    if lang != "en" || locale != medium {
        return Err(invalid("只能配对同一媒介语词库中的英语义项"));
    }
    Ok(Peer {
        revision,
        values: bindings(conn, id, revision).await?,
    })
}

pub(super) async fn apply(
    conn: &mut PgConnection,
    access: &AdminAuthorization,
    entry: &super::model::NextEntry,
    draft: &NextDraft,
    changes: Vec<NextDialectChange>,
) -> Result<Vec<Binding>, AppError> {
    let mut own = bindings(conn, entry.id, entry.revision).await?;
    let retained: BTreeSet<_> = draft.senses.iter().map(|s| s.id).collect();
    if own
        .values()
        .any(|s| !retained.contains(&s.sense_id) && s.pair_id.is_some())
    {
        return Err(invalid("请先解除并保存配对，再删除该义项"));
    }
    own.retain(|id, _| retained.contains(id));
    let mut peers = BTreeMap::<Uuid, Peer>::new();
    let mut touched = BTreeSet::new();
    for c in changes {
        if entry.target_language != "en"
            || !retained.contains(&c.sense_id)
            || !touched.insert(c.sense_id)
            || (!c.note.is_empty() && language::text(&c.note, 1000).is_err())
            || (!c.no_equivalent && !c.note.is_empty())
            || (c.no_equivalent && c.target.is_some())
            || (c.usage == NextDialectUsage::General && (c.target.is_some() || c.no_equivalent))
        {
            return Err(invalid("词义的方言、配对或说明无效"));
        }
        let mut endpoints = Vec::new();
        let mut retain_pair = false;
        if let Some(pair) = own.get(&c.sense_id).and_then(|s| s.pair_id) {
            let (gb,gs,us,ss):(Uuid,Uuid,Uuid,Uuid)=sqlx::query_as("SELECT gb_entry,gb_sense,us_entry,us_sense FROM lexicon_next.dialect_pairs WHERE id=$1").bind(pair).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
            let (id, sid) = if gb == entry.id { (us, ss) } else { (gb, gs) };
            let previous_revision = c.previous_target_revision.ok_or_else(changed)?;
            retain_pair = c
                .target
                .as_ref()
                .is_some_and(|target| target.entry_id == id && target.sense_id == sid);
            if retain_pair
                && c.target
                    .as_ref()
                    .is_none_or(|target| target.revision != previous_revision)
            {
                return Err(changed());
            }
            endpoints.push((id, sid, previous_revision, Some(pair), retain_pair));
        } else if c.previous_target_revision.is_some() {
            return Err(changed());
        }
        if let Some(target) = c.target.as_ref()
            && !retain_pair
        {
            endpoints.push((
                target.entry_id,
                target.sense_id,
                target.revision,
                None,
                false,
            ));
        }
        let mut pair_id = None;
        for (id, sid, rev, old, keep) in endpoints {
            if id == entry.id {
                return Err(invalid("请选择另一个词条的义项"));
            }
            if keep {
                let peer = load_peer(
                    conn,
                    access,
                    entry.medium_locale.as_deref().unwrap_or("zh-Hans"),
                    id,
                    rev,
                )
                .await?;
                let other = peer
                    .values
                    .get(&sid)
                    .ok_or_else(|| invalid("请先在对方义项设置相反的方言"))?;
                if other.pair_id != old {
                    return Err(changed());
                }
                if c.usage == other.usage || other.usage == NextDialectUsage::General {
                    return Err(invalid("双方必须属于相反的方言"));
                }
                pair_id = old;
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(slot) = peers.entry(id) {
                slot.insert(
                    load_peer(
                        conn,
                        access,
                        entry.medium_locale.as_deref().unwrap_or("zh-Hans"),
                        id,
                        rev,
                    )
                    .await?,
                );
            }
            let peer = peers.get_mut(&id).expect("loaded");
            if peer.revision != rev {
                return Err(changed());
            }
            let other = peer
                .values
                .get_mut(&sid)
                .ok_or_else(|| invalid("请先在对方义项设置相反的方言"))?;
            if other.pair_id != old {
                return Err(changed());
            }
            if old.is_some() {
                other.pair_id = None;
            } else {
                if c.usage == other.usage || other.usage == NextDialectUsage::General {
                    return Err(invalid("双方必须属于相反的方言"));
                }
                let (gb, gs, us, ss) = if c.usage == NextDialectUsage::British {
                    (entry.id, c.sense_id, id, sid)
                } else {
                    (id, sid, entry.id, c.sense_id)
                };
                let pair = Uuid::now_v7();
                sqlx::query("INSERT INTO lexicon_next.dialect_pairs(id,gb_entry,gb_sense,us_entry,us_sense) VALUES($1,$2,$3,$4,$5)").bind(pair).bind(gb).bind(gs).bind(us).bind(ss).execute(&mut *conn).await.map_err(AppError::internal)?;
                other.pair_id = Some(pair);
                other.no_equivalent = false;
                other.note.clear();
                pair_id = Some(pair);
            }
        }
        if c.usage == NextDialectUsage::General {
            own.remove(&c.sense_id);
        } else {
            own.insert(
                c.sense_id,
                Binding {
                    sense_id: c.sense_id,
                    usage: c.usage,
                    no_equivalent: c.no_equivalent,
                    note: c.note,
                    pair_id,
                },
            );
        }
    }
    for (id, peer) in peers {
        let next = peer.revision.checked_add(1).ok_or_else(language::invalid)?;
        let entry = repository::get_on(conn, id).await?;
        repository::snapshot(
            conn,
            id,
            next,
            access.admin_id,
            &entry.draft,
            &peer.values.into_values().collect::<Vec<_>>(),
        )
        .await?;
        sqlx::query("INSERT INTO lexicon_next.equivalent_taxonomy_refs(entry_id,revision,taxonomy_id) SELECT entry_id,$3,taxonomy_id FROM lexicon_next.equivalent_taxonomy_refs WHERE entry_id=$1 AND revision=$2")
            .bind(id).bind(peer.revision).bind(next).execute(&mut *conn).await.map_err(AppError::internal)?;
        sqlx::query(
            "UPDATE lexicon_next.entries SET revision=$2, updated_at=clock_timestamp() WHERE id=$1",
        )
        .bind(id)
        .bind(next)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    }
    Ok(own.into_values().collect())
}
