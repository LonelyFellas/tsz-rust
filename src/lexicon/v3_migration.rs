//! Deterministic V2 -> V3 current-aggregate migration and bounded rollback.
//!
//! The implementation lives outside the HTTP service so inventory and dry-run
//! stay operational commands. Historical publication snapshots are read only.

use std::collections::{BTreeSet, HashSet};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::lexicon::{
    dto::{
        CommonDialectV3, Dialect, DialectModeV3, DialectRulesV3, DraftFormsStepContent,
        DraftFormsStepContentV3, DraftMeaningsStepContentV3, EntryPresentationV3,
        LegacyHeadwordsCompatibilityV3, PronunciationStyle, SourceDialect, StepSaveIntent,
        TextOrigin, UkDialectV3, UsDialectV3, WordCommonFormVariantV3, WordConcreteFormV3,
        WordFormGroupMemberV3, WordFormGroupV3, WordFormTypeV3, WordFormVariantV2, WordPosFormsV3,
        WordPronunciationV3, WordRegionalVariantsV3, WordUkFormVariantV3, WordUsFormVariantV3,
    },
    v3_contract,
    v3_projection::{
        V3FormVariantSurfaceSource, form_variant_sources, presentation_from_legacy_bridge,
    },
};

pub const ROLLBACK_BLOCKED_V3_WRITE: &str = "v3_migration_rollback_v3_write_detected";
pub const ROLLBACK_BLOCKED_V3_PUBLICATION: &str = "v3_migration_rollback_v3_publication_detected";
pub const ROLLBACK_BLOCKED_SOURCE_CHANGED: &str =
    "v3_migration_rollback_source_publication_changed";
pub const MAX_MIGRATION_BATCH_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub struct MigrationInventoryReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub scanned_entries: usize,
    pub eligible_entries: usize,
    pub blocked_entries: usize,
    pub skipped_entries: usize,
    pub entries: Vec<MigrationEntryPreview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationDryRunReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub selection_digest: String,
    pub manifest_digest: String,
    pub scanned_entries: usize,
    pub eligible_entries: usize,
    pub blocked_entries: usize,
    pub entries: Vec<MigrationEntryPreview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationApprovalReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub manifest_digest: String,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationCanaryReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub entry_id: Uuid,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationApplyReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub selection_digest: String,
    pub scanned_entries: usize,
    pub eligible_entries: usize,
    pub applied_entries: usize,
    pub replayed_entries: usize,
    pub blocked_entries: usize,
    pub failed_entries: usize,
    pub entries: Vec<MigrationEntryResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationVerifyReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub checked_entries: usize,
    pub verified_entries: usize,
    pub ready: bool,
    pub entries: Vec<MigrationEntryResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationRollbackReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub batch_id: Uuid,
    pub rolled_back_entries: usize,
    pub entries: Vec<MigrationEntryResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationEntryPreview {
    pub entry_id: Uuid,
    pub source_revision: Option<i64>,
    pub source_current_publication_id: Option<Uuid>,
    pub eligible: bool,
    pub block_code: Option<String>,
    pub block_reason: Option<String>,
    pub expected_digest: Option<String>,
    pub counts: Option<MigrationNodeCounts>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationEntryResult {
    pub entry_id: Uuid,
    pub status: String,
    pub digest: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct MigrationNodeCounts {
    pub pos: usize,
    pub form_groups: usize,
    pub synthetic_groups: usize,
    pub concrete_forms: usize,
    pub memberships: usize,
    pub variants: usize,
    pub pronunciations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourcePosMode {
    pos_id: Uuid,
    spelling_mode: String,
    phonetic_mode: String,
    sort_order: i32,
}

#[derive(Debug, Clone)]
struct MigrationPlan {
    entry_id: Uuid,
    source_revision: i64,
    source_current_publication_id: Option<Uuid>,
    source_publications_digest: Vec<u8>,
    source_pos_modes: Vec<SourcePosMode>,
    source_forms: Value,
    source_meanings: Value,
    source_draft_surfaces: Value,
    target_forms: DraftFormsStepContentV3,
    target_forms_value: Value,
    target_meanings: Value,
    expected_digest: Vec<u8>,
    presentation: EntryPresentationV3,
    mappings: Vec<NodeMapping>,
    counts: MigrationNodeCounts,
}

#[derive(Debug, Clone, Serialize)]
struct NodeMapping {
    v2_node_id: Option<Uuid>,
    v3_node_id: Uuid,
    role: &'static str,
    mapping_kind: &'static str,
}

#[derive(Debug, FromRow)]
struct SourceEntryRow {
    id: Uuid,
    content_schema_version: i16,
    kind: String,
    revision: i64,
    headword_mode: Option<String>,
    source_dialect: Option<String>,
    current_publication_id: Option<Uuid>,
    common_headword: Option<String>,
    uk_headword: Option<String>,
    us_headword: Option<String>,
    forms: Value,
    meanings: Value,
}

#[derive(Debug, FromRow, Serialize)]
struct PublicationFingerprintRow {
    id: Uuid,
    publication_number: i32,
    source_revision: i64,
    content_schema_version: i16,
    snapshot: Value,
    snapshot_hash: Vec<u8>,
}

#[derive(Debug, FromRow)]
struct MigrationBackupRow {
    entry_id: Uuid,
    status: String,
    source_revision: i64,
    source_current_publication_id: Option<Uuid>,
    source_publications_digest: Vec<u8>,
    source_pos_modes: Value,
    source_forms: Value,
    source_meanings: Value,
    source_draft_surfaces: Value,
    expected_forms: Value,
    expected_presentation: Value,
    expected_digest: Vec<u8>,
    applied_digest: Option<Vec<u8>>,
}

#[derive(Debug)]
struct PlanBlocked {
    code: &'static str,
    message: String,
}

impl PlanBlocked {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
enum PreparedManifestEntry {
    Eligible(Box<MigrationPlan>),
    Blocked { entry_id: Uuid, issue: PlanBlocked },
}

fn manifest_digest(entries: &[PreparedManifestEntry]) -> anyhow::Result<Vec<u8>> {
    let values = entries
        .iter()
        .map(|entry| match entry {
            PreparedManifestEntry::Eligible(plan) => Ok(serde_json::json!({
                "entry_id": plan.entry_id,
                "status": "planned",
                "source_revision": plan.source_revision,
                "source_current_publication_id": plan.source_current_publication_id,
                "source_publications_digest": plan.source_publications_digest,
                "source_pos_modes": plan.source_pos_modes,
                "source_forms": plan.source_forms,
                "source_meanings": plan.source_meanings,
                "source_draft_surfaces": plan.source_draft_surfaces,
                "expected_forms": plan.target_forms_value,
                "expected_presentation": plan.presentation,
                "expected_digest": plan.expected_digest,
                "mappings": plan.mappings,
            })),
            PreparedManifestEntry::Blocked { entry_id, issue } => Ok(serde_json::json!({
                "entry_id": entry_id,
                "status": "blocked",
                "block_code": issue.code,
            })),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    digest_json(&serde_json::json!({
        "manifest_version": 1,
        "entries": values,
    }))
}

fn deterministic_uuid(parts: &[&str]) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(b"tsz.smart-lexicon-v3.migration.v1\0");
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn digest_json<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    Ok(Sha256::digest(serde_json::to_vec(value)?).to_vec())
}

fn hex_digest(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn normalize_v3_text(value: &str) -> String {
    value.nfkc().collect::<String>().trim().to_lowercase()
}

fn form_type(value: &str) -> Result<WordFormTypeV3, PlanBlocked> {
    match value {
        "base" => Ok(WordFormTypeV3::Base),
        "third_person_singular" => Ok(WordFormTypeV3::ThirdPersonSingular),
        "present_participle" => Ok(WordFormTypeV3::PresentParticiple),
        "past_tense" => Ok(WordFormTypeV3::PastTense),
        "past_participle" => Ok(WordFormTypeV3::PastParticiple),
        "plural" => Ok(WordFormTypeV3::Plural),
        "comparative" => Ok(WordFormTypeV3::Comparative),
        "superlative" => Ok(WordFormTypeV3::Superlative),
        _ => Err(PlanBlocked::new(
            "unsupported_v2_form_type",
            format!("unsupported V2 form_type {value}"),
        )),
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

const fn origin_name(value: TextOrigin) -> &'static str {
    match value {
        TextOrigin::Dictionary => "dictionary",
        TextOrigin::Converted => "converted",
        TextOrigin::Manual => "manual",
    }
}

const fn style_name(value: PronunciationStyle) -> &'static str {
    match value {
        PronunciationStyle::Normal => "normal",
        PronunciationStyle::Strong => "strong",
        PronunciationStyle::Weak => "weak",
    }
}

fn convert_pronunciations(
    pronunciations: &[crate::lexicon::dto::WordPronunciationV2],
    mappings: &mut Vec<NodeMapping>,
    counts: &mut MigrationNodeCounts,
) -> Vec<WordPronunciationV3> {
    pronunciations
        .iter()
        .map(|pronunciation| {
            mappings.push(NodeMapping {
                v2_node_id: Some(pronunciation.id),
                v3_node_id: pronunciation.id,
                role: "pronunciation",
                mapping_kind: "preserved",
            });
            counts.pronunciations += 1;
            WordPronunciationV3 {
                id: pronunciation.id,
                dict_phonetic: pronunciation.dict_phonetic.clone(),
                actual_pron: pronunciation.actual_pron.clone(),
                style: Some(pronunciation.style),
            }
        })
        .collect()
}

fn record_variant_mapping(
    variant: &WordFormVariantV2,
    mappings: &mut Vec<NodeMapping>,
    counts: &mut MigrationNodeCounts,
) {
    mappings.push(NodeMapping {
        v2_node_id: Some(variant.id),
        v3_node_id: variant.id,
        role: "form_variant",
        mapping_kind: "preserved",
    });
    counts.variants += 1;
}

fn convert_regional_variants(
    variants: &[WordFormVariantV2],
    expected_uk_us: bool,
    mappings: &mut Vec<NodeMapping>,
    counts: &mut MigrationNodeCounts,
) -> Result<WordRegionalVariantsV3, PlanBlocked> {
    let mut common = None;
    let mut uk = None;
    let mut us = None;
    let mut ids = HashSet::new();
    for variant in variants {
        if !ids.insert(variant.id) {
            return Err(PlanBlocked::new(
                "duplicate_v2_variant_id",
                format!("duplicate V2 variant UUID {}", variant.id),
            ));
        }
        match variant.dialect {
            Dialect::Common if common.replace(variant).is_some() => {
                return Err(PlanBlocked::new(
                    "duplicate_v2_variant_dialect",
                    "multiple common variants on one V2 form",
                ));
            }
            Dialect::Uk if uk.replace(variant).is_some() => {
                return Err(PlanBlocked::new(
                    "duplicate_v2_variant_dialect",
                    "multiple uk variants on one V2 form",
                ));
            }
            Dialect::Us if us.replace(variant).is_some() => {
                return Err(PlanBlocked::new(
                    "duplicate_v2_variant_dialect",
                    "multiple us variants on one V2 form",
                ));
            }
            _ => {}
        }
    }

    match (common, uk, us) {
        (Some(common), None, None) if !expected_uk_us => {
            record_variant_mapping(common, mappings, counts);
            Ok(WordRegionalVariantsV3::Common {
                common: WordCommonFormVariantV3 {
                    id: common.id,
                    dialect: CommonDialectV3::Common,
                    spelling: common.spelling.clone(),
                    origin: common.origin,
                    pronunciations: convert_pronunciations(
                        &common.pronunciations,
                        mappings,
                        counts,
                    ),
                },
            })
        }
        (None, Some(uk), Some(us)) if expected_uk_us => {
            record_variant_mapping(uk, mappings, counts);
            let uk_pronunciations = convert_pronunciations(&uk.pronunciations, mappings, counts);
            record_variant_mapping(us, mappings, counts);
            let us_pronunciations = convert_pronunciations(&us.pronunciations, mappings, counts);
            Ok(WordRegionalVariantsV3::UkUs {
                uk: WordUkFormVariantV3 {
                    id: uk.id,
                    dialect: UkDialectV3::Uk,
                    spelling: uk.spelling.clone(),
                    origin: uk.origin,
                    pronunciations: uk_pronunciations,
                },
                us: WordUsFormVariantV3 {
                    id: us.id,
                    dialect: UsDialectV3::Us,
                    spelling: us.spelling.clone(),
                    origin: us.origin,
                    pronunciations: us_pronunciations,
                },
            })
        }
        _ => Err(PlanBlocked::new(
            "invalid_v2_regional_shape",
            "V2 form variants must be common xor complete uk/us",
        )),
    }
}

fn expected_uk_us_variants(spelling_mode: &str, phonetic_mode: &str) -> Result<bool, PlanBlocked> {
    if !matches!(spelling_mode, "unified" | "distinguish")
        || !matches!(phonetic_mode, "unified" | "distinguish")
        || (spelling_mode == "distinguish" && phonetic_mode != "distinguish")
    {
        return Err(PlanBlocked::new(
            "invalid_v2_dialect_rules",
            "V2 spelling/phonetic dialect rules are invalid",
        ));
    }
    Ok(spelling_mode == "distinguish" || phonetic_mode == "distinguish")
}

fn dialect_rules_v3(
    spelling_mode: &str,
    phonetic_mode: &str,
) -> Result<DialectRulesV3, PlanBlocked> {
    expected_uk_us_variants(spelling_mode, phonetic_mode)?;
    let parse = |value| match value {
        "unified" => Ok(DialectModeV3::Unified),
        "distinguish" => Ok(DialectModeV3::Distinguish),
        _ => Err(PlanBlocked::new(
            "invalid_v2_dialect_rules",
            "V2 dialect mode is invalid",
        )),
    };
    Ok(DialectRulesV3 {
        spelling_mode: parse(spelling_mode)?,
        phonetic_mode: parse(phonetic_mode)?,
    })
}

fn convert_forms(
    entry_id: Uuid,
    source: &DraftFormsStepContent,
) -> Result<
    (
        DraftFormsStepContentV3,
        Vec<NodeMapping>,
        MigrationNodeCounts,
    ),
    PlanBlocked,
> {
    let mut mappings = vec![NodeMapping {
        v2_node_id: Some(entry_id),
        v3_node_id: entry_id,
        role: "entry",
        mapping_kind: "preserved",
    }];
    let mut counts = MigrationNodeCounts::default();
    let mut seen_node_ids = HashSet::new();
    seen_node_ids.insert(entry_id);
    let mut seen_pos_codes = HashSet::new();
    let mut target_pos = Vec::with_capacity(source.pos.len());

    for pos in &source.pos {
        let expected_uk_us = expected_uk_us_variants(
            &pos.dialect_rules.spelling_mode,
            &pos.dialect_rules.phonetic_mode,
        )?;
        if !seen_node_ids.insert(pos.pos_id) {
            return Err(PlanBlocked::new(
                "duplicate_v2_node_id",
                format!("duplicate V2 POS UUID {}", pos.pos_id),
            ));
        }
        if !seen_pos_codes.insert(pos.pos.clone()) {
            return Err(PlanBlocked::new(
                "duplicate_v2_pos",
                format!("duplicate V2 POS code {}", pos.pos),
            ));
        }
        mappings.push(NodeMapping {
            v2_node_id: Some(pos.pos_id),
            v3_node_id: pos.pos_id,
            role: "pos",
            mapping_kind: "preserved",
        });
        counts.pos += 1;

        if pos.base_form.form_type != "base" {
            return Err(PlanBlocked::new(
                "invalid_v2_base_form_type",
                format!(
                    "V2 base form {} has type {}",
                    pos.base_form.id, pos.base_form.form_type
                ),
            ));
        }
        if !seen_node_ids.insert(pos.base_form.id) {
            return Err(PlanBlocked::new(
                "duplicate_v2_node_id",
                format!("duplicate V2 base UUID {}", pos.base_form.id),
            ));
        }
        mappings.push(NodeMapping {
            v2_node_id: Some(pos.base_form.id),
            v3_node_id: pos.base_form.id,
            role: "concrete_form",
            mapping_kind: "preserved",
        });
        counts.concrete_forms += 1;
        let mut forms = vec![WordConcreteFormV3 {
            id: pos.base_form.id,
            form_type: WordFormTypeV3::Base,
            regional_variants: convert_regional_variants(
                &pos.base_form.variants,
                expected_uk_us,
                &mut mappings,
                &mut counts,
            )?,
        }];

        let group_specs = if pos.form_groups.is_empty() {
            let group_id = deterministic_uuid(&[
                "base-only-group",
                &entry_id.to_string(),
                &pos.pos_id.to_string(),
            ]);
            if !seen_node_ids.insert(group_id) {
                return Err(PlanBlocked::new(
                    "deterministic_uuid_collision",
                    format!("synthetic group UUID collision {group_id}"),
                ));
            }
            mappings.push(NodeMapping {
                v2_node_id: None,
                v3_node_id: group_id,
                role: "synthetic_base_only_group",
                mapping_kind: "deterministic_generated",
            });
            counts.form_groups += 1;
            counts.synthetic_groups += 1;
            vec![(group_id, false, Vec::new())]
        } else {
            let mut groups = Vec::with_capacity(pos.form_groups.len());
            for group in &pos.form_groups {
                if !seen_node_ids.insert(group.id) {
                    return Err(PlanBlocked::new(
                        "duplicate_v2_node_id",
                        format!("duplicate V2 group UUID {}", group.id),
                    ));
                }
                mappings.push(NodeMapping {
                    v2_node_id: Some(group.id),
                    v3_node_id: group.id,
                    role: "form_group",
                    mapping_kind: "preserved",
                });
                counts.form_groups += 1;
                let mut slot_ids = Vec::with_capacity(group.slots.len());
                for slot in &group.slots {
                    if !seen_node_ids.insert(slot.id) {
                        return Err(PlanBlocked::new(
                            "duplicate_v2_node_id",
                            format!("duplicate V2 slot UUID {}", slot.id),
                        ));
                    }
                    mappings.push(NodeMapping {
                        v2_node_id: Some(slot.id),
                        v3_node_id: slot.id,
                        role: "concrete_form",
                        mapping_kind: "preserved",
                    });
                    counts.concrete_forms += 1;
                    forms.push(WordConcreteFormV3 {
                        id: slot.id,
                        form_type: form_type(&slot.form_type)?,
                        regional_variants: convert_regional_variants(
                            &slot.variants,
                            expected_uk_us,
                            &mut mappings,
                            &mut counts,
                        )?,
                    });
                    slot_ids.push(slot.id);
                }
                groups.push((group.id, group.is_regular, slot_ids));
            }
            groups
        };

        let mut form_groups = Vec::with_capacity(group_specs.len());
        for (group_id, is_regular, slot_ids) in group_specs {
            let mut member_form_ids = Vec::with_capacity(slot_ids.len() + 1);
            member_form_ids.push(pos.base_form.id);
            member_form_ids.extend(slot_ids);
            let mut members = Vec::with_capacity(member_form_ids.len());
            for form_id in member_form_ids {
                let membership_id = deterministic_uuid(&[
                    "membership",
                    &entry_id.to_string(),
                    &pos.pos_id.to_string(),
                    &group_id.to_string(),
                    &form_id.to_string(),
                ]);
                if !seen_node_ids.insert(membership_id) {
                    return Err(PlanBlocked::new(
                        "deterministic_uuid_collision",
                        format!("membership UUID collision {membership_id}"),
                    ));
                }
                mappings.push(NodeMapping {
                    v2_node_id: Some(form_id),
                    v3_node_id: membership_id,
                    role: "group_membership",
                    mapping_kind: "deterministic_generated",
                });
                counts.memberships += 1;
                members.push(WordFormGroupMemberV3 {
                    id: membership_id,
                    form_id,
                });
            }
            form_groups.push(WordFormGroupV3 {
                id: group_id,
                is_regular,
                members,
            });
        }

        target_pos.push(WordPosFormsV3 {
            pos_id: pos.pos_id,
            pos: pos.pos.clone(),
            dialect_rules: dialect_rules_v3(
                &pos.dialect_rules.spelling_mode,
                &pos.dialect_rules.phonetic_mode,
            )?,
            forms,
            form_groups,
        });
    }

    Ok((
        DraftFormsStepContentV3 { pos: target_pos },
        mappings,
        counts,
    ))
}

async fn source_entry(pool: &PgPool, entry_id: Uuid) -> anyhow::Result<Option<SourceEntryRow>> {
    sqlx::query_as::<_, SourceEntryRow>(
        r#"
        SELECT entry.id,
               entry.content_schema_version,
               entry.kind,
               entry.revision,
               entry.headword_mode,
               entry.source_dialect,
               entry.current_publication_id,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = entry.id AND dialect = 'common') AS common_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = entry.id AND dialect = 'uk') AS uk_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = entry.id AND dialect = 'us') AS us_headword,
               projection.forms,
               projection.meanings
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        WHERE entry.id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn publication_fingerprint(
    pool: &PgPool,
    entry_id: Uuid,
) -> anyhow::Result<(Vec<u8>, Vec<PublicationFingerprintRow>)> {
    let rows = sqlx::query_as::<_, PublicationFingerprintRow>(
        r#"
        SELECT id, publication_number, source_revision, content_schema_version,
               snapshot, snapshot_hash
        FROM lexicon.entry_publications
        WHERE entry_id = $1
        ORDER BY publication_number, id
        "#,
    )
    .bind(entry_id)
    .fetch_all(pool)
    .await?;
    Ok((digest_json(&rows)?, rows))
}

async fn source_pos_modes(pool: &PgPool, entry_id: Uuid) -> anyhow::Result<Vec<SourcePosMode>> {
    #[derive(FromRow)]
    struct Row {
        pos_id: Uuid,
        spelling_mode: Option<String>,
        phonetic_mode: Option<String>,
        sort_order: i32,
        content_schema_version: i16,
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"
        SELECT id AS pos_id, spelling_mode, phonetic_mode, sort_order,
               content_schema_version
        FROM lexicon.entry_pos
        WHERE entry_id = $1
        ORDER BY sort_order, id
        "#,
    )
    .bind(entry_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            ensure!(
                row.content_schema_version == 2,
                "entry {entry_id} has non-V2 POS {}",
                row.pos_id
            );
            Ok(SourcePosMode {
                pos_id: row.pos_id,
                spelling_mode: row.spelling_mode.context("V2 POS spelling_mode is null")?,
                phonetic_mode: row.phonetic_mode.context("V2 POS phonetic_mode is null")?,
                sort_order: row.sort_order,
            })
        })
        .collect()
}

fn validate_source_pos_modes(
    source: &DraftFormsStepContent,
    modes: &[SourcePosMode],
) -> Result<(), PlanBlocked> {
    if source.pos.len() != modes.len() {
        return Err(PlanBlocked::new(
            "v2_projection_relational_mismatch",
            "V2 POS count differs between editor projection and relational rows",
        ));
    }
    for (ordinal, pos) in source.pos.iter().enumerate() {
        let Some(mode) = modes.iter().find(|mode| mode.pos_id == pos.pos_id) else {
            return Err(PlanBlocked::new(
                "v2_projection_relational_mismatch",
                format!("V2 POS {} has no relational row", pos.pos_id),
            ));
        };
        if mode.spelling_mode != pos.dialect_rules.spelling_mode
            || mode.phonetic_mode != pos.dialect_rules.phonetic_mode
            || mode.sort_order != ordinal as i32
        {
            return Err(PlanBlocked::new(
                "v2_projection_relational_mismatch",
                format!(
                    "V2 POS {} modes/order differ from relational rows",
                    pos.pos_id
                ),
            ));
        }
    }
    Ok(())
}

async fn source_draft_surfaces(pool: &PgPool, entry_id: Uuid) -> anyhow::Result<Value> {
    let rows = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT to_jsonb(source)
        FROM lexicon.surface_sources source
        WHERE source.entry_id = $1
          AND source.content_schema_version = 2
          AND source.content_scope = 'draft'
          AND source.is_deleted = FALSE
        ORDER BY source.source_id, source.dialect_scope,
                 source.normalization_version
        "#,
    )
    .bind(entry_id)
    .fetch_all(pool)
    .await?;
    Ok(Value::Array(rows))
}

fn bridge_from_source(row: &SourceEntryRow) -> Result<LegacyHeadwordsCompatibilityV3, PlanBlocked> {
    match row.headword_mode.as_deref() {
        Some("unified") => Ok(LegacyHeadwordsCompatibilityV3::Unified {
            common: row.common_headword.clone().ok_or_else(|| {
                PlanBlocked::new(
                    "invalid_v2_headwords",
                    "unified V2 entry has no common headword",
                )
            })?,
        }),
        Some("distinguish") => Ok(LegacyHeadwordsCompatibilityV3::Distinguish {
            uk: row.uk_headword.clone().ok_or_else(|| {
                PlanBlocked::new(
                    "invalid_v2_headwords",
                    "distinguish V2 entry has no UK headword",
                )
            })?,
            us: row.us_headword.clone().ok_or_else(|| {
                PlanBlocked::new(
                    "invalid_v2_headwords",
                    "distinguish V2 entry has no US headword",
                )
            })?,
            source_dialect: match row.source_dialect.as_deref() {
                Some("uk") => SourceDialect::Uk,
                Some("us") => SourceDialect::Us,
                _ => {
                    return Err(PlanBlocked::new(
                        "invalid_v2_headwords",
                        "distinguish V2 entry has invalid source_dialect",
                    ));
                }
            },
        }),
        _ => Err(PlanBlocked::new(
            "invalid_v2_headwords",
            "V2 entry has invalid headword_mode",
        )),
    }
}

async fn actual_ids(
    pool: &PgPool,
    query: &'static str,
    entry_id: Uuid,
) -> anyhow::Result<BTreeSet<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(query)
        .bind(entry_id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect())
}

async fn validate_relational_source(
    pool: &PgPool,
    plan: &MigrationPlan,
) -> anyhow::Result<Result<(), PlanBlocked>> {
    let expected_pos = plan
        .target_forms
        .pos
        .iter()
        .map(|pos| pos.pos_id)
        .collect::<BTreeSet<_>>();
    let expected_groups = plan
        .mappings
        .iter()
        .filter(|mapping| mapping.role == "form_group")
        .map(|mapping| mapping.v3_node_id)
        .collect::<BTreeSet<_>>();
    let expected_forms = plan
        .mappings
        .iter()
        .filter(|mapping| mapping.role == "concrete_form")
        .map(|mapping| mapping.v3_node_id)
        .collect::<BTreeSet<_>>();
    let expected_variants = plan
        .mappings
        .iter()
        .filter(|mapping| mapping.role == "form_variant")
        .map(|mapping| mapping.v3_node_id)
        .collect::<BTreeSet<_>>();
    let expected_pronunciations = plan
        .mappings
        .iter()
        .filter(|mapping| mapping.role == "pronunciation")
        .map(|mapping| mapping.v3_node_id)
        .collect::<BTreeSet<_>>();
    let checks = [
        (
            "pos",
            expected_pos,
            actual_ids(
                pool,
                "SELECT id FROM lexicon.entry_pos WHERE entry_id = $1",
                plan.entry_id,
            )
            .await?,
        ),
        (
            "form_group",
            expected_groups,
            actual_ids(
                pool,
                "SELECT id FROM lexicon.form_groups WHERE entry_id = $1",
                plan.entry_id,
            )
            .await?,
        ),
        (
            "form_slot",
            expected_forms,
            actual_ids(
                pool,
                "SELECT id FROM lexicon.form_slots WHERE entry_id = $1",
                plan.entry_id,
            )
            .await?,
        ),
        (
            "form_variant",
            expected_variants,
            actual_ids(
                pool,
                "SELECT id FROM lexicon.form_variants WHERE entry_id = $1",
                plan.entry_id,
            )
            .await?,
        ),
        (
            "pronunciation",
            expected_pronunciations,
            actual_ids(
                pool,
                "SELECT id FROM lexicon.pronunciations WHERE entry_id = $1",
                plan.entry_id,
            )
            .await?,
        ),
    ];
    for (role, expected, actual) in checks {
        if expected != actual {
            return Ok(Err(PlanBlocked::new(
                "v2_projection_relational_mismatch",
                format!("V2 {role} UUIDs differ between editor projection and relational rows"),
            )));
        }
    }
    Ok(Ok(()))
}

async fn load_plan(
    pool: &PgPool,
    entry_id: Uuid,
) -> anyhow::Result<Result<MigrationPlan, PlanBlocked>> {
    let Some(row) = source_entry(pool, entry_id).await? else {
        return Ok(Err(PlanBlocked::new(
            "entry_not_found",
            format!("entry {entry_id} was not found"),
        )));
    };
    if row.content_schema_version != 2 {
        return Ok(Err(PlanBlocked::new(
            "source_schema_not_v2",
            format!(
                "entry {entry_id} has schema version {}",
                row.content_schema_version
            ),
        )));
    }
    if row.kind != "word" {
        return Ok(Err(PlanBlocked::new(
            "phrase_out_of_scope",
            "Phase 1 migrates word entries only",
        )));
    }
    let source_current_publication_id = row.current_publication_id;
    let source_forms: DraftFormsStepContent = match serde_json::from_value(row.forms.clone()) {
        Ok(forms) => forms,
        Err(error) => {
            return Ok(Err(PlanBlocked::new(
                "invalid_v2_forms_projection",
                error.to_string(),
            )));
        }
    };
    let target_meanings: DraftMeaningsStepContentV3 =
        match serde_json::from_value(row.meanings.clone()) {
            Ok(meanings) => meanings,
            Err(error) => {
                return Ok(Err(PlanBlocked::new(
                    "invalid_v2_meanings_projection",
                    error.to_string(),
                )));
            }
        };
    let (target_forms, mappings, counts) = match convert_forms(entry_id, &source_forms) {
        Ok(converted) => converted,
        Err(blocked) => return Ok(Err(blocked)),
    };
    let mut target_issues = v3_contract::validate_forms(&target_forms, StepSaveIntent::Save);
    target_issues.extend(v3_contract::validate_meanings(&target_meanings));
    target_issues.extend(v3_contract::validate_aggregate_node_limit(
        &target_forms,
        &target_meanings,
    ));
    if !target_issues.is_empty() {
        let summary = target_issues
            .iter()
            .map(|issue| format!("{}:{}:{}", issue.code, issue.node_id, issue.field))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(Err(PlanBlocked::new(
            "v3_target_contract_invalid",
            format!("converted V3 target violates its writable contract: {summary}"),
        )));
    }
    let target_forms_value = serde_json::to_value(&target_forms)?;
    let target_meanings = serde_json::to_value(target_meanings)?;
    let expected_digest = digest_json(&serde_json::json!({
        "schema_version": 3,
        "forms": target_forms_value,
        "meanings": target_meanings,
    }))?;
    let bridge = match bridge_from_source(&row) {
        Ok(bridge) => bridge,
        Err(blocked) => return Ok(Err(blocked)),
    };
    let presentation = presentation_from_legacy_bridge(entry_id, &bridge);
    let (source_publications_digest, publications) =
        publication_fingerprint(pool, entry_id).await?;
    if source_current_publication_id.is_some_and(|current_id| {
        publications
            .iter()
            .all(|publication| publication.id != current_id)
    }) {
        return Ok(Err(PlanBlocked::new(
            "current_publication_missing",
            "entry current_publication_id does not identify a publication row",
        )));
    }
    if publications
        .iter()
        .any(|publication| publication.content_schema_version != 2)
    {
        return Ok(Err(PlanBlocked::new(
            "non_v2_publication_before_migration",
            "source entry already has a non-V2 publication",
        )));
    }
    let source_pos_modes = match source_pos_modes(pool, entry_id).await {
        Ok(modes) => modes,
        Err(error) => {
            return Ok(Err(PlanBlocked::new(
                "v2_projection_relational_mismatch",
                error.to_string(),
            )));
        }
    };
    if let Err(blocked) = validate_source_pos_modes(&source_forms, &source_pos_modes) {
        return Ok(Err(blocked));
    }
    let source_draft_surfaces = source_draft_surfaces(pool, entry_id).await?;
    let plan = MigrationPlan {
        entry_id: row.id,
        source_revision: row.revision,
        source_current_publication_id,
        source_publications_digest,
        source_pos_modes,
        source_forms: row.forms,
        source_meanings: row.meanings,
        source_draft_surfaces,
        target_forms,
        target_forms_value,
        target_meanings,
        expected_digest,
        presentation,
        mappings,
        counts,
    };
    if let Err(blocked) = validate_relational_source(pool, &plan).await? {
        return Ok(Err(blocked));
    }
    Ok(Ok(plan))
}

fn plan_preview(plan: &MigrationPlan) -> MigrationEntryPreview {
    MigrationEntryPreview {
        entry_id: plan.entry_id,
        source_revision: Some(plan.source_revision),
        source_current_publication_id: plan.source_current_publication_id,
        eligible: true,
        block_code: None,
        block_reason: None,
        expected_digest: Some(hex_digest(&plan.expected_digest)),
        counts: Some(plan.counts),
    }
}

fn blocked_preview(entry_id: Uuid, blocked: &PlanBlocked) -> MigrationEntryPreview {
    MigrationEntryPreview {
        entry_id,
        source_revision: None,
        source_current_publication_id: None,
        eligible: false,
        block_code: Some(blocked.code.to_owned()),
        block_reason: Some(blocked.message.clone()),
        expected_digest: None,
        counts: None,
    }
}

async fn all_entry_ids(pool: &PgPool) -> anyhow::Result<Vec<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM lexicon.entries ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

fn selection_digest(entry_ids: &[Uuid]) -> anyhow::Result<Vec<u8>> {
    let mut sorted = entry_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    digest_json(&sorted)
}

pub async fn inventory(pool: &PgPool) -> anyhow::Result<MigrationInventoryReport> {
    let entry_ids = all_entry_ids(pool).await?;
    let mut entries = Vec::with_capacity(entry_ids.len());
    let mut eligible_entries = 0;
    let mut blocked_entries = 0;
    let mut skipped_entries = 0;
    for entry_id in &entry_ids {
        match load_plan(pool, *entry_id).await? {
            Ok(plan) => {
                eligible_entries += 1;
                entries.push(plan_preview(&plan));
            }
            Err(blocked) => {
                if matches!(blocked.code, "phrase_out_of_scope" | "source_schema_not_v2") {
                    skipped_entries += 1;
                } else {
                    blocked_entries += 1;
                }
                entries.push(blocked_preview(*entry_id, &blocked));
            }
        }
    }
    Ok(MigrationInventoryReport {
        schema_version: 1,
        mode: "inventory",
        scanned_entries: entry_ids.len(),
        eligible_entries,
        blocked_entries,
        skipped_entries,
        entries,
    })
}

pub async fn dry_run(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    selected_entry_ids: &[Uuid],
) -> anyhow::Result<MigrationDryRunReport> {
    ensure!(
        !selected_entry_ids.is_empty(),
        "migration_entry_ids_required: select an explicit bounded batch"
    );
    let mut entry_ids = selected_entry_ids.to_vec();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    ensure!(
        entry_ids.len() <= MAX_MIGRATION_BATCH_ENTRIES,
        "migration_batch_too_large: maximum {MAX_MIGRATION_BATCH_ENTRIES} entries"
    );
    let selection = selection_digest(&entry_ids)?;
    let mut prepared = Vec::with_capacity(entry_ids.len());
    let mut eligible_entries = 0;
    let mut blocked_entries = 0;
    for entry_id in &entry_ids {
        match load_plan(pool, *entry_id).await? {
            Ok(plan) => {
                eligible_entries += 1;
                prepared.push(PreparedManifestEntry::Eligible(Box::new(plan)));
            }
            Err(blocked) => {
                ensure!(
                    blocked.code != "entry_not_found",
                    "migration_entry_not_found: {entry_id}"
                );
                blocked_entries += 1;
                prepared.push(PreparedManifestEntry::Blocked {
                    entry_id: *entry_id,
                    issue: blocked,
                });
            }
        }
    }
    let manifest = manifest_digest(&prepared)?;
    persist_dry_run_manifest(
        pool, batch_id, actor_id, request_id, &selection, &manifest, &prepared,
    )
    .await?;
    let entries = prepared
        .iter()
        .map(|entry| match entry {
            PreparedManifestEntry::Eligible(plan) => plan_preview(plan),
            PreparedManifestEntry::Blocked { entry_id, issue } => blocked_preview(*entry_id, issue),
        })
        .collect();
    Ok(MigrationDryRunReport {
        schema_version: 1,
        mode: "dry_run",
        batch_id,
        selection_digest: hex_digest(&selection),
        manifest_digest: hex_digest(&manifest),
        scanned_entries: entry_ids.len(),
        eligible_entries,
        blocked_entries,
        entries,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_dry_run_manifest(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    selection_digest: &[u8],
    manifest_digest: &[u8],
    entries: &[PreparedManifestEntry],
) -> anyhow::Result<()> {
    #[derive(FromRow)]
    struct ExistingBatch {
        selection_digest: Vec<u8>,
        manifest_digest: Vec<u8>,
        requested_by_admin_id: Uuid,
        request_id: Uuid,
        status: String,
        scanned_count: i32,
        eligible_count: i32,
        blocked_count: i32,
    }

    let eligible_count = entries
        .iter()
        .filter(|entry| matches!(entry, PreparedManifestEntry::Eligible(_)))
        .count() as i32;
    let blocked_count = entries.len() as i32 - eligible_count;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.plan:{batch_id}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_migration_batches (
            id, status, selection_digest, manifest_digest,
            requested_by_admin_id, request_id,
            scanned_count, eligible_count, blocked_count
        ) VALUES ($1, 'planned', $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(batch_id)
    .bind(selection_digest)
    .bind(manifest_digest)
    .bind(actor_id)
    .bind(request_id)
    .bind(entries.len() as i32)
    .bind(eligible_count)
    .bind(blocked_count)
    .execute(&mut *tx)
    .await?;
    let existing = sqlx::query_as::<_, ExistingBatch>(
        r#"
        SELECT selection_digest, manifest_digest, requested_by_admin_id,
               request_id, status, scanned_count, eligible_count, blocked_count
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        existing.selection_digest == selection_digest
            && existing.manifest_digest == manifest_digest
            && existing.requested_by_admin_id == actor_id
            && existing.request_id == request_id
            && existing.scanned_count == entries.len() as i32
            && existing.eligible_count == eligible_count
            && existing.blocked_count == blocked_count,
        "migration_batch_id_conflict: batch {batch_id} was reused with a different manifest"
    );
    ensure!(
        !matches!(existing.status.as_str(), "failed" | "rolled_back"),
        "migration_batch_not_plannable: batch {batch_id} is {}",
        existing.status
    );

    for entry in entries {
        match entry {
            PreparedManifestEntry::Eligible(plan) => {
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_migration_entries (
                        batch_id, entry_id, status, source_revision,
                        source_current_publication_id, source_publications_digest,
                        source_pos_modes, source_forms, source_meanings,
                        source_draft_surfaces, expected_forms,
                        expected_presentation, expected_digest
                    ) VALUES (
                        $1, $2, 'planned', $3, $4, $5, $6, $7, $8,
                        $9, $10, $11, $12
                    )
                    ON CONFLICT (batch_id, entry_id) DO NOTHING
                    "#,
                )
                .bind(batch_id)
                .bind(plan.entry_id)
                .bind(plan.source_revision)
                .bind(plan.source_current_publication_id)
                .bind(&plan.source_publications_digest)
                .bind(serde_json::to_value(&plan.source_pos_modes)?)
                .bind(&plan.source_forms)
                .bind(&plan.source_meanings)
                .bind(&plan.source_draft_surfaces)
                .bind(&plan.target_forms_value)
                .bind(serde_json::to_value(&plan.presentation)?)
                .bind(&plan.expected_digest)
                .execute(&mut *tx)
                .await?;
                insert_migration_map(&mut tx, batch_id, plan).await?;
            }
            PreparedManifestEntry::Blocked { entry_id, issue } => {
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_migration_entries (
                        batch_id, entry_id, status, block_code
                    ) VALUES ($1, $2, 'blocked', $3)
                    ON CONFLICT (batch_id, entry_id) DO NOTHING
                    "#,
                )
                .bind(batch_id)
                .bind(entry_id)
                .bind(issue.code)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn approve(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    expected_manifest_digest: &str,
) -> anyhow::Result<MigrationApprovalReport> {
    #[derive(FromRow)]
    struct Batch {
        status: String,
        manifest_digest: Vec<u8>,
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.plan:{batch_id}"))
        .execute(&mut *tx)
        .await?;
    let batch = sqlx::query_as::<_, Batch>(
        r#"
        SELECT status, manifest_digest
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("migration_dry_run_required: batch {batch_id}"))?;
    ensure!(
        hex_digest(&batch.manifest_digest) == expected_manifest_digest,
        "migration_manifest_digest_mismatch: batch {batch_id}"
    );
    let replayed = match batch.status.as_str() {
        "planned" => {
            sqlx::query(
                r#"
                UPDATE lexicon.v3_migration_batches
                SET status = 'approved', approved_by_admin_id = $2,
                    approval_request_id = $3, approved_at = now()
                WHERE id = $1 AND status = 'planned'
                "#,
            )
            .bind(batch_id)
            .bind(actor_id)
            .bind(request_id)
            .execute(&mut *tx)
            .await?;
            false
        }
        "approved" | "applying" | "applied" | "verified" => true,
        status => anyhow::bail!("migration_batch_not_approvable: batch {batch_id} is {status}"),
    };
    tx.commit().await?;
    Ok(MigrationApprovalReport {
        schema_version: 1,
        mode: "approve",
        batch_id,
        manifest_digest: hex_digest(&batch.manifest_digest),
        replayed,
    })
}

async fn publication_fingerprint_tx(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> anyhow::Result<Vec<u8>> {
    let rows = sqlx::query_as::<_, PublicationFingerprintRow>(
        r#"
        SELECT id, publication_number, source_revision, content_schema_version,
               snapshot, snapshot_hash
        FROM lexicon.entry_publications
        WHERE entry_id = $1
        ORDER BY publication_number, id
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await?;
    digest_json(&rows)
}

async fn source_draft_surfaces_tx(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> anyhow::Result<Value> {
    let rows = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT to_jsonb(source)
        FROM lexicon.surface_sources source
        WHERE source.entry_id = $1
          AND source.content_schema_version = 2
          AND source.content_scope = 'draft'
          AND source.is_deleted = FALSE
        ORDER BY source.source_id, source.dialect_scope,
                 source.normalization_version
        FOR UPDATE
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(Value::Array(rows))
}

async fn lock_and_recheck_source(
    tx: &mut Transaction<'_, Postgres>,
    plan: &MigrationPlan,
) -> anyhow::Result<i16> {
    #[derive(FromRow)]
    struct Locked {
        content_schema_version: i16,
        revision: i64,
        current_publication_id: Option<Uuid>,
        forms: Value,
        meanings: Value,
    }
    // apply() holds the batch's entry advisory locks for the full command.
    // Reacquiring one here from this separate checkpoint transaction would
    // self-deadlock against that control transaction.
    let locked = sqlx::query_as::<_, Locked>(
        r#"
        SELECT entry.content_schema_version, entry.revision,
               entry.current_publication_id, projection.forms, projection.meanings
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        WHERE entry.id = $1
        FOR UPDATE OF entry, projection
        "#,
    )
    .bind(plan.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    if locked.content_schema_version == 3 {
        return Ok(3);
    }
    ensure!(
        locked.content_schema_version == 2,
        "source_schema_changed: entry {} is schema {}",
        plan.entry_id,
        locked.content_schema_version
    );
    ensure!(
        locked.revision == plan.source_revision
            && locked.current_publication_id == plan.source_current_publication_id
            && locked.forms == plan.source_forms
            && locked.meanings == plan.source_meanings,
        "source_changed_since_dry_run: entry {}",
        plan.entry_id
    );
    ensure!(
        publication_fingerprint_tx(tx, plan.entry_id).await? == plan.source_publications_digest,
        "source_publications_changed_since_dry_run: entry {}",
        plan.entry_id
    );
    ensure!(
        source_draft_surfaces_tx(tx, plan.entry_id).await? == plan.source_draft_surfaces,
        "source_draft_surfaces_changed_since_dry_run: entry {}",
        plan.entry_id
    );
    Ok(2)
}

async fn convert_existing_node(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    node_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
    stable_slot: bool,
) -> anyhow::Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE lexicon.nodes
        SET node_type = $3, parent_node_id = $4, node_role = $5,
            stable_slot = $6, removed_from_draft_at = NULL
        WHERE id = $1 AND entry_id = $2
        "#,
    )
    .bind(node_id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await?;
    ensure!(
        result.rows_affected() == 1,
        "missing_v2_node: entry {entry_id} node {node_id}"
    );
    Ok(())
}

async fn insert_generated_node(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    node_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, $3, $4, $5, FALSE)
        "#,
    )
    .bind(node_id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_variant(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    form_id: Uuid,
    dialect: &str,
    variant_id: Uuid,
    spelling: &str,
    origin: TextOrigin,
    pronunciations: &[WordPronunciationV3],
) -> anyhow::Result<()> {
    convert_existing_node(
        tx,
        entry_id,
        variant_id,
        "form_variant",
        Some(form_id),
        &format!("forms.form_variant:{dialect}"),
        true,
    )
    .await?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_form_variants (
            id, entry_id, form_id, dialect, spelling, normalized_spelling,
            normalization_version, origin
        ) VALUES ($1, $2, $3, $4, $5, $6, 1, $7)
        "#,
    )
    .bind(variant_id)
    .bind(entry_id)
    .bind(form_id)
    .bind(dialect)
    .bind(spelling)
    .bind(
        crate::lexicon::normalization::normalize_headword(spelling)
            .context("validated V2 form spelling could not use normalization v1")?
            .key,
    )
    .bind(origin_name(origin))
    .execute(&mut **tx)
    .await?;
    for (ordinal, pronunciation) in pronunciations.iter().enumerate() {
        convert_existing_node(
            tx,
            entry_id,
            pronunciation.id,
            "pronunciation",
            Some(variant_id),
            "forms.pronunciation",
            false,
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_pronunciations (
                id, entry_id, form_variant_id, dict_phonetic, actual_pron,
                normalized_dict_phonetic, normalized_actual_pron, style,
                normalization_version, ordinal
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, $9)
            "#,
        )
        .bind(pronunciation.id)
        .bind(entry_id)
        .bind(variant_id)
        .bind(&pronunciation.dict_phonetic)
        .bind(&pronunciation.actual_pron)
        .bind(normalize_v3_text(&pronunciation.dict_phonetic))
        .bind(normalize_v3_text(&pronunciation.actual_pron))
        .bind(pronunciation.style.map(style_name))
        .bind(ordinal as i32)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn write_v3_forms(
    tx: &mut Transaction<'_, Postgres>,
    plan: &MigrationPlan,
) -> anyhow::Result<()> {
    for (pos_ordinal, pos) in plan.target_forms.pos.iter().enumerate() {
        convert_existing_node(
            tx,
            plan.entry_id,
            pos.pos_id,
            "pos",
            None,
            "forms.pos",
            false,
        )
        .await?;
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entry_pos
            SET content_schema_version = 3, spelling_mode = $3,
                phonetic_mode = $4, sort_order = $5
            WHERE id = $1 AND entry_id = $2 AND content_schema_version = 2
            "#,
        )
        .bind(pos.pos_id)
        .bind(plan.entry_id)
        .bind(pos.dialect_rules.spelling_mode.as_str())
        .bind(pos.dialect_rules.phonetic_mode.as_str())
        .bind(pos_ordinal as i32)
        .execute(&mut **tx)
        .await?;
        ensure!(
            updated.rows_affected() == 1,
            "missing_v2_pos: entry {} POS {}",
            plan.entry_id,
            pos.pos_id
        );
        for (group_ordinal, group) in pos.form_groups.iter().enumerate() {
            let synthetic = plan.mappings.iter().any(|mapping| {
                mapping.v3_node_id == group.id && mapping.role == "synthetic_base_only_group"
            });
            if synthetic {
                insert_generated_node(
                    tx,
                    plan.entry_id,
                    group.id,
                    "form_group",
                    Some(pos.pos_id),
                    "forms.form_group",
                )
                .await?;
            } else {
                convert_existing_node(
                    tx,
                    plan.entry_id,
                    group.id,
                    "form_group",
                    Some(pos.pos_id),
                    "forms.form_group",
                    false,
                )
                .await?;
            }
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_form_groups (
                    id, entry_id, entry_pos_id, is_regular, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(group.id)
            .bind(plan.entry_id)
            .bind(pos.pos_id)
            .bind(group.is_regular)
            .bind(group_ordinal as i32)
            .execute(&mut **tx)
            .await?;
        }
        for (form_ordinal, form) in pos.forms.iter().enumerate() {
            convert_existing_node(
                tx,
                plan.entry_id,
                form.id,
                "concrete_form",
                Some(pos.pos_id),
                "forms.concrete_form",
                false,
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_concrete_forms (
                    id, entry_id, entry_pos_id, form_type, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(form.id)
            .bind(plan.entry_id)
            .bind(pos.pos_id)
            .bind(form_type_name(form.form_type))
            .bind(form_ordinal as i32)
            .execute(&mut **tx)
            .await?;
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    insert_v3_variant(
                        tx,
                        plan.entry_id,
                        form.id,
                        "common",
                        common.id,
                        &common.spelling,
                        common.origin,
                        &common.pronunciations,
                    )
                    .await?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    insert_v3_variant(
                        tx,
                        plan.entry_id,
                        form.id,
                        "uk",
                        uk.id,
                        &uk.spelling,
                        uk.origin,
                        &uk.pronunciations,
                    )
                    .await?;
                    insert_v3_variant(
                        tx,
                        plan.entry_id,
                        form.id,
                        "us",
                        us.id,
                        &us.spelling,
                        us.origin,
                        &us.pronunciations,
                    )
                    .await?;
                }
            }
        }
        for group in &pos.form_groups {
            for (ordinal, membership) in group.members.iter().enumerate() {
                insert_generated_node(
                    tx,
                    plan.entry_id,
                    membership.id,
                    "group_membership",
                    Some(group.id),
                    "forms.group_membership",
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_group_memberships (
                        id, entry_id, entry_pos_id, form_group_id, form_id, ordinal
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    "#,
                )
                .bind(membership.id)
                .bind(plan.entry_id)
                .bind(pos.pos_id)
                .bind(group.id)
                .bind(membership.form_id)
                .bind(ordinal as i32)
                .execute(&mut **tx)
                .await?;
            }
        }
    }
    Ok(())
}

async fn insert_migration_map(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    plan: &MigrationPlan,
) -> anyhow::Result<()> {
    for mapping in &plan.mappings {
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_migration_map (
                batch_id, entry_id, v2_node_id, v3_node_id, role, mapping_kind
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (batch_id, entry_id, role, v3_node_id) DO NOTHING
            "#,
        )
        .bind(batch_id)
        .bind(plan.entry_id)
        .bind(mapping.v2_node_id)
        .bind(mapping.v3_node_id)
        .bind(mapping.role)
        .bind(mapping.mapping_kind)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn replace_v3_surface_projection(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    entry_id: Uuid,
    source_revision: i64,
    source_draft_surfaces: &Value,
    sources: &[V3FormVariantSurfaceSource],
) -> anyhow::Result<()> {
    let source_surface_count = source_draft_surfaces
        .as_array()
        .context("source draft surface manifest is not an array")?
        .len() as u64;
    let event_offset = sqlx::query_scalar::<_, i64>(
        "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
    )
    .fetch_one(&mut **tx)
    .await?;
    let retired = sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET source_revision = $2,
            event_offset = $3,
            is_deleted = TRUE,
            updated_at = now()
        WHERE entry_id = $1
          AND content_schema_version = 2
          AND content_scope = 'draft'
          AND is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .bind(source_revision)
    .bind(event_offset)
    .execute(&mut **tx)
    .await?;
    ensure!(
        retired.rows_affected() == source_surface_count,
        "source_draft_surface_retirement_mismatch: entry {entry_id}"
    );
    sqlx::query(
        r#"
        DELETE FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_schema_version = 3 AND content_scope = 'draft'
        "#,
    )
    .bind(entry_id)
    .execute(&mut **tx)
    .await?;
    for source in sources {
        sqlx::query(
            r#"
            INSERT INTO lexicon.surface_sources (
                entry_id, source_id, source_kind, source_node_id,
                language, entry_kind, dialect, dialect_scope,
                surface, normalized_surface, normalization_version,
                source_revision, event_offset, is_deleted, content_scope, publication_id,
                pos_id, pos, form_type, content_schema_version,
                form_id, variant_id, group_ids, projection_version, updated_at
            ) VALUES (
                $1, $2, 'form_variant', $3,
                'en', 'word', $4, $5,
                $6, $7, $8,
                $9, $10, FALSE, 'draft', NULL,
                $11, $12, $13, 3,
                $14, $15, $16, $17, now()
            )
            "#,
        )
        .bind(source.entry_id)
        .bind(&source.source_id)
        .bind(source.variant_id)
        .bind(source.dialect.as_str())
        .bind(source.dialect_scope.as_str())
        .bind(&source.surface)
        .bind(&source.normalized_surface)
        .bind(source.normalization_version)
        .bind(source_revision)
        .bind(event_offset)
        .bind(source.pos_id)
        .bind(&source.pos)
        .bind(form_type_name(source.form_type))
        .bind(source.form_id)
        .bind(source.variant_id)
        .bind(&source.group_ids)
        .bind(source.projection_version)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.surface_projection', $2, $3,
            'lexicon.surface_projection_replaced', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(event_offset)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "content_schema_version": 3,
        "content_scope": "draft",
        "publication_id": Option::<Uuid>::None,
        "source_revision": source_revision,
        "event_offset": event_offset,
        "source_count": sources.len(),
        "migration_batch_id": batch_id,
        "transition": "v2_to_v3",
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_migration_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    request_id: Uuid,
    action: &str,
    entry_id: Uuid,
    revision: i64,
    batch_id: Uuid,
) -> anyhow::Result<()> {
    let mappings = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT v3_node_id, mapping_kind
        FROM lexicon.v3_migration_map
        WHERE batch_id = $1 AND entry_id = $2
        ORDER BY v3_node_id
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await?;
    let changed_node_ids = mappings
        .iter()
        .map(|(node_id, _)| *node_id)
        .collect::<Vec<_>>();
    let generated_node_ids = mappings
        .iter()
        .filter_map(|(node_id, kind)| (kind == "deterministic_generated").then_some(*node_id))
        .collect::<Vec<_>>();
    let retired_node_ids = if action == "lexicon.entry.rollback.v3_migration" {
        generated_node_ids.clone()
    } else {
        Vec::new()
    };
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        ) VALUES ($1, $2, $3, 'lexicon.entry', $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(entry_id)
    .bind(revision)
    .bind(request_id)
    .bind(serde_json::json!({
        "migration_batch_id": batch_id,
        "source_schema_version": 2,
        "target_schema_version": 3,
        "changed_node_ids": changed_node_ids,
        "generated_node_ids": generated_node_ids,
        "retired_node_ids": retired_node_ids,
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_migration_batch_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    request_id: Uuid,
    action: &str,
    batch_id: Uuid,
    metadata: Value,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        ) VALUES ($1, $2, $3, 'lexicon.migration_batch', $4, NULL, $5, $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(batch_id)
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn existing_batch_digest(
    pool: &PgPool,
    batch_id: Uuid,
    entry_id: Uuid,
) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT migration.expected_digest
        FROM lexicon.v3_migration_entries migration
        JOIN lexicon.v3_entry_state state
          ON state.entry_id = migration.entry_id
         AND state.migration_batch_id = migration.batch_id
        WHERE migration.batch_id = $1
          AND migration.entry_id = $2
          AND migration.status IN ('applied', 'verified')
          AND state.origin = 'migrated_v2'
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .fetch_optional(pool)
    .await?)
}

fn ensure_plan_matches_manifest(
    plan: &MigrationPlan,
    backup: &MigrationBackupRow,
) -> anyhow::Result<()> {
    ensure!(
        plan.entry_id == backup.entry_id
            && plan.source_revision == backup.source_revision
            && plan.source_current_publication_id == backup.source_current_publication_id
            && plan.source_publications_digest == backup.source_publications_digest
            && serde_json::to_value(&plan.source_pos_modes)? == backup.source_pos_modes
            && plan.source_forms == backup.source_forms
            && plan.source_meanings == backup.source_meanings
            && plan.source_draft_surfaces == backup.source_draft_surfaces
            && plan.target_forms_value == backup.expected_forms
            && serde_json::to_value(&plan.presentation)? == backup.expected_presentation
            && plan.expected_digest == backup.expected_digest,
        "migration_manifest_source_changed: entry {}",
        backup.entry_id
    );
    Ok(())
}

async fn apply_plan(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    plan: &MigrationPlan,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let source_version = lock_and_recheck_source(&mut tx, plan).await?;
    if source_version == 3 {
        let state_batch: Option<Uuid> = sqlx::query_scalar(
            "SELECT migration_batch_id FROM lexicon.v3_entry_state WHERE entry_id = $1 AND origin = 'migrated_v2'",
        )
        .bind(plan.entry_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        ensure!(
            state_batch == Some(batch_id),
            "entry_already_migrated_by_another_batch: entry {}",
            plan.entry_id
        );
        tx.commit().await?;
        return Ok(true);
    }

    let status: String = sqlx::query_scalar(
        "SELECT status FROM lexicon.v3_migration_entries WHERE batch_id = $1 AND entry_id = $2 FOR UPDATE",
    )
    .bind(batch_id)
    .bind(plan.entry_id)
    .fetch_one(&mut *tx)
    .await?;
    if matches!(status.as_str(), "applied" | "verified") {
        tx.commit().await?;
        return Ok(true);
    }
    ensure!(
        status == "planned",
        "migration_entry_not_applicable: entry {} status {status}",
        plan.entry_id
    );

    let updated = sqlx::query(
        r#"
        UPDATE lexicon.entries
        SET content_schema_version = 3
        WHERE id = $1 AND content_schema_version = 2 AND revision = $2
          AND current_publication_id IS NOT DISTINCT FROM $3
        "#,
    )
    .bind(plan.entry_id)
    .bind(plan.source_revision)
    .bind(plan.source_current_publication_id)
    .execute(&mut *tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "source_changed_since_dry_run: entry {}",
        plan.entry_id
    );
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_entry_state (
            entry_id, content_schema_version, origin, migration_batch_id,
            source_publication_id, source_revision, first_v3_write_revision,
            publication_canary_enabled
        ) VALUES ($1, 3, 'migrated_v2', $2, $3, $4, NULL, FALSE)
        "#,
    )
    .bind(plan.entry_id)
    .bind(batch_id)
    .bind(plan.source_current_publication_id)
    .bind(plan.source_revision)
    .execute(&mut *tx)
    .await?;

    write_v3_forms(&mut tx, plan).await?;
    sqlx::query(
        r#"
        UPDATE lexicon.entry_editor_projection
        SET forms = $2, meanings = $3, rebuilt_revision = $4, updated_at = now()
        WHERE entry_id = $1
        "#,
    )
    .bind(plan.entry_id)
    .bind(&plan.target_forms_value)
    .bind(&plan.target_meanings)
    .bind(plan.source_revision)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_presentation_projection (
            entry_id, content_schema_version, source_revision,
            label, matched_surfaces, strategy_version, updated_at
        ) VALUES ($1, 3, $2, $3, $4, $5, now())
        ON CONFLICT (entry_id) DO UPDATE
        SET content_schema_version = 3,
            source_revision = EXCLUDED.source_revision,
            label = EXCLUDED.label,
            matched_surfaces = EXCLUDED.matched_surfaces,
            strategy_version = EXCLUDED.strategy_version,
            updated_at = now()
        "#,
    )
    .bind(plan.entry_id)
    .bind(plan.source_revision)
    .bind(&plan.presentation.label)
    .bind(&plan.presentation.matched_surfaces)
    .bind(&plan.presentation.strategy_version)
    .execute(&mut *tx)
    .await?;
    let surface_sources = form_variant_sources(plan.entry_id, &plan.target_forms)
        .context("converted V3 forms could not produce a surface projection")?;
    replace_v3_surface_projection(
        &mut tx,
        batch_id,
        plan.entry_id,
        plan.source_revision,
        &plan.source_draft_surfaces,
        &surface_sources,
    )
    .await?;
    ensure!(
        publication_fingerprint_tx(&mut tx, plan.entry_id).await?
            == plan.source_publications_digest,
        "historical_publication_changed_during_migration: entry {}",
        plan.entry_id
    );
    let migration_updated = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'applied', applied_digest = expected_digest, applied_at = now()
        WHERE batch_id = $1 AND entry_id = $2 AND status = 'planned'
        "#,
    )
    .bind(batch_id)
    .bind(plan.entry_id)
    .execute(&mut *tx)
    .await?;
    ensure!(
        migration_updated.rows_affected() == 1,
        "migration_entry_changed_during_apply: batch {batch_id} entry {}",
        plan.entry_id
    );
    insert_migration_audit(
        &mut tx,
        actor_id,
        request_id,
        "lexicon.entry.migrate.v2_to_v3",
        plan.entry_id,
        plan.source_revision,
        batch_id,
    )
    .await?;
    tx.commit().await?;
    Ok(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyFailureCheckpoint {
    Failed,
    AlreadyApplied,
}

fn classify_apply_failure_checkpoint(
    status: &str,
    expected_digest: &[u8],
    applied_digest: Option<&[u8]>,
    batch_id: Uuid,
    entry_id: Uuid,
) -> anyhow::Result<ApplyFailureCheckpoint> {
    match status {
        "planned" | "failed" => Ok(ApplyFailureCheckpoint::Failed),
        "applied" | "verified" => {
            ensure!(
                applied_digest == Some(expected_digest),
                "migration_entry_digest_mismatch_after_apply: batch {batch_id} entry {entry_id}"
            );
            Ok(ApplyFailureCheckpoint::AlreadyApplied)
        }
        _ => anyhow::bail!(
            "migration_entry_changed_during_apply: batch {batch_id} entry {entry_id} is {status}"
        ),
    }
}

async fn record_failed_entry(
    pool: &PgPool,
    batch_id: Uuid,
    entry_id: Uuid,
) -> anyhow::Result<ApplyFailureCheckpoint> {
    let mut tx = pool.begin().await?;
    let (status, expected_digest, applied_digest): (String, Vec<u8>, Option<Vec<u8>>) =
        sqlx::query_as(
            r#"
            SELECT status, expected_digest, applied_digest
            FROM lexicon.v3_migration_entries
            WHERE batch_id = $1 AND entry_id = $2
            FOR UPDATE
            "#,
        )
        .bind(batch_id)
        .bind(entry_id)
        .fetch_one(&mut *tx)
        .await?;
    let checkpoint = classify_apply_failure_checkpoint(
        &status,
        &expected_digest,
        applied_digest.as_deref(),
        batch_id,
        entry_id,
    )?;
    if checkpoint == ApplyFailureCheckpoint::Failed {
        let updated = sqlx::query(
            r#"
                UPDATE lexicon.v3_migration_entries
                SET status = 'failed', failure_code = 'apply_failed'
                WHERE batch_id = $1 AND entry_id = $2
                  AND status = $3
                "#,
        )
        .bind(batch_id)
        .bind(entry_id)
        .bind(&status)
        .execute(&mut *tx)
        .await?;
        ensure!(
            updated.rows_affected() == 1,
            "migration_entry_changed_during_apply: batch {batch_id} entry {entry_id}"
        );
    }
    tx.commit().await?;
    Ok(checkpoint)
}

async fn finish_batch(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
) -> anyhow::Result<(i64, i64, i64)> {
    let (batch_status, scanned_count, eligible_count): (String, i32, i32) = sqlx::query_as(
        r#"
        SELECT status, scanned_count, eligible_count
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_optional(&mut **tx)
    .await?
    .with_context(|| format!("migration_batch_not_found: {batch_id}"))?;
    ensure!(
        matches!(batch_status.as_str(), "applying" | "applied" | "verified"),
        "migration_batch_changed_during_apply: batch {batch_id} is {batch_status}"
    );
    let (applied, blocked, failed, planned) = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        r#"
        SELECT count(*) FILTER (WHERE status IN ('applied', 'verified')),
               count(*) FILTER (WHERE status = 'blocked'),
               count(*) FILTER (WHERE status = 'failed'),
               count(*) FILTER (WHERE status = 'planned')
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        "#,
    )
    .bind(batch_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        planned == 0
            && applied + blocked + failed == i64::from(scanned_count)
            && applied + failed == i64::from(eligible_count)
            && i64::from(eligible_count) + blocked == i64::from(scanned_count),
        "migration_manifest_control_mismatch: batch {batch_id}"
    );
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_batches
        SET status = CASE
                WHEN $4 > 0 THEN 'failed'
                WHEN status = 'verified' THEN 'verified'
                ELSE 'applied'
            END,
            applied_count = $2,
            blocked_count = $3,
            failed_count = $4,
            finished_at = now()
        WHERE id = $1 AND status = $5
        "#,
    )
    .bind(batch_id)
    .bind(applied as i32)
    .bind(blocked as i32)
    .bind(failed as i32)
    .bind(&batch_status)
    .execute(&mut **tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "migration_batch_changed_during_apply: {batch_id}"
    );
    Ok((applied, blocked, failed))
}

async fn mark_manifest_preflight_failed(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    entry_id: Uuid,
) -> anyhow::Result<()> {
    let failed_entry = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'failed', failure_code = 'manifest_source_changed'
        WHERE batch_id = $1 AND entry_id = $2 AND status = 'planned'
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await?;
    ensure!(
        failed_entry.rows_affected() == 1,
        "migration_entry_changed_during_preflight: batch {batch_id} entry {entry_id}"
    );
    sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'failed', failure_code = 'manifest_preflight_aborted'
        WHERE batch_id = $1 AND status = 'planned'
        "#,
    )
    .bind(batch_id)
    .execute(&mut **tx)
    .await?;
    let (applied, blocked, failed, planned) = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        r#"
        SELECT count(*) FILTER (WHERE status IN ('applied', 'verified')),
               count(*) FILTER (WHERE status = 'blocked'),
               count(*) FILTER (WHERE status = 'failed'),
               count(*) FILTER (WHERE status = 'planned')
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        "#,
    )
    .bind(batch_id)
    .fetch_one(&mut **tx)
    .await?;
    let (batch_status, scanned_count, eligible_count): (String, i32, i32) = sqlx::query_as(
        r#"
        SELECT status, scanned_count, eligible_count
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        batch_status == "applying"
            && planned == 0
            && applied + blocked + failed == i64::from(scanned_count)
            && applied + failed == i64::from(eligible_count)
            && i64::from(eligible_count) + blocked == i64::from(scanned_count),
        "migration_manifest_control_mismatch: batch {batch_id}"
    );
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_batches
        SET status = 'failed',
            applied_count = $2,
            blocked_count = $3,
            failed_count = $4,
            finished_at = now()
        WHERE id = $1 AND status = $5
        "#,
    )
    .bind(batch_id)
    .bind(applied as i32)
    .bind(blocked as i32)
    .bind(failed as i32)
    .bind(&batch_status)
    .execute(&mut **tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "migration_batch_changed_during_preflight: {batch_id}"
    );
    Ok(())
}

pub async fn apply(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    expected_manifest_digest: &str,
) -> anyhow::Result<MigrationApplyReport> {
    #[derive(FromRow)]
    struct Batch {
        status: String,
        selection_digest: Vec<u8>,
        manifest_digest: Vec<u8>,
        scanned_count: i32,
        eligible_count: i32,
    }
    #[derive(FromRow)]
    struct BlockedEntry {
        entry_id: Uuid,
        block_code: String,
    }
    #[derive(FromRow)]
    struct FailedEntry {
        entry_id: Uuid,
        failure_code: String,
    }

    // Keep this transaction open while per-entry checkpoint transactions use
    // a second pool connection. The configured application/test pools both
    // provide more than one connection.
    let mut control_tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.plan:{batch_id}"))
        .execute(&mut *control_tx)
        .await?;
    let locked_entry_ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT entry_id
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *control_tx)
    .await?;
    for entry_id in &locked_entry_ids {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
            .execute(&mut *control_tx)
            .await?;
    }
    let batch = sqlx::query_as::<_, Batch>(
        r#"
        SELECT status, selection_digest, manifest_digest,
               scanned_count, eligible_count
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_optional(&mut *control_tx)
    .await?
    .with_context(|| format!("migration_dry_run_required: batch {batch_id}"))?;
    ensure!(
        hex_digest(&batch.manifest_digest) == expected_manifest_digest,
        "migration_manifest_digest_mismatch: batch {batch_id}"
    );
    match batch.status.as_str() {
        "planned" => anyhow::bail!("migration_approval_required: batch {batch_id}"),
        "approved" => {
            sqlx::query(
                "UPDATE lexicon.v3_migration_batches SET status = 'applying' WHERE id = $1 AND status = 'approved'",
            )
            .bind(batch_id)
            .execute(&mut *control_tx)
            .await?;
        }
        "applying" | "applied" | "verified" => {}
        status => anyhow::bail!("migration_batch_not_applicable: batch {batch_id} is {status}"),
    }
    let current_entry_ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT entry_id
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *control_tx)
    .await?;
    ensure!(
        current_entry_ids == locked_entry_ids,
        "migration_batch_changed_during_apply: {batch_id}"
    );

    let backups = sqlx::query_as::<_, MigrationBackupRow>(
        r#"
        SELECT entry_id, status, source_revision, source_current_publication_id,
               source_publications_digest, source_pos_modes, source_forms,
               source_meanings, source_draft_surfaces, expected_forms,
               expected_presentation,
               expected_digest, applied_digest
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status IN ('planned', 'applied', 'verified')
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *control_tx)
    .await?;
    let blocked_entries = sqlx::query_as::<_, BlockedEntry>(
        r#"
        SELECT entry_id, block_code
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status = 'blocked'
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *control_tx)
    .await?;
    let failed_entries = sqlx::query_as::<_, FailedEntry>(
        r#"
        SELECT entry_id, failure_code
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status = 'failed'
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *control_tx)
    .await?;
    ensure!(
        backups.len() + blocked_entries.len() + failed_entries.len()
            == batch.scanned_count as usize,
        "migration_manifest_control_mismatch: batch {batch_id}"
    );

    // Validate the entire approved manifest before the first business write.
    // Entry transactions still checkpoint independently after this gate.
    let mut plans = Vec::with_capacity(backups.len());
    for backup in &backups {
        if matches!(backup.status.as_str(), "applied" | "verified")
            || existing_batch_digest(pool, batch_id, backup.entry_id)
                .await?
                .is_some()
        {
            plans.push(None);
            continue;
        }
        let plan = match load_plan(pool, backup.entry_id).await? {
            Ok(plan) => plan,
            Err(issue) => {
                if existing_batch_digest(pool, batch_id, backup.entry_id)
                    .await?
                    .is_some()
                {
                    plans.push(None);
                    continue;
                }
                mark_manifest_preflight_failed(&mut control_tx, batch_id, backup.entry_id).await?;
                control_tx.commit().await?;
                anyhow::bail!(
                    "migration_manifest_source_changed: entry {} ({})",
                    backup.entry_id,
                    issue.code
                );
            }
        };
        if let Err(error) = ensure_plan_matches_manifest(&plan, backup) {
            mark_manifest_preflight_failed(&mut control_tx, batch_id, backup.entry_id).await?;
            control_tx.commit().await?;
            return Err(error);
        }
        plans.push(Some(plan));
    }

    let mut entries = blocked_entries
        .into_iter()
        .map(|entry| MigrationEntryResult {
            entry_id: entry.entry_id,
            status: "blocked".to_owned(),
            digest: None,
            reason: Some(entry.block_code),
        })
        .collect::<Vec<_>>();
    entries.extend(
        failed_entries
            .into_iter()
            .map(|entry| MigrationEntryResult {
                entry_id: entry.entry_id,
                status: "failed".to_owned(),
                digest: None,
                reason: Some(entry.failure_code),
            }),
    );
    let mut replayed_entries = 0;
    for (backup, plan) in backups.iter().zip(plans) {
        let Some(plan) = plan else {
            replayed_entries += 1;
            entries.push(MigrationEntryResult {
                entry_id: backup.entry_id,
                status: "replayed".to_owned(),
                digest: Some(hex_digest(&backup.expected_digest)),
                reason: None,
            });
            continue;
        };
        match apply_plan(pool, batch_id, actor_id, request_id, &plan).await {
            Ok(replayed) => {
                replayed_entries += usize::from(replayed);
                entries.push(MigrationEntryResult {
                    entry_id: backup.entry_id,
                    status: if replayed { "replayed" } else { "applied" }.to_owned(),
                    digest: Some(hex_digest(&plan.expected_digest)),
                    reason: None,
                });
            }
            Err(error) => match record_failed_entry(pool, batch_id, backup.entry_id).await? {
                ApplyFailureCheckpoint::Failed => entries.push(MigrationEntryResult {
                    entry_id: backup.entry_id,
                    status: "failed".to_owned(),
                    digest: None,
                    reason: Some(error.to_string()),
                }),
                ApplyFailureCheckpoint::AlreadyApplied => {
                    replayed_entries += 1;
                    entries.push(MigrationEntryResult {
                        entry_id: backup.entry_id,
                        status: "replayed".to_owned(),
                        digest: Some(hex_digest(&backup.expected_digest)),
                        reason: None,
                    });
                }
            },
        }
    }
    entries.sort_by_key(|entry| entry.entry_id);
    let (applied, blocked, failed) = finish_batch(&mut control_tx, batch_id).await?;
    control_tx.commit().await?;
    Ok(MigrationApplyReport {
        schema_version: 1,
        mode: "apply",
        batch_id,
        selection_digest: hex_digest(&batch.selection_digest),
        scanned_entries: batch.scanned_count as usize,
        eligible_entries: batch.eligible_count as usize,
        applied_entries: applied as usize,
        replayed_entries,
        blocked_entries: blocked as usize,
        failed_entries: failed as usize,
        entries,
    })
}

fn counts_from_forms(forms: &DraftFormsStepContentV3) -> MigrationNodeCounts {
    let mut counts = MigrationNodeCounts {
        pos: forms.pos.len(),
        ..MigrationNodeCounts::default()
    };
    for pos in &forms.pos {
        counts.form_groups += pos.form_groups.len();
        counts.concrete_forms += pos.forms.len();
        counts.memberships += pos
            .form_groups
            .iter()
            .map(|group| group.members.len())
            .sum::<usize>();
        for form in &pos.forms {
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    counts.variants += 1;
                    counts.pronunciations += common.pronunciations.len();
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    counts.variants += 2;
                    counts.pronunciations += uk.pronunciations.len() + us.pronunciations.len();
                }
            }
        }
    }
    counts
}

async fn verify_entry(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    backup: &MigrationBackupRow,
) -> anyhow::Result<Vec<u8>> {
    #[derive(FromRow)]
    struct Current {
        content_schema_version: i16,
        revision: i64,
        current_publication_id: Option<Uuid>,
        forms: Value,
        meanings: Value,
        origin: String,
        migration_batch_id: Option<Uuid>,
        presentation_revision: Option<i64>,
        presentation_label: Option<String>,
        presentation_surfaces: Option<Vec<String>>,
        strategy_version: Option<String>,
    }
    let current = sqlx::query_as::<_, Current>(
        r#"
        SELECT entry.content_schema_version, entry.revision,
               entry.current_publication_id, projection.forms, projection.meanings,
               state.origin, state.migration_batch_id,
               presentation.source_revision AS presentation_revision,
               presentation.label AS presentation_label,
               presentation.matched_surfaces AS presentation_surfaces,
               presentation.strategy_version
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        JOIN lexicon.v3_entry_state state ON state.entry_id = entry.id
        LEFT JOIN lexicon.entry_presentation_projection presentation
          ON presentation.entry_id = entry.id
        WHERE entry.id = $1
        FOR UPDATE OF entry, projection, state
        "#,
    )
    .bind(backup.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        current.content_schema_version == 3
            && current.revision == backup.source_revision
            && current.current_publication_id == backup.source_current_publication_id
            && current.origin == "migrated_v2"
            && current.migration_batch_id == Some(batch_id),
        "migration_state_mismatch: entry {}",
        backup.entry_id
    );
    ensure!(
        current.forms == backup.expected_forms,
        "migration_forms_digest_mismatch: entry {}",
        backup.entry_id
    );
    let actual_digest = digest_json(&serde_json::json!({
        "schema_version": 3,
        "forms": &current.forms,
        "meanings": &current.meanings,
    }))?;
    ensure!(
        actual_digest == backup.expected_digest
            && backup.applied_digest.as_deref() == Some(backup.expected_digest.as_slice()),
        "migration_content_digest_mismatch: entry {}",
        backup.entry_id
    );
    ensure!(
        publication_fingerprint_tx(tx, backup.entry_id).await? == backup.source_publications_digest,
        "migration_publication_digest_mismatch: entry {}",
        backup.entry_id
    );
    let actual_presentation = serde_json::json!({
        "label": current.presentation_label,
        "matched_surfaces": current.presentation_surfaces,
        "strategy_version": current.strategy_version,
    });
    ensure!(
        current.presentation_revision == Some(backup.source_revision)
            && actual_presentation == backup.expected_presentation,
        "migration_presentation_mismatch: entry {}",
        backup.entry_id
    );

    let forms: DraftFormsStepContentV3 = serde_json::from_value(current.forms.clone())?;
    let expected_counts = counts_from_forms(&forms);
    let actual_counts = MigrationNodeCounts {
        pos: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.entry_pos WHERE entry_id = $1 AND content_schema_version = 3",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
        form_groups: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.v3_form_groups WHERE entry_id = $1",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
        synthetic_groups: 0,
        concrete_forms: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
        memberships: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.v3_group_memberships WHERE entry_id = $1",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
        variants: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.v3_form_variants WHERE entry_id = $1",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
        pronunciations: sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon.v3_pronunciations WHERE entry_id = $1",
        )
        .bind(backup.entry_id)
        .fetch_one(&mut **tx)
        .await? as usize,
    };
    ensure!(
        actual_counts.pos == expected_counts.pos
            && actual_counts.form_groups == expected_counts.form_groups
            && actual_counts.concrete_forms == expected_counts.concrete_forms
            && actual_counts.memberships == expected_counts.memberships
            && actual_counts.variants == expected_counts.variants
            && actual_counts.pronunciations == expected_counts.pronunciations,
        "migration_node_count_mismatch: entry {}",
        backup.entry_id
    );
    let expected_surface_count = form_variant_sources(backup.entry_id, &forms)?.len() as i64;
    let surface_events = sqlx::query_as::<_, (i64, Value)>(
        r#"
        SELECT aggregate_revision, payload
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'migration_batch_id' = $2
          AND payload ->> 'transition' = 'v2_to_v3'
        ORDER BY occurred_at, id
        "#,
    )
    .bind(backup.entry_id)
    .bind(batch_id.to_string())
    .fetch_all(&mut **tx)
    .await?;
    ensure!(
        surface_events.len() == 1,
        "migration_surface_outbox_mismatch: entry {}",
        backup.entry_id
    );
    let (aggregate_revision, payload) = &surface_events[0];
    let event_offset = payload
        .get("event_offset")
        .and_then(Value::as_i64)
        .context("migration surface outbox event_offset is invalid")?;
    ensure!(
        *aggregate_revision == event_offset
            && payload
                .get("content_schema_version")
                .and_then(Value::as_i64)
                == Some(3)
            && payload.get("source_revision").and_then(Value::as_i64)
                == Some(backup.source_revision)
            && payload.get("source_count").and_then(Value::as_i64) == Some(expected_surface_count),
        "migration_surface_outbox_mismatch: entry {}",
        backup.entry_id
    );
    let (actual_surface_count, active_v3_offset_count): (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*), count(*) FILTER (WHERE event_offset = $2)
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_schema_version = 3
          AND content_scope = 'draft' AND is_deleted = FALSE
        "#,
    )
    .bind(backup.entry_id)
    .bind(event_offset)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        actual_surface_count == expected_surface_count
            && active_v3_offset_count == expected_surface_count,
        "migration_surface_projection_mismatch: entry {}",
        backup.entry_id
    );
    let active_v2_draft_surfaces: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_schema_version = 2
          AND content_scope = 'draft' AND is_deleted = FALSE
        "#,
    )
    .bind(backup.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        active_v2_draft_surfaces == 0,
        "migration_v2_surface_retirement_mismatch: entry {}",
        backup.entry_id
    );
    let source_surface_count = backup
        .source_draft_surfaces
        .as_array()
        .context("source draft surface manifest is not an array")?
        .len() as i64;
    let (retired_source_count, matching_source_offsets): (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*), count(*) FILTER (
                   WHERE source.is_deleted = TRUE AND source.event_offset = $3
               )
        FROM lexicon.surface_sources source
        JOIN jsonb_to_recordset($2::jsonb) AS manifest(
            source_id TEXT,
            dialect_scope TEXT,
            normalization_version SMALLINT
        )
          ON source.source_id = manifest.source_id
         AND source.dialect_scope = manifest.dialect_scope
         AND source.normalization_version = manifest.normalization_version
        WHERE source.entry_id = $1
          AND source.content_schema_version = 2
          AND source.content_scope = 'draft'
        "#,
    )
    .bind(backup.entry_id)
    .bind(&backup.source_draft_surfaces)
    .bind(event_offset)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        retired_source_count == source_surface_count
            && matching_source_offsets == source_surface_count,
        "migration_v2_surface_retirement_mismatch: entry {}",
        backup.entry_id
    );
    Ok(actual_digest)
}

async fn migration_backups(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
) -> anyhow::Result<Vec<MigrationBackupRow>> {
    Ok(sqlx::query_as::<_, MigrationBackupRow>(
        r#"
        SELECT entry_id, status, source_revision, source_current_publication_id,
               source_publications_digest, source_pos_modes, source_forms,
               source_meanings, source_draft_surfaces, expected_forms,
               expected_presentation,
               expected_digest, applied_digest
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status IN ('applied', 'verified')
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut **tx)
    .await?)
}

pub async fn verify(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
) -> anyhow::Result<MigrationVerifyReport> {
    #[derive(FromRow)]
    struct BatchState {
        status: String,
        scanned_count: i32,
        blocked_count: i32,
        failed_count: i32,
    }
    #[derive(FromRow)]
    struct NonAppliedEntry {
        entry_id: Uuid,
        status: String,
        block_code: Option<String>,
        failure_code: Option<String>,
    }
    let mut tx = pool.begin().await?;
    let locked_entry_ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT entry_id
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status IN ('applied', 'verified')
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *tx)
    .await?;
    for entry_id in &locked_entry_ids {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
            .execute(&mut *tx)
            .await?;
    }
    let batch = sqlx::query_as::<_, BatchState>(
        r#"
        SELECT status, scanned_count, blocked_count, failed_count
        FROM lexicon.v3_migration_batches
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(batch_id)
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("migration_batch_not_found: {batch_id}"))?;
    ensure!(
        matches!(batch.status.as_str(), "applied" | "verified" | "failed"),
        "migration_batch_not_verifiable: batch {batch_id} is {}",
        batch.status
    );
    let backups = migration_backups(&mut tx, batch_id).await?;
    ensure!(
        backups
            .iter()
            .map(|backup| backup.entry_id)
            .eq(locked_entry_ids),
        "migration_batch_changed_during_verify: {batch_id}"
    );
    if batch.status == "verified" {
        ensure!(
            batch.blocked_count == 0
                && batch.failed_count == 0
                && batch.scanned_count as usize == backups.len()
                && backups.iter().all(|backup| backup.status == "verified"),
            "migration_batch_changed_during_verify: {batch_id}"
        );
        let entries = backups
            .iter()
            .map(|backup| MigrationEntryResult {
                entry_id: backup.entry_id,
                status: "verified".to_owned(),
                digest: Some(hex_digest(&backup.expected_digest)),
                reason: None,
            })
            .collect::<Vec<_>>();
        tx.commit().await?;
        return Ok(MigrationVerifyReport {
            schema_version: 1,
            mode: "verify",
            batch_id,
            checked_entries: entries.len(),
            verified_entries: entries.len(),
            ready: true,
            entries,
        });
    }
    let non_applied = sqlx::query_as::<_, NonAppliedEntry>(
        r#"
        SELECT entry_id, status, block_code, failure_code
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status NOT IN ('applied', 'verified')
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut entries = Vec::with_capacity(backups.len() + non_applied.len());
    let mut verified_entries = 0;
    for backup in &backups {
        match verify_entry(&mut tx, batch_id, backup).await {
            Ok(digest) => {
                verified_entries += 1;
                entries.push(MigrationEntryResult {
                    entry_id: backup.entry_id,
                    status: "verified".to_owned(),
                    digest: Some(hex_digest(&digest)),
                    reason: None,
                });
            }
            Err(error) => entries.push(MigrationEntryResult {
                entry_id: backup.entry_id,
                status: "failed".to_owned(),
                digest: None,
                reason: Some(error.to_string()),
            }),
        }
    }
    entries.extend(non_applied.iter().map(|entry| {
        MigrationEntryResult {
            entry_id: entry.entry_id,
            status: entry.status.clone(),
            digest: None,
            reason: entry
                .block_code
                .clone()
                .or_else(|| entry.failure_code.clone()),
        }
    }));
    entries.sort_by_key(|entry| entry.entry_id);
    let ready = verified_entries == backups.len()
        && non_applied.is_empty()
        && batch.blocked_count == 0
        && batch.failed_count == 0
        && batch.scanned_count as usize == backups.len();
    if ready {
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.v3_migration_entries
            SET status = 'verified', verified_at = COALESCE(verified_at, now())
            WHERE batch_id = $1 AND status IN ('applied', 'verified')
            "#,
        )
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
        ensure!(
            updated.rows_affected() == backups.len() as u64,
            "migration_batch_changed_during_verify: {batch_id}"
        );
    }
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_batches
        SET status = CASE WHEN $2 THEN 'verified' ELSE 'failed' END,
            finished_at = now()
        WHERE id = $1 AND status = $3
        "#,
    )
    .bind(batch_id)
    .bind(ready)
    .bind(&batch.status)
    .execute(&mut *tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "migration_batch_changed_during_verify: {batch_id}"
    );
    insert_migration_batch_audit(
        &mut tx,
        actor_id,
        request_id,
        "lexicon.migration_batch.verify",
        batch_id,
        serde_json::json!({
            "source_schema_version": 2,
            "target_schema_version": 3,
            "checked_entries": backups.len() + non_applied.len(),
            "verified_entries": verified_entries,
            "ready": ready,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(MigrationVerifyReport {
        schema_version: 1,
        mode: "verify",
        batch_id,
        checked_entries: backups.len() + non_applied.len(),
        verified_entries,
        ready,
        entries,
    })
}

pub async fn enable_publication_canary(
    pool: &PgPool,
    batch_id: Uuid,
    entry_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
) -> anyhow::Result<MigrationCanaryReport> {
    #[derive(FromRow)]
    struct CanaryState {
        batch_status: String,
        entry_status: String,
        origin: String,
        migration_batch_id: Option<Uuid>,
        source_publication_id: Option<Uuid>,
        publication_canary_enabled: bool,
        revision: i64,
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
        .execute(&mut *tx)
        .await?;
    let state = sqlx::query_as::<_, CanaryState>(
        r#"
        SELECT batch.status AS batch_status,
               migration.status AS entry_status,
               state.origin, state.migration_batch_id,
               state.source_publication_id,
               state.publication_canary_enabled,
               entry.revision
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.v3_migration_entries migration
          ON migration.batch_id = batch.id AND migration.entry_id = $2
        JOIN lexicon.v3_entry_state state ON state.entry_id = migration.entry_id
        JOIN lexicon.entries entry ON entry.id = migration.entry_id
        WHERE batch.id = $1
        FOR UPDATE OF batch, migration, state, entry
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("migration_canary_not_eligible: batch {batch_id} entry {entry_id}"))?;
    ensure!(
        state.batch_status == "verified"
            && state.entry_status == "verified"
            && state.origin == "migrated_v2"
            && state.migration_batch_id == Some(batch_id)
            && state.source_publication_id.is_some(),
        "migration_canary_not_eligible: batch {batch_id} entry {entry_id}"
    );
    let replayed = state.publication_canary_enabled;
    if !replayed {
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.v3_entry_state
            SET publication_canary_enabled = TRUE
            WHERE entry_id = $1 AND origin = 'migrated_v2'
              AND migration_batch_id = $2
              AND source_publication_id IS NOT NULL
              AND publication_canary_enabled = FALSE
            "#,
        )
        .bind(entry_id)
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
        ensure!(
            updated.rows_affected() == 1,
            "migration_canary_not_eligible: batch {batch_id} entry {entry_id}"
        );
        insert_migration_audit(
            &mut tx,
            actor_id,
            request_id,
            "lexicon.entry.enable_v3_publication_canary",
            entry_id,
            state.revision,
            batch_id,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(MigrationCanaryReport {
        schema_version: 1,
        mode: "enable_publication_canary",
        batch_id,
        entry_id,
        replayed,
    })
}

async fn rollback_preflight(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    backup: &MigrationBackupRow,
) -> anyhow::Result<()> {
    #[derive(FromRow)]
    struct Current {
        content_schema_version: i16,
        revision: i64,
        current_publication_id: Option<Uuid>,
        forms: Value,
        meanings: Value,
        origin: String,
        migration_batch_id: Option<Uuid>,
        first_v3_write_revision: Option<i64>,
    }
    let current = sqlx::query_as::<_, Current>(
        r#"
        SELECT entry.content_schema_version, entry.revision,
               entry.current_publication_id, projection.forms, projection.meanings,
               state.origin, state.migration_batch_id, state.first_v3_write_revision
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        JOIN lexicon.v3_entry_state state ON state.entry_id = entry.id
        WHERE entry.id = $1
        FOR UPDATE OF entry, projection, state
        "#,
    )
    .bind(backup.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        current.content_schema_version == 3
            && current.origin == "migrated_v2"
            && current.migration_batch_id == Some(batch_id),
        "migration_rollback_state_mismatch: entry {}",
        backup.entry_id
    );
    ensure!(
        current.first_v3_write_revision.is_none(),
        "{ROLLBACK_BLOCKED_V3_WRITE}: entry {} first V3 write revision is {}",
        backup.entry_id,
        current.first_v3_write_revision.unwrap_or_default()
    );
    let current_digest = digest_json(&serde_json::json!({
        "schema_version": 3,
        "forms": &current.forms,
        "meanings": &current.meanings,
    }))?;
    ensure!(
        current.revision == backup.source_revision
            && current.forms == backup.expected_forms
            && current_digest == backup.expected_digest,
        "{ROLLBACK_BLOCKED_V3_WRITE}: entry {} canonical V3 content changed",
        backup.entry_id
    );
    let v3_publication_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = $1 AND content_schema_version = 3",
    )
    .bind(backup.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        v3_publication_count == 0,
        "{ROLLBACK_BLOCKED_V3_PUBLICATION}: entry {} has a V3 publication",
        backup.entry_id
    );
    ensure!(
        current.current_publication_id == backup.source_current_publication_id
            && publication_fingerprint_tx(tx, backup.entry_id).await?
                == backup.source_publications_digest,
        "{ROLLBACK_BLOCKED_SOURCE_CHANGED}: entry {}",
        backup.entry_id
    );
    Ok(())
}

async fn rollback_entry(
    tx: &mut Transaction<'_, Postgres>,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
    backup: &MigrationBackupRow,
) -> anyhow::Result<()> {
    let event_offset = sqlx::query_scalar::<_, i64>(
        "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
    )
    .fetch_one(&mut **tx)
    .await?;
    for statement in [
        "DELETE FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3",
        "DELETE FROM lexicon.entry_presentation_projection WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_pronunciations WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_group_memberships WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_concrete_forms WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_groups WHERE entry_id = $1",
    ] {
        sqlx::query(statement)
            .bind(backup.entry_id)
            .execute(&mut **tx)
            .await?;
    }
    let source_surface_count = backup
        .source_draft_surfaces
        .as_array()
        .context("source draft surface manifest is not an array")?
        .len() as u64;
    let restored_surfaces = sqlx::query(
        r#"
        UPDATE lexicon.surface_sources source
        SET is_deleted = FALSE,
            source_revision = manifest.source_revision,
            event_offset = $3,
            updated_at = now()
        FROM jsonb_to_recordset($2::jsonb) AS manifest(
            source_id TEXT,
            dialect_scope TEXT,
            normalization_version SMALLINT,
            source_revision BIGINT
        )
        WHERE source.entry_id = $1
          AND source.content_schema_version = 2
          AND source.content_scope = 'draft'
          AND source.source_id = manifest.source_id
          AND source.dialect_scope = manifest.dialect_scope
          AND source.normalization_version = manifest.normalization_version
        "#,
    )
    .bind(backup.entry_id)
    .bind(&backup.source_draft_surfaces)
    .bind(event_offset)
    .execute(&mut **tx)
    .await?;
    ensure!(
        restored_surfaces.rows_affected() == source_surface_count,
        "migration_rollback_surface_restore_mismatch: entry {}",
        backup.entry_id
    );
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.surface_projection', $2, $3,
            'lexicon.surface_projection_replaced', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(backup.entry_id)
    .bind(event_offset)
    .bind(serde_json::json!({
        "entry_id": backup.entry_id,
        "content_schema_version": 2,
        "content_scope": "draft",
        "publication_id": Option::<Uuid>::None,
        "source_revision": backup.source_revision,
        "event_offset": event_offset,
        "source_count": source_surface_count,
        "migration_batch_id": batch_id,
        "transition": "v3_to_v2",
    }))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM lexicon.nodes
        WHERE entry_id = $1
          AND id IN (
              SELECT v3_node_id
              FROM lexicon.v3_migration_map
              WHERE batch_id = $2 AND entry_id = $1
                AND role = 'group_membership'
          )
        "#,
    )
    .bind(backup.entry_id)
    .bind(batch_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM lexicon.nodes
        WHERE entry_id = $1
          AND id IN (
              SELECT v3_node_id
              FROM lexicon.v3_migration_map
              WHERE batch_id = $2 AND entry_id = $1
                AND role = 'synthetic_base_only_group'
          )
        "#,
    )
    .bind(backup.entry_id)
    .bind(batch_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE lexicon.nodes node
        SET node_type = 'pos', parent_node_id = NULL,
            node_role = 'forms.pos', stable_slot = FALSE,
            removed_from_draft_at = NULL
        FROM lexicon.entry_pos pos
        WHERE pos.id = node.id AND pos.entry_id = node.entry_id
          AND node.entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE lexicon.nodes node
        SET node_type = 'form_group', parent_node_id = form_group.entry_pos_id,
            node_role = 'forms.form_group', stable_slot = FALSE,
            removed_from_draft_at = NULL
        FROM lexicon.form_groups form_group
        WHERE form_group.id = node.id AND form_group.entry_id = node.entry_id
          AND node.entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE lexicon.nodes node
        SET node_type = 'form_slot',
            parent_node_id = COALESCE(slot.form_group_id, slot.entry_pos_id),
            node_role = CASE WHEN slot.form_type = 'base'
                THEN 'forms.base_form'
                ELSE 'forms.form_slot:' || slot.form_type END,
            stable_slot = TRUE, removed_from_draft_at = NULL
        FROM lexicon.form_slots slot
        WHERE slot.id = node.id AND slot.entry_id = node.entry_id
          AND node.entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE lexicon.nodes node
        SET node_type = 'form_variant', parent_node_id = variant.form_slot_id,
            node_role = 'forms.form_variant:' || variant.dialect,
            stable_slot = TRUE, removed_from_draft_at = NULL
        FROM lexicon.form_variants variant
        WHERE variant.id = node.id AND variant.entry_id = node.entry_id
          AND node.entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE lexicon.nodes node
        SET node_type = 'pronunciation', parent_node_id = pronunciation.form_variant_id,
            node_role = 'forms.pronunciation', stable_slot = FALSE,
            removed_from_draft_at = NULL
        FROM lexicon.pronunciations pronunciation
        WHERE pronunciation.id = node.id
          AND pronunciation.entry_id = node.entry_id
          AND node.entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;

    let source_pos_modes: Vec<SourcePosMode> =
        serde_json::from_value(backup.source_pos_modes.clone())?;
    let current_pos_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM lexicon.entry_pos WHERE entry_id = $1 ORDER BY id",
    )
    .bind(backup.entry_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let source_pos_ids = source_pos_modes
        .iter()
        .map(|pos| pos.pos_id)
        .collect::<BTreeSet<_>>();
    ensure!(
        current_pos_ids == source_pos_ids,
        "{ROLLBACK_BLOCKED_V3_WRITE}: entry {} POS identity changed",
        backup.entry_id
    );
    for pos in &source_pos_modes {
        sqlx::query(
            r#"
            UPDATE lexicon.entry_pos
            SET content_schema_version = 2, spelling_mode = $3,
                phonetic_mode = $4, sort_order = $5
            WHERE id = $1 AND entry_id = $2 AND content_schema_version = 3
            "#,
        )
        .bind(pos.pos_id)
        .bind(backup.entry_id)
        .bind(&pos.spelling_mode)
        .bind(&pos.phonetic_mode)
        .bind(pos.sort_order)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query("DELETE FROM lexicon.v3_entry_state WHERE entry_id = $1")
        .bind(backup.entry_id)
        .execute(&mut **tx)
        .await?;
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.entries
        SET content_schema_version = 2
        WHERE id = $1 AND content_schema_version = 3
          AND revision = $2
          AND current_publication_id IS NOT DISTINCT FROM $3
        "#,
    )
    .bind(backup.entry_id)
    .bind(backup.source_revision)
    .bind(backup.source_current_publication_id)
    .execute(&mut **tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "{ROLLBACK_BLOCKED_V3_WRITE}: entry {} changed during rollback",
        backup.entry_id
    );
    sqlx::query(
        r#"
        UPDATE lexicon.entry_editor_projection
        SET forms = $2, meanings = $3, rebuilt_revision = $4, updated_at = now()
        WHERE entry_id = $1
        "#,
    )
    .bind(backup.entry_id)
    .bind(&backup.source_forms)
    .bind(&backup.source_meanings)
    .bind(backup.source_revision)
    .execute(&mut **tx)
    .await?;
    ensure!(
        publication_fingerprint_tx(tx, backup.entry_id).await? == backup.source_publications_digest,
        "{ROLLBACK_BLOCKED_SOURCE_CHANGED}: entry {}",
        backup.entry_id
    );
    sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'rolled_back', rolled_back_at = now()
        WHERE batch_id = $1 AND entry_id = $2
          AND status IN ('applied', 'verified')
        "#,
    )
    .bind(batch_id)
    .bind(backup.entry_id)
    .execute(&mut **tx)
    .await?;
    insert_migration_audit(
        tx,
        actor_id,
        request_id,
        "lexicon.entry.rollback.v3_migration",
        backup.entry_id,
        backup.source_revision,
        batch_id,
    )
    .await?;
    Ok(())
}

pub async fn rollback(
    pool: &PgPool,
    batch_id: Uuid,
    actor_id: Uuid,
    request_id: Uuid,
) -> anyhow::Result<MigrationRollbackReport> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.rollback:{batch_id}"))
        .execute(&mut *tx)
        .await?;
    let locked_entry_ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT entry_id
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *tx)
    .await?;
    for entry_id in &locked_entry_ids {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
            .execute(&mut *tx)
            .await?;
    }
    let batch_status: String = sqlx::query_scalar(
        "SELECT status FROM lexicon.v3_migration_batches WHERE id = $1 FOR UPDATE",
    )
    .bind(batch_id)
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("migration_batch_not_found: {batch_id}"))?;
    if batch_status == "rolled_back" {
        let replay_identity = sqlx::query_as::<_, (Uuid, Uuid)>(
            r#"
            SELECT actor_admin_id, request_id
            FROM audit.admin_actions
            WHERE action = 'lexicon.migration_batch.rollback'
              AND resource_type = 'lexicon.migration_batch'
              AND resource_id = $1
            ORDER BY occurred_at DESC, id DESC
            LIMIT 1
            "#,
        )
        .bind(batch_id)
        .fetch_optional(&mut *tx)
        .await?
        .with_context(|| format!("migration_rollback_audit_missing: {batch_id}"))?;
        ensure!(
            replay_identity == (actor_id, request_id),
            "migration_rollback_idempotency_conflict: batch {batch_id}"
        );
        let replayed_entries = sqlx::query_as::<_, (Uuid, Vec<u8>)>(
            r#"
            SELECT entry_id, expected_digest
            FROM lexicon.v3_migration_entries
            WHERE batch_id = $1 AND status = 'rolled_back'
            ORDER BY entry_id
            "#,
        )
        .bind(batch_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(entry_id, digest)| MigrationEntryResult {
            entry_id,
            status: "rolled_back".to_owned(),
            digest: Some(hex_digest(&digest)),
            reason: None,
        })
        .collect::<Vec<_>>();
        tx.commit().await?;
        return Ok(MigrationRollbackReport {
            schema_version: 1,
            mode: "rollback",
            batch_id,
            rolled_back_entries: replayed_entries.len(),
            entries: replayed_entries,
        });
    }
    ensure!(
        matches!(
            batch_status.as_str(),
            "approved" | "applying" | "applied" | "verified" | "failed"
        ),
        "migration_batch_not_rollbackable: batch {batch_id} is {batch_status}"
    );
    ensure!(
        !locked_entry_ids.is_empty(),
        "migration_batch_is_empty: {batch_id}"
    );
    let current_entry_ids = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT entry_id
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *tx)
    .await?;
    ensure!(
        current_entry_ids == locked_entry_ids,
        "migration_batch_changed_during_rollback: {batch_id}"
    );
    if matches!(batch_status.as_str(), "approved" | "applying" | "failed") {
        sqlx::query(
            r#"
            UPDATE lexicon.v3_migration_entries
            SET status = 'failed', failure_code = 'rollback_aborted_unapplied'
            WHERE batch_id = $1 AND status = 'planned'
            "#,
        )
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
    }
    let planned_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_migration_entries WHERE batch_id = $1 AND status = 'planned'",
    )
    .bind(batch_id)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        planned_count == 0,
        "migration_batch_changed_during_rollback: batch {batch_id} still has planned entries"
    );
    let backups = sqlx::query_as::<_, MigrationBackupRow>(
        r#"
        SELECT entry_id, status, source_revision, source_current_publication_id,
               source_publications_digest, source_pos_modes, source_forms,
               source_meanings, source_draft_surfaces, expected_forms,
               expected_presentation,
               expected_digest, applied_digest
        FROM lexicon.v3_migration_entries
        WHERE batch_id = $1 AND status IN ('applied', 'verified')
        ORDER BY entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&mut *tx)
    .await?;
    for backup in &backups {
        rollback_preflight(&mut tx, batch_id, backup).await?;
    }
    let mut entries = Vec::with_capacity(backups.len());
    for backup in &backups {
        rollback_entry(&mut tx, batch_id, actor_id, request_id, backup).await?;
        entries.push(MigrationEntryResult {
            entry_id: backup.entry_id,
            status: "rolled_back".to_owned(),
            digest: Some(hex_digest(&backup.expected_digest)),
            reason: None,
        });
    }
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_batches
        SET status = 'rolled_back', finished_at = now()
        WHERE id = $1 AND status = $2
        "#,
    )
    .bind(batch_id)
    .bind(&batch_status)
    .execute(&mut *tx)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "migration_batch_changed_during_rollback: {batch_id}"
    );
    insert_migration_batch_audit(
        &mut tx,
        actor_id,
        request_id,
        "lexicon.migration_batch.rollback",
        batch_id,
        serde_json::json!({
            "source_schema_version": 3,
            "target_schema_version": 2,
            "rolled_back_entries": entries.len(),
            "rolled_back_entry_ids": entries
                .iter()
                .map(|entry| entry.entry_id)
                .collect::<Vec<_>>(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(MigrationRollbackReport {
        schema_version: 1,
        mode: "rollback",
        batch_id,
        rolled_back_entries: entries.len(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::{ApplyFailureCheckpoint, classify_apply_failure_checkpoint};
    use uuid::Uuid;

    #[test]
    fn committed_apply_checkpoint_wins_over_an_ambiguous_commit_error() {
        let digest = [0x5a; 32];
        let checkpoint = classify_apply_failure_checkpoint(
            "applied",
            &digest,
            Some(&digest),
            Uuid::nil(),
            Uuid::nil(),
        )
        .unwrap();
        assert_eq!(checkpoint, ApplyFailureCheckpoint::AlreadyApplied);
    }

    #[test]
    fn ambiguous_apply_checkpoint_fails_closed_on_digest_mismatch() {
        let error = classify_apply_failure_checkpoint(
            "applied",
            &[0x5a; 32],
            Some(&[0xa5; 32]),
            Uuid::nil(),
            Uuid::nil(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("migration_entry_digest_mismatch_after_apply")
        );
    }
}
