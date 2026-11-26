use std::collections::BTreeSet;

use axum::http::HeaderMap;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    generation::{NextAgentRelation, NextAgentRelationTarget, NextAgentSource},
    model::{NextDraft, strip_legacy_creation_detection},
};
use crate::{
    error::AppError,
    lexicon::normalization::{NormalizedHeadword, normalize_text_key},
};

struct PublishedSense {
    target: NextAgentRelationTarget,
    word: String,
    pos: String,
    glosses: Vec<String>,
}

async fn published_senses(
    pool: &PgPool,
    source: Uuid,
    medium: &str,
    words: &[String],
    prefix: &str,
    related: &[Uuid],
    limit: Option<i64>,
) -> Result<Vec<PublishedSense>, AppError> {
    // Read the effective publication for its meaning, and check the live sense
    // identity so that a generated link remains writable in the draft.
    let rows: Vec<(Uuid, Value, Vec<Uuid>)> = sqlx::query_as(
        "SELECT e.id, p.draft, ARRAY(
            SELECT s.id FROM lexicon_next.nodes n JOIN lexicon_next.senses s ON s.id=n.id
            WHERE n.entry_id=e.id
         )
         FROM lexicon_next.entries e
         JOIN LATERAL (
            SELECT snapshot->'draft' AS draft FROM lexicon_next.publications
            WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1
         ) p ON true
         WHERE e.status='published' AND e.target_language='en' AND e.medium_locale=$2
           AND e.id<>$1 AND (
             e.id=ANY($5) OR lexicon.question_candidate_key_v1(p.draft->>'headword')=ANY($3)
             OR ($4<>'' AND starts_with(lexicon.question_candidate_key_v1(p.draft->>'headword'),$4))
           )
         ORDER BY e.id=ANY($5) DESC, (lexicon.question_candidate_key_v1(p.draft->>'headword')=ANY($3)) DESC, e.id
         LIMIT $6",
    )
    .bind(source)
    .bind(medium)
    .bind(words)
    .bind(prefix)
    .bind(related)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::internal)?;

    let mut result = Vec::new();
    for (entry_id, mut document, live_senses) in rows {
        strip_legacy_creation_detection(&mut document);
        let draft: NextDraft = serde_json::from_value(document).map_err(AppError::internal)?;
        for sense in draft.senses {
            let Some(pos) = sense.part_of_speech else {
                continue;
            };
            if !live_senses.contains(&sense.id) {
                continue;
            }
            let glosses: Vec<_> = sense
                .equivalents
                .iter()
                .filter_map(|row| (row.locale == medium).then_some(row.text.as_str()))
                .chain(
                    sense
                        .definitions
                        .iter()
                        .filter_map(|row| (row.locale == medium).then_some(row.text.as_str())),
                )
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .collect();
            if !glosses.is_empty() {
                result.push(PublishedSense {
                    target: NextAgentRelationTarget {
                        entry_id,
                        sense_id: sense.id,
                    },
                    word: draft.headword.clone(),
                    pos,
                    glosses,
                });
            }
        }
    }
    Ok(result)
}

pub(super) async fn materials(
    pool: &PgPool,
    source: Uuid,
    headword: &str,
    medium: &str,
) -> Result<Option<NextAgentSource>, AppError> {
    let Ok(headword) = NormalizedHeadword::parse(headword) else {
        return Ok(None);
    };
    let stem = headword.key.strip_suffix('e').unwrap_or(&headword.key);
    let prefix = if stem.len() >= 3 && stem.bytes().all(|byte| byte.is_ascii_lowercase()) {
        stem
    } else {
        ""
    };
    // Dictionary links provide lexical search terms. Only published smart-lexicon
    // senses for those terms become internal candidates.
    let words: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT term
         FROM dictionary.entry_contents c JOIN dictionary.datasets d ON d.id=c.dataset_id
         CROSS JOIN LATERAL jsonb_array_elements(c.senses) s
         CROSS JOIN LATERAL unnest(
           dictionary.choice_words(s,'synonyms') ||
           dictionary.choice_words(s,'antonyms') ||
           dictionary.choice_words(s,'derived')
         ) term
         WHERE d.status='active' AND c.normalized_term=$1 AND term<>$1
         ORDER BY term LIMIT 80",
    )
    .bind(&headword.key)
    .fetch_all(pool)
    .await
    .map_err(AppError::internal)?;
    let related: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT (r->>'target_word_id')::uuid
         FROM (
           SELECT p.snapshot FROM lexicon_next.entries e
           JOIN lexicon_next.publications p ON p.entry_id=e.id
           WHERE e.id=$1 AND e.status='published'
           ORDER BY p.lifecycle_revision DESC LIMIT 1
         ) published
         CROSS JOIN LATERAL jsonb_array_elements(published.snapshot #> '{draft,senses}') s
         CROSS JOIN LATERAL jsonb_array_elements(s->'relations') r
         WHERE r->>'target_word_id' IS NOT NULL LIMIT 32",
    )
    .bind(source)
    .fetch_all(pool)
    .await
    .map_err(AppError::internal)?;
    let senses = published_senses(pool, source, medium, &words, prefix, &related, Some(24))
        .await?
        .into_iter()
        .filter(|sense| normalize_text_key(&sense.word) != headword.key);
    let mut excerpt = String::new();
    for sense in senses {
        let Some(gloss) = sense
            .glosses
            .iter()
            .find(|gloss| gloss.chars().count() <= 120)
        else {
            continue;
        };
        let line = json!({"word": sense.word, "pos": sense.pos, "gloss": gloss}).to_string();
        if excerpt.chars().count() + line.chars().count() < 1400 {
            excerpt.push_str(&line);
            excerpt.push('\n');
        }
    }
    Ok((!excerpt.is_empty()).then_some(NextAgentSource {
        source: "智能词库".into(),
        locator: "published-relation-senses".into(),
        excerpt,
        status: "found".into(),
    }))
}

fn gloss_parts(normalized: &str) -> Vec<&str> {
    // Match all listed glosses even when generation changes the separator or order.
    let mut parts: Vec<_> = normalized
        .split([',', ';', '、'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    parts.sort_unstable();
    parts
}

pub(super) async fn resolve(
    pool: &PgPool,
    source: Uuid,
    medium: &str,
    headers: &HeaderMap,
    rows: &mut [NextAgentRelation],
) -> Result<(), AppError> {
    for row in &mut *rows {
        row.target = None;
    }
    if rows.is_empty()
        || !headers
            .get("x-tsz-next-relation-targets")
            .is_some_and(|value| value == "v1")
    {
        return Ok(());
    }
    let words: BTreeSet<_> = rows
        .iter()
        .filter_map(|row| {
            NormalizedHeadword::parse(&row.word)
                .ok()
                .map(|word| word.key)
        })
        .collect();
    let senses = published_senses(
        pool,
        source,
        medium,
        &words.into_iter().collect::<Vec<_>>(),
        "",
        &[],
        None,
    )
    .await?;
    for row in &mut *rows {
        let word = normalize_text_key(&row.word);
        let gloss = normalize_text_key(&row.gloss);
        let parts = gloss_parts(&gloss);
        if parts.is_empty() {
            continue;
        }
        let mut matches = senses.iter().filter(|sense| {
            normalize_text_key(&sense.word) == word
                && sense
                    .glosses
                    .iter()
                    .any(|text| gloss_parts(&normalize_text_key(text)) == parts)
        });
        if let (Some(sense), None) = (matches.next(), matches.next()) {
            row.target = Some(sense.target);
        }
    }
    rows.sort_by_key(|row| row.target.is_none());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminRepository, AdminRole, NewAdmin};

    async fn publication(
        pool: &PgPool,
        owner: Uuid,
        headword: &str,
        medium: &str,
        status: &str,
        glosses: &[&str],
    ) -> NextAgentRelationTarget {
        let entry = Uuid::new_v4();
        let mut senses = Vec::new();
        sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,medium_locale,kind,headword,owner_id,revision,status,lifecycle_revision) VALUES($1,'en',$2,'word',$3,$4,1,$5,1)")
            .bind(entry).bind(medium).bind(headword).bind(owner).bind(status)
            .execute(pool).await.unwrap();
        for (index, gloss) in glosses.iter().enumerate() {
            let sense = Uuid::new_v4();
            sqlx::query("INSERT INTO lexicon_next.nodes(id,entry_id,kind) VALUES($1,$2,'sense')")
                .bind(sense)
                .bind(entry)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO lexicon_next.senses(id,part_of_speech,sort_order) VALUES($1,'noun',$2)")
                .bind(sense).bind(index as i32).execute(pool).await.unwrap();
            senses.push(
                json!({"id":sense,"part_of_speech":"noun","definitions":[],"equivalents":[
                    {"id":Uuid::new_v4(),"locale":medium,"text":gloss}
                ]}),
            );
        }
        let document = json!({"schema_version":1,"headword":headword,"forms":[],"senses":senses});
        sqlx::query("INSERT INTO lexicon_next.draft_revisions(entry_id,revision,actor_id,document) VALUES($1,1,$2,$3)")
            .bind(entry).bind(owner).bind(&document).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO lexicon_next.publications(id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot) VALUES($1,$2,1,1,$3,$4)")
            .bind(Uuid::new_v4()).bind(entry).bind(owner).bind(json!({"draft": document,"sentence_resources":[],"relation_targets":[]}))
            .execute(pool).await.unwrap();
        NextAgentRelationTarget {
            entry_id: entry,
            sense_id: serde_json::from_value(senses[0]["id"].clone()).unwrap(),
        }
    }

    #[sqlx::test]
    async fn only_current_unambiguous_internal_senses_link_and_rank_first(pool: PgPool) {
        let actor = Uuid::new_v4();
        AdminRepository::new(pool.clone())
            .create(NewAdmin {
                id: actor,
                phone: "relation-candidates".into(),
                display_name: "Relation candidates test".into(),
                password_hash: "test-only".into(),
                role: AdminRole::Admin,
                must_change_password: false,
                created_by_admin_id: None,
            })
            .await
            .unwrap();
        let chatter = publication(&pool, actor, "chatter", "zh-Hans", "published", &["闲聊"]).await;
        publication(&pool, actor, "chatter", "zh-Hant", "published", &["闲聊"]).await;
        publication(&pool, actor, "chatter", "zh-Hans", "archived", &["闲聊"]).await;
        publication(
            &pool,
            actor,
            "chattering",
            "zh-Hans",
            "published",
            &["健谈", "健谈"],
        )
        .await;
        let chatty = publication(
            &pool,
            actor,
            "chatty",
            "zh-Hans",
            "published",
            &["爱闲聊的，健谈的"],
        )
        .await;
        let incomplete_gloss = "词义".repeat(60);
        let long_gloss = format!("{incomplete_gloss}，仅限网络聊天室");
        publication(
            &pool,
            actor,
            "chatline",
            "zh-Hans",
            "published",
            &[&long_gloss],
        )
        .await;
        let source = Uuid::new_v4();
        let reference = materials(&pool, source, "chat", "zh-Hans")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reference.excerpt.matches("\"word\":\"chatter\"").count(), 1);
        assert!(!reference.excerpt.contains("chatline"));
        let proposal = |word: &str, gloss: &str| NextAgentRelation {
            sense: 0,
            kind: "derivative".into(),
            word: word.into(),
            gloss: gloss.into(),
            score: 75.0,
            target: None,
        };
        let mut rows = [
            proposal("chattering", "健谈"),
            proposal("chatter", "机械振动"),
            proposal("chatty", "健谈的；沉默的"),
            proposal("chatline", &incomplete_gloss),
            proposal("chatty", "健谈的；爱闲聊的"),
            proposal("chatter", "闲聊"),
        ];
        let mut headers = HeaderMap::new();
        resolve(&pool, source, "zh-Hans", &headers, &mut rows)
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.target.is_none()));
        headers.insert("x-tsz-next-relation-targets", "v1".parse().unwrap());
        resolve(&pool, source, "zh-Hans", &headers, &mut rows)
            .await
            .unwrap();
        assert_eq!(rows[0].target, Some(chatty));
        assert_eq!(rows[1].target, Some(chatter));
        assert!(rows[2..].iter().all(|r| r.target.is_none()));
        sqlx::query("DELETE FROM lexicon_next.senses WHERE id=$1")
            .bind(chatter.sense_id)
            .execute(&pool)
            .await
            .unwrap();
        resolve(&pool, source, "zh-Hans", &headers, &mut rows)
            .await
            .unwrap();
        assert_eq!(rows[0].target, Some(chatty));
        assert!(rows[1..].iter().all(|r| r.target.is_none()));
    }
}
