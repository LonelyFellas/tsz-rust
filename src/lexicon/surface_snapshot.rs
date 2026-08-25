use std::{collections::HashMap, time::Duration};

use chrono::Utc;
use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    lexicon::{
        dto::{
            LexiconSurfaceMatchV2, MatchedEntryContextV2, MatchedEntryContextV3,
            SurfaceConfirmationReasonV2, SurfaceContinuationDisabledV2,
            SurfaceContinuationEnabledV2, SurfaceMatchEnabledNextPageV2,
            SurfaceMatchEnabledNextPageV3, SurfaceMatchEnabledTerminalPageV2,
            SurfaceMatchEnabledTerminalPageV3, SurfaceMatchItemV3, SurfaceMatchPageAny,
            SurfaceMatchPageBaseV2, SurfaceMatchPageBaseV3, SurfaceMatchPageV2, SurfaceMatchPageV3,
            SurfaceMatchTemporarilyDisabledPageV2, SurfaceMatchTemporarilyDisabledPageV3,
            SurfacePolicyBlockCodeV2, SurfacePolicyNameV2,
        },
        surface_policy::{SURFACE_POLICY_PREFIX, SurfaceCreationPolicy},
    },
    platform::{generate_token_plaintext, hash_token},
};

const SNAPSHOT_PREFIX: &str = "lexicon:surface-snapshot:";
const ACTIVE_PREFIX: &str = "lexicon:surface-snapshot-active:";
const TOKEN_PREFIX: &str = "lexicon:surface-confirmation-token:";
const IMPACT_TOKEN_PREFIX: &str = "lexicon:surface-impact-confirmation-token:";
pub const DEFAULT_SURFACE_PAGE_SIZE: usize = 20;
pub const MAX_SURFACE_PAGE_SIZE: usize = 50;
pub const DEFAULT_SURFACE_IDLE_TTL: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_SURFACE_TOKEN_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceConsumptionCommand {
    CreateEntry,
    SaveForms,
    PublishEntry,
    RestoreEntry,
    RestoreEntriesBatch,
    ActivatePublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceConfirmationBinding {
    pub actor_id: Uuid,
    pub command: SurfaceConsumptionCommand,
    pub owner_context: String,
    pub base_revision: Option<i64>,
    pub canonical_content_digest: String,
    pub owner_evidence_digest: String,
    pub normalization_version: i16,
    pub policy_name: SurfacePolicyNameV2,
    pub policy_epoch: u64,
}

#[derive(Debug, Clone)]
pub struct CreateSurfaceSnapshot {
    pub binding: SurfaceConfirmationBinding,
    pub policy_enabled: bool,
    pub policy_block_code: Option<SurfacePolicyBlockCodeV2>,
    pub items: Vec<LexiconSurfaceMatchV2>,
    pub matched_entry_contexts: Vec<MatchedEntryContextV2>,
    pub confirmation_reasons: Vec<SurfaceConfirmationReasonV2>,
    pub owner_bundle: Value,
    pub page_size: usize,
}

#[derive(Debug, Clone)]
pub struct CreatedSurfaceSnapshot {
    pub snapshot_id: Uuid,
    pub page: SurfaceMatchPageV2,
}

/// V3 page material is stored inside the immutable owner bundle while the
/// existing snapshot engine continues to use its battle-tested V2 membership
/// records for ordering, cursor advancement and token digests. The synthetic
/// V2 records never cross the HTTP boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct V3SurfaceSnapshotItem {
    pub match_id: String,
    pub item: SurfaceMatchItemV3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct V3SurfaceSnapshotPageData {
    pub items: Vec<V3SurfaceSnapshotItem>,
    pub matched_entry_contexts: Vec<MatchedEntryContextV3>,
}

pub(crate) const V3_SURFACE_PAGE_DATA_KEY: &str = "v3_surface_page_data";

#[derive(Debug, Clone)]
pub struct ExpectedSurfaceConfirmation {
    pub binding: SurfaceConfirmationBinding,
    pub current_policy: SurfaceCreationPolicy,
}

#[derive(Debug, Clone)]
pub struct ExpectedSurfaceOwner {
    pub actor_id: Uuid,
    pub command: SurfaceConsumptionCommand,
    pub owner_context: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedSurfaceConfirmation {
    pub snapshot_id: Uuid,
    pub binding: SurfaceConfirmationBinding,
    pub match_ids: Vec<String>,
    pub match_digest: String,
    pub context_digest: String,
    pub owner_bundle: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SurfaceSnapshotError {
    #[error("surface snapshot input is invalid: {0}")]
    InvalidInput(&'static str),
    #[error("surface snapshot expired")]
    Expired,
    #[error("surface snapshot cursor is invalid or was already used")]
    InvalidCursor,
    #[error("surface confirmation token binding does not match")]
    BindingMismatch,
    #[error("surface policy changed")]
    PolicyChanged(SurfacePolicyNameV2),
    #[error(transparent)]
    Pool(#[from] deadpool_redis::PoolError),
    #[error(transparent)]
    Redis(#[from] deadpool_redis::redis::RedisError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SurfaceSnapshotBundle {
    snapshot_id: Uuid,
    binding: SurfaceConfirmationBinding,
    policy_enabled: bool,
    policy_block_code: Option<SurfacePolicyBlockCodeV2>,
    items: Vec<LexiconSurfaceMatchV2>,
    contexts: Vec<MatchedEntryContextV2>,
    confirmation_reasons: Vec<SurfaceConfirmationReasonV2>,
    owner_bundle: Value,
    page_size: usize,
    next_offset: usize,
    next_cursor_digest: Option<String>,
    terminal_token_digest: Option<String>,
    #[serde(default)]
    issue_impact_confirmation_token: bool,
    #[serde(default)]
    terminal_impact_token_digest: Option<String>,
    terminal_token_expires_at_ms: Option<i64>,
    lease_expires_at_ms: i64,
    match_digest: String,
    #[serde(default)]
    context_digest: String,
    active_context_digest: String,
}

#[derive(Debug)]
struct InitializedSnapshot {
    bundle: SurfaceSnapshotBundle,
    page: SurfaceMatchPageV2,
    terminal_token_digest: Option<String>,
    terminal_impact_token_digest: Option<String>,
}

#[derive(Clone)]
pub struct SurfaceSnapshotStore {
    redis: Pool,
    idle_ttl: Duration,
    token_ttl: Duration,
    policy_prefix: String,
}

pub fn surface_owner_bundle_digest(owner_bundle: &Value) -> Result<String, serde_json::Error> {
    Ok(hash_token(&serde_json::to_string(owner_bundle)?))
}

impl SurfaceSnapshotStore {
    pub fn with_defaults(redis: Pool) -> Self {
        Self::new(redis, DEFAULT_SURFACE_IDLE_TTL, DEFAULT_SURFACE_TOKEN_TTL)
    }

    pub fn new(redis: Pool, idle_ttl: Duration, token_ttl: Duration) -> Self {
        Self {
            redis,
            idle_ttl,
            token_ttl,
            policy_prefix: SURFACE_POLICY_PREFIX.to_owned(),
        }
    }

    pub(crate) fn with_policy_prefix(redis: Pool, policy_prefix: String) -> Self {
        Self {
            redis,
            idle_ttl: DEFAULT_SURFACE_IDLE_TTL,
            token_ttl: DEFAULT_SURFACE_TOKEN_TTL,
            policy_prefix,
        }
    }

    pub async fn create(
        &self,
        input: CreateSurfaceSnapshot,
    ) -> Result<CreatedSurfaceSnapshot, SurfaceSnapshotError> {
        self.create_internal(input, false).await
    }

    /// Create a Forms-owned snapshot whose enabled terminal page signs both
    /// the surface acknowledgement and the downstream-impact acknowledgement.
    /// The impact token is a UUID so it can be passed directly through
    /// `SaveFormsStepInput.confirmed_impact_token`.
    pub async fn create_with_impact_confirmation(
        &self,
        input: CreateSurfaceSnapshot,
    ) -> Result<CreatedSurfaceSnapshot, SurfaceSnapshotError> {
        self.create_internal(input, true).await
    }

    async fn create_internal(
        &self,
        input: CreateSurfaceSnapshot,
        issue_impact_confirmation_token: bool,
    ) -> Result<CreatedSurfaceSnapshot, SurfaceSnapshotError> {
        let now_ms = Utc::now().timestamp_millis();
        let initialized = initialize_snapshot(
            input,
            now_ms,
            self.idle_ttl,
            self.token_ttl,
            generate_token_plaintext(),
            generate_token_plaintext(),
            issue_impact_confirmation_token.then(Uuid::now_v7),
        )?;
        let snapshot_id = initialized.bundle.snapshot_id;
        let snapshot_key = snapshot_key(snapshot_id);
        let active_key = active_key(&initialized.bundle.active_context_digest);
        let terminal_token_key = initialized
            .terminal_token_digest
            .as_deref()
            .map(token_key)
            .unwrap_or_else(|| format!("{TOKEN_PREFIX}unused:{snapshot_id}"));
        let terminal_impact_token_key = initialized
            .terminal_impact_token_digest
            .as_deref()
            .map(impact_token_key)
            .unwrap_or_else(|| format!("{IMPACT_TOKEN_PREFIX}unused:{snapshot_id}"));
        let ttl_ms = remaining_bundle_ttl_ms(&initialized.bundle, now_ms)?;
        let token_ttl_ms = initialized
            .bundle
            .terminal_token_expires_at_ms
            .map(|expires| expires.saturating_sub(now_ms).max(1))
            .unwrap_or(1);
        let payload = serde_json::to_string(&initialized.bundle)?;

        let mut connection = self.redis.get().await?;
        let result: Vec<String> = deadpool_redis::redis::Script::new(
            r#"
            local bundle = cjson.decode(ARGV[1])
            local policy_payload = redis.call('GET', ARGV[10] .. bundle.binding.policy_name)
            if not policy_payload then return {'policy', bundle.binding.policy_name} end
            local policy = cjson.decode(policy_payload)
            if policy.name ~= bundle.binding.policy_name
               or tonumber(policy.epoch) ~= tonumber(bundle.binding.policy_epoch)
               or policy.enabled ~= bundle.policy_enabled then
                return {'policy', bundle.binding.policy_name}
            end
            local old_snapshot_id = redis.call('GET', KEYS[2])
            if old_snapshot_id then
                local old_key = ARGV[6] .. old_snapshot_id
                local old_payload = redis.call('GET', old_key)
                if old_payload then
                    local old = cjson.decode(old_payload)
                    if old.terminal_token_digest
                       and old.terminal_token_digest ~= cjson.null then
                        redis.call('DEL', ARGV[7] .. old.terminal_token_digest)
                    end
                    if old.terminal_impact_token_digest
                       and old.terminal_impact_token_digest ~= cjson.null then
                        redis.call('DEL', ARGV[8] .. old.terminal_impact_token_digest)
                    end
                end
                redis.call('DEL', old_key)
            end
            redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
            redis.call('SET', KEYS[2], ARGV[3], 'PX', ARGV[2])
            if ARGV[4] == '1' then
                redis.call('SET', KEYS[3], ARGV[3], 'PX', ARGV[9])
            end
            if ARGV[5] == '1' then
                redis.call('SET', KEYS[4], ARGV[3], 'PX', ARGV[9])
            end
            return {'ok'}
            "#,
        )
        .key(snapshot_key)
        .key(active_key)
        .key(terminal_token_key)
        .key(terminal_impact_token_key)
        .arg(payload)
        .arg(ttl_ms)
        .arg(snapshot_id.to_string())
        .arg(if initialized.terminal_token_digest.is_some() {
            "1"
        } else {
            "0"
        })
        .arg(if initialized.terminal_impact_token_digest.is_some() {
            "1"
        } else {
            "0"
        })
        .arg(SNAPSHOT_PREFIX)
        .arg(TOKEN_PREFIX)
        .arg(IMPACT_TOKEN_PREFIX)
        .arg(token_ttl_ms)
        .arg(&self.policy_prefix)
        .invoke_async(&mut connection)
        .await?;

        match result.first().map(String::as_str) {
            Some("ok") => {}
            Some("policy") if result.len() == 2 => {
                return Err(SurfaceSnapshotError::PolicyChanged(parse_policy_name(
                    &result[1],
                )?));
            }
            _ => {
                return Err(SurfaceSnapshotError::InvalidInput(
                    "invalid Redis snapshot create response",
                ));
            }
        }

        Ok(CreatedSurfaceSnapshot {
            snapshot_id,
            page: initialized.page,
        })
    }

    pub async fn page(
        &self,
        actor_id: Uuid,
        snapshot_id: Uuid,
        cursor: &str,
    ) -> Result<SurfaceMatchPageAny, SurfaceSnapshotError> {
        let next_cursor = generate_token_plaintext();
        let terminal_token = generate_token_plaintext();
        let terminal_impact_token = Uuid::now_v7();
        let supplied_cursor_digest = hash_token(cursor);
        let next_cursor_digest = hash_token(&next_cursor);
        let terminal_token_digest = hash_token(&terminal_token);
        let terminal_impact_token_digest = hash_token(&terminal_impact_token.to_string());
        let snapshot_key = snapshot_key(snapshot_id);
        let terminal_key = token_key(&terminal_token_digest);
        let terminal_impact_key = impact_token_key(&terminal_impact_token_digest);
        let idle_ttl_ms = duration_ms(self.idle_ttl)?;
        let token_ttl_ms = duration_ms(self.token_ttl)?;

        let mut connection = self.redis.get().await?;
        let result: Vec<String> = deadpool_redis::redis::Script::new(
            r#"
            local payload = redis.call('GET', KEYS[1])
            if not payload then return {'expired'} end
            local bundle = cjson.decode(payload)
            local policy_payload = redis.call('GET', ARGV[10] .. bundle.binding.policy_name)
            if not policy_payload then return {'policy', bundle.binding.policy_name} end
            local policy = cjson.decode(policy_payload)
            if policy.name ~= bundle.binding.policy_name
               or tonumber(policy.epoch) ~= tonumber(bundle.binding.policy_epoch)
               or policy.enabled ~= bundle.policy_enabled then
                return {'policy', bundle.binding.policy_name}
            end
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
            if bundle.binding.actor_id ~= ARGV[1] then return {'expired'} end
            if tonumber(bundle.lease_expires_at_ms) <= now_ms then
                redis.call('DEL', KEYS[1])
                return {'expired'}
            end
            if bundle.next_cursor_digest ~= ARGV[2] then return {'cursor'} end

            local start_offset = tonumber(bundle.next_offset)
            local end_offset = math.min(start_offset + tonumber(bundle.page_size), #bundle.items)
            bundle.next_offset = end_offset
            local is_terminal = end_offset >= #bundle.items
            local ttl_ms
            if is_terminal and bundle.policy_enabled then
                bundle.next_cursor_digest = nil
                bundle.terminal_token_digest = ARGV[4]
                if bundle.issue_impact_confirmation_token then
                    bundle.terminal_impact_token_digest = ARGV[5]
                end
                bundle.terminal_token_expires_at_ms = now_ms + tonumber(ARGV[7])
                bundle.lease_expires_at_ms = bundle.terminal_token_expires_at_ms
                ttl_ms = tonumber(ARGV[7])
                redis.call('SET', KEYS[2], ARGV[8], 'PX', ttl_ms)
                if bundle.issue_impact_confirmation_token then
                    redis.call('SET', KEYS[3], ARGV[8], 'PX', ttl_ms)
                end
            elseif is_terminal then
                bundle.next_cursor_digest = nil
                bundle.lease_expires_at_ms = now_ms + tonumber(ARGV[6])
                ttl_ms = tonumber(ARGV[6])
            else
                bundle.next_cursor_digest = ARGV[3]
                bundle.lease_expires_at_ms = now_ms + tonumber(ARGV[6])
                ttl_ms = tonumber(ARGV[6])
            end
            local updated = cjson.encode(bundle)
            redis.call('SET', KEYS[1], updated, 'PX', ttl_ms)
            redis.call('PEXPIRE', ARGV[9] .. bundle.active_context_digest, ttl_ms)
            return {'ok', updated, tostring(start_offset), tostring(end_offset), is_terminal and '1' or '0'}
            "#,
        )
        .key(snapshot_key)
        .key(terminal_key)
        .key(terminal_impact_key)
        .arg(actor_id.to_string())
        .arg(supplied_cursor_digest)
        .arg(next_cursor_digest)
        .arg(terminal_token_digest)
        .arg(terminal_impact_token_digest)
        .arg(idle_ttl_ms)
        .arg(token_ttl_ms)
        .arg(snapshot_id.to_string())
        .arg(ACTIVE_PREFIX)
        .arg(&self.policy_prefix)
        .invoke_async(&mut connection)
        .await?;

        match result.first().map(String::as_str) {
            Some("expired") => Err(SurfaceSnapshotError::Expired),
            Some("cursor") => Err(SurfaceSnapshotError::InvalidCursor),
            Some("policy") if result.len() == 2 => Err(SurfaceSnapshotError::PolicyChanged(
                parse_policy_name(&result[1])?,
            )),
            Some("ok") if result.len() == 5 => {
                let bundle: SurfaceSnapshotBundle = serde_json::from_str(&result[1])?;
                let start = result[2]
                    .parse::<usize>()
                    .map_err(|_| SurfaceSnapshotError::InvalidInput("invalid Redis page start"))?;
                let end = result[3]
                    .parse::<usize>()
                    .map_err(|_| SurfaceSnapshotError::InvalidInput("invalid Redis page end"))?;
                let terminal = result[4] == "1";
                let page = render_page(
                    &bundle,
                    start,
                    end,
                    (!terminal).then_some(next_cursor),
                    (terminal && bundle.policy_enabled).then_some(terminal_token),
                    (terminal && bundle.policy_enabled && bundle.issue_impact_confirmation_token)
                        .then_some(terminal_impact_token),
                );
                surface_page_any(page, &bundle.owner_bundle)
            }
            _ => Err(SurfaceSnapshotError::InvalidInput(
                "invalid Redis snapshot response",
            )),
        }
    }

    pub async fn verify(
        &self,
        token: &str,
        expected: &ExpectedSurfaceConfirmation,
    ) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
        let token_digest = hash_token(token);
        let mut connection = self.redis.get().await?;
        let result: Vec<String> = deadpool_redis::redis::Script::new(
            r#"
            local snapshot_id = redis.call('GET', KEYS[1])
            if not snapshot_id then return {'expired'} end
            local payload = redis.call('GET', ARGV[3] .. snapshot_id)
            if not payload then return {'expired'} end
            local bundle = cjson.decode(payload)
            local policy_payload = redis.call('GET', ARGV[4] .. bundle.binding.policy_name)
            if not policy_payload then return {'policy', bundle.binding.policy_name} end
            local policy = cjson.decode(policy_payload)
            if policy.name ~= bundle.binding.policy_name
               or tonumber(policy.epoch) ~= tonumber(bundle.binding.policy_epoch)
               or not policy.enabled then
                return {'policy', bundle.binding.policy_name}
            end
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
            if bundle.binding.actor_id ~= ARGV[1] then return {'expired'} end
            if bundle.terminal_token_digest ~= ARGV[2] then return {'expired'} end
            if not bundle.terminal_token_expires_at_ms
               or tonumber(bundle.terminal_token_expires_at_ms) <= now_ms then
                return {'expired'}
            end
            return {'ok', payload, tostring(now_ms)}
            "#,
        )
        .key(token_key(&token_digest))
        .arg(expected.binding.actor_id.to_string())
        .arg(&token_digest)
        .arg(SNAPSHOT_PREFIX)
        .arg(&self.policy_prefix)
        .invoke_async(&mut connection)
        .await?;

        match result.first().map(String::as_str) {
            Some("expired") => Err(SurfaceSnapshotError::Expired),
            Some("policy") if result.len() == 2 => Err(SurfaceSnapshotError::PolicyChanged(
                parse_policy_name(&result[1])?,
            )),
            Some("ok") if result.len() == 3 => {
                let bundle: SurfaceSnapshotBundle = serde_json::from_str(&result[1])?;
                let now_ms = result[2]
                    .parse::<i64>()
                    .map_err(|_| SurfaceSnapshotError::InvalidInput("invalid Redis clock"))?;
                verify_bundle(&bundle, &token_digest, expected, now_ms)
            }
            _ => Err(SurfaceSnapshotError::InvalidInput(
                "invalid Redis token response",
            )),
        }
    }

    /// Verify the UUID companion token issued on a Forms terminal page. The
    /// impact and surface tokens have distinct Redis namespaces and bundle
    /// digests, so neither can be substituted for the other.
    pub async fn verify_impact(
        &self,
        token: Uuid,
        expected: &ExpectedSurfaceOwner,
    ) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
        let token_digest = hash_token(&token.to_string());
        let mut connection = self.redis.get().await?;
        let result: Vec<String> = deadpool_redis::redis::Script::new(
            r#"
            local snapshot_id = redis.call('GET', KEYS[1])
            if not snapshot_id then return {'expired'} end
            local payload = redis.call('GET', ARGV[3] .. snapshot_id)
            if not payload then return {'expired'} end
            local bundle = cjson.decode(payload)
            local policy_payload = redis.call('GET', ARGV[4] .. bundle.binding.policy_name)
            if not policy_payload then return {'policy', bundle.binding.policy_name} end
            local policy = cjson.decode(policy_payload)
            if policy.name ~= bundle.binding.policy_name
               or tonumber(policy.epoch) ~= tonumber(bundle.binding.policy_epoch)
               or not policy.enabled then
                return {'policy', bundle.binding.policy_name}
            end
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
            if bundle.binding.actor_id ~= ARGV[1] then return {'expired'} end
            if not bundle.issue_impact_confirmation_token
               or bundle.terminal_impact_token_digest ~= ARGV[2] then
                return {'expired'}
            end
            if not bundle.terminal_token_expires_at_ms
               or tonumber(bundle.terminal_token_expires_at_ms) <= now_ms then
                return {'expired'}
            end
            return {'ok', payload, tostring(now_ms)}
            "#,
        )
        .key(impact_token_key(&token_digest))
        .arg(expected.actor_id.to_string())
        .arg(&token_digest)
        .arg(SNAPSHOT_PREFIX)
        .arg(&self.policy_prefix)
        .invoke_async(&mut connection)
        .await?;

        match result.first().map(String::as_str) {
            Some("expired") => Err(SurfaceSnapshotError::Expired),
            Some("policy") if result.len() == 2 => Err(SurfaceSnapshotError::PolicyChanged(
                parse_policy_name(&result[1])?,
            )),
            Some("ok") if result.len() == 3 => {
                let bundle: SurfaceSnapshotBundle = serde_json::from_str(&result[1])?;
                let now_ms = result[2]
                    .parse::<i64>()
                    .map_err(|_| SurfaceSnapshotError::InvalidInput("invalid Redis clock"))?;
                verify_impact_owner_bundle(&bundle, &token_digest, expected, now_ms)
            }
            _ => Err(SurfaceSnapshotError::InvalidInput(
                "invalid Redis impact token response",
            )),
        }
    }

    /// Verify an opaque terminal token and recover its immutable owner bundle
    /// without relying on the shorter-lived detection payload. Policy state is
    /// still checked atomically from the binding stored in the bundle.
    pub async fn verify_owner(
        &self,
        token: &str,
        expected: &ExpectedSurfaceOwner,
    ) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
        let token_digest = hash_token(token);
        let mut connection = self.redis.get().await?;
        let result: Vec<String> = deadpool_redis::redis::Script::new(
            r#"
            local snapshot_id = redis.call('GET', KEYS[1])
            if not snapshot_id then return {'expired'} end
            local payload = redis.call('GET', ARGV[3] .. snapshot_id)
            if not payload then return {'expired'} end
            local bundle = cjson.decode(payload)
            local policy_payload = redis.call('GET', ARGV[4] .. bundle.binding.policy_name)
            if not policy_payload then return {'policy', bundle.binding.policy_name} end
            local policy = cjson.decode(policy_payload)
            if policy.name ~= bundle.binding.policy_name
               or tonumber(policy.epoch) ~= tonumber(bundle.binding.policy_epoch)
               or not policy.enabled then
                return {'policy', bundle.binding.policy_name}
            end
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
            if bundle.binding.actor_id ~= ARGV[1] then return {'expired'} end
            if bundle.terminal_token_digest ~= ARGV[2] then return {'expired'} end
            if not bundle.terminal_token_expires_at_ms
               or tonumber(bundle.terminal_token_expires_at_ms) <= now_ms then
                return {'expired'}
            end
            return {'ok', payload, tostring(now_ms)}
            "#,
        )
        .key(token_key(&token_digest))
        .arg(expected.actor_id.to_string())
        .arg(&token_digest)
        .arg(SNAPSHOT_PREFIX)
        .arg(&self.policy_prefix)
        .invoke_async(&mut connection)
        .await?;

        match result.first().map(String::as_str) {
            Some("expired") => Err(SurfaceSnapshotError::Expired),
            Some("policy") if result.len() == 2 => Err(SurfaceSnapshotError::PolicyChanged(
                parse_policy_name(&result[1])?,
            )),
            Some("ok") if result.len() == 3 => {
                let bundle: SurfaceSnapshotBundle = serde_json::from_str(&result[1])?;
                let now_ms = result[2]
                    .parse::<i64>()
                    .map_err(|_| SurfaceSnapshotError::InvalidInput("invalid Redis clock"))?;
                verify_owner_bundle(&bundle, &token_digest, expected, now_ms)
            }
            _ => Err(SurfaceSnapshotError::InvalidInput(
                "invalid Redis token response",
            )),
        }
    }

    /// Best-effort cleanup after the owning PostgreSQL command commits. The
    /// durable command transaction remains authoritative; failure here is safe
    /// because the opaque token and bundle still expire naturally.
    pub async fn remove_verified(
        &self,
        confirmation: &VerifiedSurfaceConfirmation,
    ) -> Result<(), SurfaceSnapshotError> {
        let mut connection = self.redis.get().await?;
        deadpool_redis::redis::Script::new(
            r#"
            local payload = redis.call('GET', KEYS[1])
            if not payload then return 0 end
            local bundle = cjson.decode(payload)
            if bundle.binding.actor_id ~= ARGV[1]
               or bundle.active_context_digest ~= ARGV[2] then
                return 0
            end
            local active_key = ARGV[3] .. bundle.active_context_digest
            if redis.call('GET', active_key) == ARGV[4] then
                redis.call('DEL', active_key)
            end
            if bundle.terminal_token_digest
               and bundle.terminal_token_digest ~= cjson.null then
                redis.call('DEL', ARGV[5] .. bundle.terminal_token_digest)
            end
            if bundle.terminal_impact_token_digest
               and bundle.terminal_impact_token_digest ~= cjson.null then
                redis.call('DEL', ARGV[6] .. bundle.terminal_impact_token_digest)
            end
            redis.call('DEL', KEYS[1])
            return 1
            "#,
        )
        .key(snapshot_key(confirmation.snapshot_id))
        .arg(confirmation.binding.actor_id.to_string())
        .arg(active_context_digest(&confirmation.binding))
        .arg(ACTIVE_PREFIX)
        .arg(confirmation.snapshot_id.to_string())
        .arg(TOKEN_PREFIX)
        .arg(IMPACT_TOKEN_PREFIX)
        .invoke_async::<i64>(&mut connection)
        .await?;
        Ok(())
    }
}

fn verify_owner_bundle(
    bundle: &SurfaceSnapshotBundle,
    token_digest: &str,
    expected: &ExpectedSurfaceOwner,
    now_ms: i64,
) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
    if bundle.terminal_token_digest.as_deref() != Some(token_digest)
        || bundle
            .terminal_token_expires_at_ms
            .is_none_or(|expires| expires <= now_ms)
    {
        return Err(SurfaceSnapshotError::Expired);
    }
    if bundle.binding.actor_id != expected.actor_id
        || bundle.binding.command != expected.command
        || bundle.binding.owner_context != expected.owner_context
    {
        return Err(SurfaceSnapshotError::BindingMismatch);
    }
    Ok(verified_confirmation(bundle))
}

fn verified_confirmation(bundle: &SurfaceSnapshotBundle) -> VerifiedSurfaceConfirmation {
    VerifiedSurfaceConfirmation {
        snapshot_id: bundle.snapshot_id,
        binding: bundle.binding.clone(),
        match_ids: bundle
            .items
            .iter()
            .map(|item| item.match_id.clone())
            .collect(),
        match_digest: bundle.match_digest.clone(),
        context_digest: bundle.context_digest.clone(),
        owner_bundle: bundle.owner_bundle.clone(),
    }
}

fn initialize_snapshot(
    mut input: CreateSurfaceSnapshot,
    now_ms: i64,
    idle_ttl: Duration,
    token_ttl: Duration,
    next_cursor: String,
    terminal_token: String,
    terminal_impact_token: Option<Uuid>,
) -> Result<InitializedSnapshot, SurfaceSnapshotError> {
    if terminal_impact_token.is_some()
        && input.binding.command != SurfaceConsumptionCommand::SaveForms
    {
        return Err(SurfaceSnapshotError::InvalidInput(
            "impact confirmation token is only valid for save_forms snapshots",
        ));
    }
    validate_input(&input)?;
    for item in &mut input.items {
        item.confirmation_reasons.sort_by_key(reason_order);
        item.confirmation_reasons.dedup();
    }
    input.items.sort_by(|left, right| {
        left.match_id.cmp(&right.match_id).then_with(|| {
            reason_digest(&left.confirmation_reasons)
                .cmp(&reason_digest(&right.confirmation_reasons))
        })
    });
    input.confirmation_reasons.sort_by_key(reason_order);
    input.confirmation_reasons.dedup();

    let snapshot_id = Uuid::now_v7();
    let page_size = input.page_size.min(input.items.len());
    let terminal = page_size == input.items.len();
    let next_cursor_digest = (!terminal).then(|| hash_token(&next_cursor));
    let terminal_token_digest =
        (terminal && input.policy_enabled).then(|| hash_token(&terminal_token));
    let issue_impact_confirmation_token = terminal_impact_token.is_some();
    let terminal_impact_token_digest = terminal_impact_token
        .as_ref()
        .filter(|_| terminal && input.policy_enabled)
        .map(|token| hash_token(&token.to_string()));
    let token_expires_at = terminal_token_digest
        .as_ref()
        .map(|_| add_duration(now_ms, token_ttl))
        .transpose()?;
    let lease_expires_at_ms = token_expires_at.unwrap_or(add_duration(now_ms, idle_ttl)?);
    let match_digest = surface_match_digest(&input.items, &input.confirmation_reasons)?;
    let context_digest = surface_context_digest(&input.matched_entry_contexts)?;
    let active_context_digest = active_context_digest(&input.binding);
    let bundle = SurfaceSnapshotBundle {
        snapshot_id,
        binding: input.binding,
        policy_enabled: input.policy_enabled,
        policy_block_code: input.policy_block_code,
        items: input.items,
        contexts: input.matched_entry_contexts,
        confirmation_reasons: input.confirmation_reasons,
        owner_bundle: input.owner_bundle,
        page_size: input.page_size,
        next_offset: page_size,
        next_cursor_digest,
        terminal_token_digest: terminal_token_digest.clone(),
        issue_impact_confirmation_token,
        terminal_impact_token_digest: terminal_impact_token_digest.clone(),
        terminal_token_expires_at_ms: token_expires_at,
        lease_expires_at_ms,
        match_digest,
        context_digest,
        active_context_digest,
    };
    let page = render_page(
        &bundle,
        0,
        page_size,
        (!terminal).then_some(next_cursor),
        (terminal && bundle.policy_enabled).then_some(terminal_token),
        terminal_impact_token.filter(|_| terminal && bundle.policy_enabled),
    );
    Ok(InitializedSnapshot {
        bundle,
        page,
        terminal_token_digest,
        terminal_impact_token_digest,
    })
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "pure transition mirrors all Redis page-advance inputs for fake-clock fault tests"
)]
fn advance_bundle(
    bundle: &mut SurfaceSnapshotBundle,
    actor_id: Uuid,
    cursor: &str,
    now_ms: i64,
    idle_ttl: Duration,
    token_ttl: Duration,
    next_cursor: String,
    terminal_token: String,
    terminal_impact_token: Uuid,
) -> Result<SurfaceMatchPageV2, SurfaceSnapshotError> {
    if bundle.binding.actor_id != actor_id || bundle.lease_expires_at_ms <= now_ms {
        return Err(SurfaceSnapshotError::Expired);
    }
    if bundle.next_cursor_digest.as_deref() != Some(hash_token(cursor).as_str()) {
        return Err(SurfaceSnapshotError::InvalidCursor);
    }

    let start = bundle.next_offset;
    let end = (start + bundle.page_size).min(bundle.items.len());
    let terminal = end == bundle.items.len();
    bundle.next_offset = end;
    let (next_cursor, terminal_token, terminal_impact_token) = if terminal && bundle.policy_enabled
    {
        bundle.next_cursor_digest = None;
        bundle.terminal_token_digest = Some(hash_token(&terminal_token));
        let terminal_impact_token = bundle.issue_impact_confirmation_token.then(|| {
            bundle.terminal_impact_token_digest =
                Some(hash_token(&terminal_impact_token.to_string()));
            terminal_impact_token
        });
        let expires = add_duration(now_ms, token_ttl)?;
        bundle.terminal_token_expires_at_ms = Some(expires);
        bundle.lease_expires_at_ms = expires;
        (None, Some(terminal_token), terminal_impact_token)
    } else if terminal {
        bundle.next_cursor_digest = None;
        bundle.lease_expires_at_ms = add_duration(now_ms, idle_ttl)?;
        (None, None, None)
    } else {
        bundle.next_cursor_digest = Some(hash_token(&next_cursor));
        bundle.lease_expires_at_ms = add_duration(now_ms, idle_ttl)?;
        (Some(next_cursor), None, None)
    };
    Ok(render_page(
        bundle,
        start,
        end,
        next_cursor,
        terminal_token,
        terminal_impact_token,
    ))
}

fn verify_bundle(
    bundle: &SurfaceSnapshotBundle,
    token_digest: &str,
    expected: &ExpectedSurfaceConfirmation,
    now_ms: i64,
) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
    if bundle.terminal_token_digest.as_deref() != Some(token_digest)
        || bundle
            .terminal_token_expires_at_ms
            .is_none_or(|expires| expires <= now_ms)
    {
        return Err(SurfaceSnapshotError::Expired);
    }
    if bundle.binding.policy_name != expected.current_policy.name
        || bundle.binding.policy_epoch != expected.current_policy.epoch
        || !expected.current_policy.enabled
    {
        return Err(SurfaceSnapshotError::PolicyChanged(
            expected.current_policy.name,
        ));
    }
    if bundle.binding != expected.binding {
        return Err(SurfaceSnapshotError::BindingMismatch);
    }
    Ok(verified_confirmation(bundle))
}

fn verify_impact_owner_bundle(
    bundle: &SurfaceSnapshotBundle,
    token_digest: &str,
    expected: &ExpectedSurfaceOwner,
    now_ms: i64,
) -> Result<VerifiedSurfaceConfirmation, SurfaceSnapshotError> {
    if !bundle.issue_impact_confirmation_token
        || bundle.terminal_impact_token_digest.as_deref() != Some(token_digest)
        || bundle
            .terminal_token_expires_at_ms
            .is_none_or(|expires| expires <= now_ms)
    {
        return Err(SurfaceSnapshotError::Expired);
    }
    if bundle.binding.actor_id != expected.actor_id
        || bundle.binding.command != expected.command
        || bundle.binding.owner_context != expected.owner_context
    {
        return Err(SurfaceSnapshotError::BindingMismatch);
    }
    Ok(verified_confirmation(bundle))
}

fn validate_input(input: &CreateSurfaceSnapshot) -> Result<(), SurfaceSnapshotError> {
    if input.items.is_empty() {
        return Err(SurfaceSnapshotError::InvalidInput(
            "snapshot requires at least one match",
        ));
    }
    if input.page_size == 0 || input.page_size > MAX_SURFACE_PAGE_SIZE {
        return Err(SurfaceSnapshotError::InvalidInput("invalid page size"));
    }
    if input.confirmation_reasons.is_empty() || input.confirmation_reasons.len() > 2 {
        return Err(SurfaceSnapshotError::InvalidInput(
            "snapshot requires one or two reasons",
        ));
    }
    if input.policy_enabled == input.policy_block_code.is_some() {
        return Err(SurfaceSnapshotError::InvalidInput(
            "enabled policy must not have a block code and disabled policy must have one",
        ));
    }
    if input.binding.policy_epoch == 0 || input.binding.normalization_version <= 0 {
        return Err(SurfaceSnapshotError::InvalidInput(
            "policy epoch and normalization version must be positive",
        ));
    }
    if input.binding.owner_context.is_empty()
        || input.binding.canonical_content_digest.is_empty()
        || input.binding.owner_evidence_digest.is_empty()
    {
        return Err(SurfaceSnapshotError::InvalidInput(
            "snapshot binding digests must be present",
        ));
    }
    if surface_owner_bundle_digest(&input.owner_bundle)? != input.binding.owner_evidence_digest {
        return Err(SurfaceSnapshotError::InvalidInput(
            "owner bundle digest does not match binding",
        ));
    }
    let contexts = input
        .matched_entry_contexts
        .iter()
        .map(|context| context.word_id)
        .collect::<std::collections::HashSet<_>>();
    if contexts.len() != input.matched_entry_contexts.len()
        || input.matched_entry_contexts.iter().any(|context| {
            context.pos_labels.len() > 5
                || context.gloss_previews.len() > 5
                || context.inbound_relations.previews.len() > 5
        })
    {
        return Err(SurfaceSnapshotError::InvalidInput(
            "matched entry contexts must be unique and bounded",
        ));
    }
    if input
        .items
        .iter()
        .any(|item| !contexts.contains(&item.existing.word_id))
    {
        return Err(SurfaceSnapshotError::InvalidInput(
            "every matched entry requires bounded context",
        ));
    }
    if input.items.iter().any(|item| {
        item.confirmation_reasons.is_empty()
            || item.confirmation_reasons.len() > 2
            || item
                .confirmation_reasons
                .iter()
                .any(|reason| !input.confirmation_reasons.contains(reason))
    }) {
        return Err(SurfaceSnapshotError::InvalidInput(
            "item reason membership must be a non-empty subset of page reasons",
        ));
    }
    let mut match_ids = input
        .items
        .iter()
        .map(|item| item.match_id.as_str())
        .collect::<Vec<_>>();
    match_ids.sort_unstable();
    if match_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(SurfaceSnapshotError::InvalidInput(
            "snapshot match IDs must be unique",
        ));
    }
    Ok(())
}

fn render_page(
    bundle: &SurfaceSnapshotBundle,
    start: usize,
    end: usize,
    next_cursor: Option<String>,
    terminal_token: Option<String>,
    terminal_impact_token: Option<Uuid>,
) -> SurfaceMatchPageV2 {
    let items = bundle.items[start..end].to_vec();
    let contexts_by_id = bundle
        .contexts
        .iter()
        .map(|context| (context.word_id, context))
        .collect::<HashMap<_, _>>();
    let mut context_ids = items
        .iter()
        .map(|item| item.existing.word_id)
        .collect::<Vec<_>>();
    context_ids.sort_unstable();
    context_ids.dedup();
    let matched_entry_contexts = context_ids
        .into_iter()
        .filter_map(|word_id| {
            contexts_by_id
                .get(&word_id)
                .map(|context| (*context).clone())
        })
        .collect();
    let page = SurfaceMatchPageBaseV2 {
        schema_version: 2,
        snapshot_id: bundle.snapshot_id,
        items,
        total: bundle.items.len() as u64,
        matched_entry_contexts,
        confirmation_reasons: bundle.confirmation_reasons.clone(),
        policy_name: bundle.binding.policy_name,
        policy_epoch: bundle.binding.policy_epoch,
    };

    if bundle.policy_enabled {
        match (next_cursor, terminal_token) {
            (Some(next_cursor), None) => {
                SurfaceMatchPageV2::EnabledNext(SurfaceMatchEnabledNextPageV2 {
                    page,
                    continuation_policy: SurfaceContinuationEnabledV2::Enabled,
                    next_cursor,
                })
            }
            (None, Some(surface_confirmation_token)) => {
                SurfaceMatchPageV2::EnabledTerminal(SurfaceMatchEnabledTerminalPageV2 {
                    page,
                    continuation_policy: SurfaceContinuationEnabledV2::Enabled,
                    next_cursor: (),
                    surface_confirmation_token,
                    impact_confirmation_token: terminal_impact_token,
                })
            }
            _ => unreachable!("validated enabled snapshot page shape"),
        }
    } else {
        SurfaceMatchPageV2::TemporarilyDisabled(SurfaceMatchTemporarilyDisabledPageV2 {
            page,
            continuation_policy: SurfaceContinuationDisabledV2::TemporarilyDisabled,
            next_cursor,
            policy_block_code: bundle
                .policy_block_code
                .expect("validated disabled snapshot block code"),
        })
    }
}

pub(crate) fn surface_page_v3(
    page: SurfaceMatchPageV2,
    owner_bundle: &Value,
) -> Result<SurfaceMatchPageV3, SurfaceSnapshotError> {
    let data: V3SurfaceSnapshotPageData = owner_bundle
        .get(V3_SURFACE_PAGE_DATA_KEY)
        .cloned()
        .ok_or(SurfaceSnapshotError::InvalidInput(
            "V3 snapshot owner bundle is missing page data",
        ))
        .and_then(|value| serde_json::from_value(value).map_err(SurfaceSnapshotError::Json))?;
    match page {
        SurfaceMatchPageV2::EnabledNext(page) => Ok(SurfaceMatchPageV3::EnabledNext(
            SurfaceMatchEnabledNextPageV3 {
                page: surface_page_base_v3(page.page, &data)?,
                continuation_policy: page.continuation_policy,
                next_cursor: page.next_cursor,
            },
        )),
        SurfaceMatchPageV2::EnabledTerminal(page) => Ok(SurfaceMatchPageV3::EnabledTerminal(
            SurfaceMatchEnabledTerminalPageV3 {
                page: surface_page_base_v3(page.page, &data)?,
                continuation_policy: page.continuation_policy,
                next_cursor: page.next_cursor,
                surface_confirmation_token: page.surface_confirmation_token,
                impact_confirmation_token: page.impact_confirmation_token,
            },
        )),
        SurfaceMatchPageV2::TemporarilyDisabled(page) => Ok(
            SurfaceMatchPageV3::TemporarilyDisabled(SurfaceMatchTemporarilyDisabledPageV3 {
                page: surface_page_base_v3(page.page, &data)?,
                continuation_policy: page.continuation_policy,
                next_cursor: page.next_cursor,
                policy_block_code: page.policy_block_code,
            }),
        ),
    }
}

fn surface_page_any(
    page: SurfaceMatchPageV2,
    owner_bundle: &Value,
) -> Result<SurfaceMatchPageAny, SurfaceSnapshotError> {
    if owner_bundle.get(V3_SURFACE_PAGE_DATA_KEY).is_some() {
        surface_page_v3(page, owner_bundle).map(SurfaceMatchPageAny::V3)
    } else {
        Ok(SurfaceMatchPageAny::V2(page))
    }
}

fn surface_page_base_v3(
    page: SurfaceMatchPageBaseV2,
    data: &V3SurfaceSnapshotPageData,
) -> Result<SurfaceMatchPageBaseV3, SurfaceSnapshotError> {
    let items_by_id = data
        .items
        .iter()
        .map(|item| (item.match_id.as_str(), &item.item))
        .collect::<HashMap<_, _>>();
    let items = page
        .items
        .iter()
        .map(|item| {
            items_by_id
                .get(item.match_id.as_str())
                .map(|item| (*item).clone())
                .ok_or(SurfaceSnapshotError::InvalidInput(
                    "V3 snapshot page membership is inconsistent",
                ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let entry_ids = items
        .iter()
        .map(surface_match_item_entry_id)
        .collect::<std::collections::HashSet<_>>();
    let matched_entry_contexts = data
        .matched_entry_contexts
        .iter()
        .filter(|context| entry_ids.contains(&context.entry_id))
        .cloned()
        .collect::<Vec<_>>();
    if matched_entry_contexts.len() != entry_ids.len() {
        return Err(SurfaceSnapshotError::InvalidInput(
            "V3 snapshot page is missing matched entry context",
        ));
    }
    Ok(SurfaceMatchPageBaseV3 {
        schema_version: 3,
        snapshot_id: page.snapshot_id,
        items,
        total: page.total,
        matched_entry_contexts,
        confirmation_reasons: page.confirmation_reasons,
        policy_name: page.policy_name,
        policy_epoch: page.policy_epoch,
    })
}

const fn surface_match_item_entry_id(item: &SurfaceMatchItemV3) -> Uuid {
    match item {
        SurfaceMatchItemV3::LegacyV2(item) => item.existing.word_id,
        SurfaceMatchItemV3::FormVariantV3(item) => item.entry_id,
    }
}

pub(crate) fn surface_match_digest(
    items: &[LexiconSurfaceMatchV2],
    reasons: &[SurfaceConfirmationReasonV2],
) -> Result<String, SurfaceSnapshotError> {
    #[derive(Serialize)]
    struct Membership<'a> {
        match_id: &'a str,
        reasons: &'a [SurfaceConfirmationReasonV2],
    }
    #[derive(Serialize)]
    struct Digest<'a> {
        reasons: &'a [SurfaceConfirmationReasonV2],
        items: Vec<Membership<'a>>,
    }
    let digest = Digest {
        reasons,
        items: items
            .iter()
            .map(|item| Membership {
                match_id: &item.match_id,
                reasons: &item.confirmation_reasons,
            })
            .collect(),
    };
    Ok(hash_token(&serde_json::to_string(&digest)?))
}

pub(crate) fn surface_context_digest(
    contexts: &[MatchedEntryContextV2],
) -> Result<String, SurfaceSnapshotError> {
    let mut ordered = contexts.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|context| context.word_id);
    Ok(hash_token(&serde_json::to_string(&ordered)?))
}

fn reason_digest(reasons: &[SurfaceConfirmationReasonV2]) -> String {
    reasons
        .iter()
        .map(|reason| match reason {
            SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches => 's',
            SurfaceConfirmationReasonV2::VisibilityActivation => 'v',
        })
        .collect()
}

fn reason_order(reason: &SurfaceConfirmationReasonV2) -> u8 {
    match reason {
        SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches => 0,
        SurfaceConfirmationReasonV2::VisibilityActivation => 1,
    }
}

fn parse_policy_name(value: &str) -> Result<SurfacePolicyNameV2, SurfaceSnapshotError> {
    Ok(serde_json::from_value(Value::String(value.to_owned()))?)
}

fn add_duration(now_ms: i64, duration: Duration) -> Result<i64, SurfaceSnapshotError> {
    now_ms
        .checked_add(duration_ms(duration)?)
        .ok_or(SurfaceSnapshotError::InvalidInput("snapshot TTL overflow"))
}

fn duration_ms(duration: Duration) -> Result<i64, SurfaceSnapshotError> {
    i64::try_from(duration.as_millis())
        .ok()
        .filter(|value| *value > 0)
        .ok_or(SurfaceSnapshotError::InvalidInput(
            "snapshot TTL must be positive",
        ))
}

fn remaining_bundle_ttl_ms(
    bundle: &SurfaceSnapshotBundle,
    now_ms: i64,
) -> Result<i64, SurfaceSnapshotError> {
    let ttl = bundle.lease_expires_at_ms.saturating_sub(now_ms);
    if ttl <= 0 {
        return Err(SurfaceSnapshotError::InvalidInput(
            "snapshot TTL must be positive",
        ));
    }
    Ok(ttl)
}

fn snapshot_key(snapshot_id: Uuid) -> String {
    format!("{SNAPSHOT_PREFIX}{snapshot_id}")
}

fn token_key(token_digest: &str) -> String {
    format!("{TOKEN_PREFIX}{token_digest}")
}

fn impact_token_key(token_digest: &str) -> String {
    format!("{IMPACT_TOKEN_PREFIX}{token_digest}")
}

fn active_context_digest(binding: &SurfaceConfirmationBinding) -> String {
    hash_token(
        &serde_json::to_string(&(binding.actor_id, binding.command, &binding.owner_context))
            .expect("surface active binding is serializable"),
    )
}

fn active_key(active_context_digest: &str) -> String {
    format!("{ACTIVE_PREFIX}{active_context_digest}")
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    const IDLE_TTL: Duration = Duration::from_secs(1);
    const TOKEN_TTL: Duration = Duration::from_secs(5);

    fn binding(actor_id: Uuid) -> SurfaceConfirmationBinding {
        SurfaceConfirmationBinding {
            actor_id,
            command: SurfaceConsumptionCommand::CreateEntry,
            owner_context: Uuid::from_u128(0xd37ec710).to_string(),
            base_revision: None,
            canonical_content_digest: "content-v1".to_owned(),
            owner_evidence_digest: "detection-v1".to_owned(),
            normalization_version: 1,
            policy_name: SurfacePolicyNameV2::AllowNewExactHeadwordEntries,
            policy_epoch: 7,
        }
    }

    fn item(index: usize, reasons: Vec<SurfaceConfirmationReasonV2>) -> LexiconSurfaceMatchV2 {
        serde_json::from_value(json!({
            "match_id": format!("match-{index:02}"),
            "match_category": "exact_headword",
            "severity": "warning",
            "attention_level": "high",
            "can_continue": true,
            "confirmation_reasons": reasons,
            "candidate": {
                "candidate_type": "headword",
                "candidate_ref": "candidate:common",
                "surface": "workspace",
                "normalized_surface": "workspace",
                "dialect": "common",
                "entry_kind": "word"
            },
            "existing": {
                "word_id": Uuid::from_u128(0x1000 + index as u128),
                "headword": "workspace",
                "kind": "word",
                "status": "draft",
                "source": {
                    "source_kind": "headword",
                    "source_id": format!("source-{index:02}"),
                    "content_scope": "draft",
                    "surface": "workspace",
                    "dialect": "common"
                }
            }
        }))
        .unwrap()
    }

    fn context(index: usize) -> MatchedEntryContextV2 {
        MatchedEntryContextV2 {
            word_id: Uuid::from_u128(0x1000 + index as u128),
            pos_labels: vec!["noun".to_owned()],
            gloss_previews: vec![format!("gloss-{index}")],
            updated_at: Utc.timestamp_opt(1_700_000_000 + index as i64, 0).unwrap(),
            inbound_relations: serde_json::from_value(json!({
                "total": 0,
                "by_type": {"synonym": 0, "antonym": 0, "derivative": 0},
                "previews": [],
                "truncated": false
            }))
            .unwrap(),
        }
    }

    fn input(
        actor_id: Uuid,
        item_count: usize,
        page_size: usize,
        enabled: bool,
    ) -> CreateSurfaceSnapshot {
        let reasons = vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches];
        let owner_bundle = json!({
            "detection_id": Uuid::from_u128(0xd37ec710),
            "canonical_detection": {"headword": "workspace"}
        });
        let mut binding = binding(actor_id);
        binding.owner_evidence_digest = surface_owner_bundle_digest(&owner_bundle).unwrap();
        CreateSurfaceSnapshot {
            binding,
            policy_enabled: enabled,
            policy_block_code: (!enabled)
                .then_some(SurfacePolicyBlockCodeV2::ExactHeadwordCreationTemporarilyDisabled),
            items: (0..item_count)
                .rev()
                .map(|index| item(index, reasons.clone()))
                .collect(),
            matched_entry_contexts: (0..item_count).map(context).collect(),
            confirmation_reasons: reasons,
            owner_bundle,
            page_size,
        }
    }

    fn forms_input(
        actor_id: Uuid,
        item_count: usize,
        page_size: usize,
        enabled: bool,
    ) -> CreateSurfaceSnapshot {
        let mut input = input(actor_id, item_count, page_size, enabled);
        input.binding.command = SurfaceConsumptionCommand::SaveForms;
        input.binding.base_revision = Some(7);
        input
    }

    fn next_cursor(page: &SurfaceMatchPageV2) -> &str {
        match page {
            SurfaceMatchPageV2::EnabledNext(page) => &page.next_cursor,
            SurfaceMatchPageV2::TemporarilyDisabled(page) => {
                page.next_cursor.as_deref().expect("non-terminal cursor")
            }
            SurfaceMatchPageV2::EnabledTerminal(_) => panic!("expected non-terminal page"),
        }
    }

    fn terminal_token(page: &SurfaceMatchPageV2) -> &str {
        match page {
            SurfaceMatchPageV2::EnabledTerminal(page) => &page.surface_confirmation_token,
            _ => panic!("expected enabled terminal page"),
        }
    }

    fn terminal_impact_token(page: &SurfaceMatchPageV2) -> Option<Uuid> {
        match page {
            SurfaceMatchPageV2::EnabledTerminal(page) => page.impact_confirmation_token,
            _ => panic!("expected enabled terminal page"),
        }
    }

    fn unused_impact_token() -> Uuid {
        Uuid::from_u128(0x1a2b3c4d)
    }

    fn page_match_ids(page: &SurfaceMatchPageV2) -> Vec<&str> {
        let items = match page {
            SurfaceMatchPageV2::EnabledNext(page) => &page.page.items,
            SurfaceMatchPageV2::EnabledTerminal(page) => &page.page.items,
            SurfaceMatchPageV2::TemporarilyDisabled(page) => &page.page.items,
        };
        items.iter().map(|item| item.match_id.as_str()).collect()
    }

    #[test]
    fn cursor_is_actor_bound_strictly_sequential_and_terminal_only_signs_token() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 3, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-1".to_owned(),
            "unused-token".to_owned(),
            None,
        )
        .unwrap();
        assert_eq!(page_match_ids(&initialized.page), vec!["match-00"]);
        assert_eq!(next_cursor(&initialized.page), "cursor-1");
        let stored = serde_json::to_string(&initialized.bundle).unwrap();
        assert!(!stored.contains("cursor-1"));
        assert!(!stored.contains("unused-token"));

        let mut bundle = initialized.bundle;
        assert!(matches!(
            advance_bundle(
                &mut bundle,
                Uuid::now_v7(),
                "cursor-1",
                10,
                IDLE_TTL,
                TOKEN_TTL,
                "cursor-x".to_owned(),
                "token-x".to_owned(),
                unused_impact_token(),
            ),
            Err(SurfaceSnapshotError::Expired)
        ));

        let second = advance_bundle(
            &mut bundle,
            actor,
            "cursor-1",
            10,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-2".to_owned(),
            "unused-token".to_owned(),
            unused_impact_token(),
        )
        .unwrap();
        assert_eq!(page_match_ids(&second), vec!["match-01"]);
        assert_eq!(next_cursor(&second), "cursor-2");

        let terminal = advance_bundle(
            &mut bundle,
            actor,
            "cursor-2",
            20,
            IDLE_TTL,
            TOKEN_TTL,
            "unused-cursor".to_owned(),
            "terminal-token".to_owned(),
            unused_impact_token(),
        )
        .unwrap();
        assert_eq!(page_match_ids(&terminal), vec!["match-02"]);
        assert_eq!(terminal_token(&terminal), "terminal-token");
        assert_eq!(terminal_impact_token(&terminal), None);
        assert!(!bundle.issue_impact_confirmation_token);
        assert!(bundle.terminal_impact_token_digest.is_none());
    }

    #[test]
    fn forms_surface_and_impact_tokens_are_signed_together_only_on_terminal_page() {
        let actor = Uuid::now_v7();
        let impact_token = Uuid::from_u128(0x1234_5678_9abc_def0);
        let impact_token_wire = impact_token.to_string();
        let initialized = initialize_snapshot(
            forms_input(actor, 3, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-1".to_owned(),
            "unused-surface-token".to_owned(),
            Some(impact_token),
        )
        .unwrap();
        assert!(matches!(
            initialized.page,
            SurfaceMatchPageV2::EnabledNext(_)
        ));
        assert!(initialized.terminal_token_digest.is_none());
        assert!(initialized.terminal_impact_token_digest.is_none());
        assert!(initialized.bundle.issue_impact_confirmation_token);
        let serialized = serde_json::to_string(&initialized.bundle).unwrap();
        assert!(!serialized.contains("unused-surface-token"));
        assert!(!serialized.contains(&impact_token_wire));

        let mut bundle = initialized.bundle;
        let second = advance_bundle(
            &mut bundle,
            actor,
            "cursor-1",
            10,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-2".to_owned(),
            "still-unused-surface-token".to_owned(),
            unused_impact_token(),
        )
        .unwrap();
        assert!(matches!(second, SurfaceMatchPageV2::EnabledNext(_)));
        assert!(bundle.terminal_token_digest.is_none());
        assert!(bundle.terminal_impact_token_digest.is_none());

        let terminal = advance_bundle(
            &mut bundle,
            actor,
            "cursor-2",
            20,
            IDLE_TTL,
            TOKEN_TTL,
            "unused-cursor".to_owned(),
            "surface-terminal-token".to_owned(),
            impact_token,
        )
        .unwrap();
        assert_eq!(terminal_token(&terminal), "surface-terminal-token");
        assert_eq!(terminal_impact_token(&terminal), Some(impact_token));
        assert_eq!(bundle.terminal_token_expires_at_ms, Some(5_020));
        assert_eq!(bundle.lease_expires_at_ms, 5_020);

        let expected = ExpectedSurfaceConfirmation {
            binding: bundle.binding.clone(),
            current_policy: SurfaceCreationPolicy {
                enabled: true,
                name: bundle.binding.policy_name,
                epoch: bundle.binding.policy_epoch,
            },
        };
        let expected_owner = ExpectedSurfaceOwner {
            actor_id: actor,
            command: SurfaceConsumptionCommand::SaveForms,
            owner_context: bundle.binding.owner_context.clone(),
        };
        let surface_digest = hash_token("surface-terminal-token");
        let impact_digest = hash_token(&impact_token_wire);
        assert_ne!(surface_digest, impact_digest);
        verify_bundle(&bundle, &surface_digest, &expected, 5_019).unwrap();
        verify_impact_owner_bundle(&bundle, &impact_digest, &expected_owner, 5_019).unwrap();
        assert!(matches!(
            verify_bundle(&bundle, &impact_digest, &expected, 5_019),
            Err(SurfaceSnapshotError::Expired)
        ));
        assert!(matches!(
            verify_impact_owner_bundle(&bundle, &surface_digest, &expected_owner, 5_019),
            Err(SurfaceSnapshotError::Expired)
        ));
        assert!(matches!(
            verify_bundle(&bundle, &surface_digest, &expected, 5_020),
            Err(SurfaceSnapshotError::Expired)
        ));
        assert!(matches!(
            verify_impact_owner_bundle(&bundle, &impact_digest, &expected_owner, 5_020),
            Err(SurfaceSnapshotError::Expired)
        ));

        let surface_digest_before_replay = bundle.terminal_token_digest.clone();
        let impact_digest_before_replay = bundle.terminal_impact_token_digest.clone();
        assert!(matches!(
            advance_bundle(
                &mut bundle,
                actor,
                "cursor-2",
                21,
                IDLE_TTL,
                TOKEN_TTL,
                "attacker-cursor".to_owned(),
                "attacker-surface-token".to_owned(),
                Uuid::now_v7(),
            ),
            Err(SurfaceSnapshotError::InvalidCursor)
        ));
        assert_eq!(bundle.terminal_token_digest, surface_digest_before_replay);
        assert_eq!(
            bundle.terminal_impact_token_digest,
            impact_digest_before_replay
        );
        assert_eq!(bundle.lease_expires_at_ms, 5_020);
    }

    #[test]
    fn forms_single_page_terminal_immediately_returns_uuid_impact_token() {
        let actor = Uuid::now_v7();
        let impact_token = Uuid::from_u128(0xf012_3456_789a_bcde);
        let initialized = initialize_snapshot(
            forms_input(actor, 1, 1, true),
            100,
            IDLE_TTL,
            TOKEN_TTL,
            "unused-cursor".to_owned(),
            "surface-terminal-token".to_owned(),
            Some(impact_token),
        )
        .unwrap();
        assert_eq!(terminal_token(&initialized.page), "surface-terminal-token");
        assert_eq!(terminal_impact_token(&initialized.page), Some(impact_token));
        assert_eq!(initialized.bundle.terminal_token_expires_at_ms, Some(5_100));
        assert_eq!(initialized.bundle.lease_expires_at_ms, 5_100);
        assert_eq!(
            initialized.terminal_impact_token_digest,
            Some(hash_token(&impact_token.to_string()))
        );
    }

    #[test]
    fn successful_new_page_renews_idle_lease_but_replayed_cursor_does_not() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 3, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-1".to_owned(),
            "unused".to_owned(),
            None,
        )
        .unwrap();
        let mut bundle = initialized.bundle;
        assert_eq!(bundle.lease_expires_at_ms, 1_000);

        advance_bundle(
            &mut bundle,
            actor,
            "cursor-1",
            999,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-2".to_owned(),
            "unused".to_owned(),
            unused_impact_token(),
        )
        .unwrap();
        assert_eq!(bundle.lease_expires_at_ms, 1_999);

        let lease_before_replay = bundle.lease_expires_at_ms;
        assert!(matches!(
            advance_bundle(
                &mut bundle,
                actor,
                "cursor-1",
                1_000,
                IDLE_TTL,
                TOKEN_TTL,
                "attacker-cursor".to_owned(),
                "attacker-token".to_owned(),
                unused_impact_token(),
            ),
            Err(SurfaceSnapshotError::InvalidCursor)
        ));
        assert_eq!(bundle.lease_expires_at_ms, lease_before_replay);
        assert!(matches!(
            advance_bundle(
                &mut bundle,
                actor,
                "cursor-2",
                1_999,
                IDLE_TTL,
                TOKEN_TTL,
                "unused".to_owned(),
                "unused".to_owned(),
                unused_impact_token(),
            ),
            Err(SurfaceSnapshotError::Expired)
        ));
    }

    #[test]
    fn disabled_policy_pages_full_snapshot_without_signing_either_forms_token() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            forms_input(actor, 2, 1, false),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-1".to_owned(),
            "must-not-appear".to_owned(),
            Some(unused_impact_token()),
        )
        .unwrap();
        assert!(matches!(
            initialized.page,
            SurfaceMatchPageV2::TemporarilyDisabled(_)
        ));
        let mut bundle = initialized.bundle;
        let terminal = advance_bundle(
            &mut bundle,
            actor,
            "cursor-1",
            10,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "must-not-appear".to_owned(),
            unused_impact_token(),
        )
        .unwrap();
        match terminal {
            SurfaceMatchPageV2::TemporarilyDisabled(page) => {
                assert_eq!(page.next_cursor, None);
                assert_eq!(
                    page.policy_block_code,
                    SurfacePolicyBlockCodeV2::ExactHeadwordCreationTemporarilyDisabled
                );
            }
            _ => panic!("disabled policy must never sign a token"),
        }
        assert!(bundle.terminal_token_digest.is_none());
        assert!(bundle.terminal_impact_token_digest.is_none());
    }

    #[test]
    fn impact_confirmation_switch_rejects_non_forms_snapshot_owner() {
        let actor = Uuid::now_v7();
        assert!(matches!(
            initialize_snapshot(
                input(actor, 1, 1, true),
                0,
                IDLE_TTL,
                TOKEN_TTL,
                "unused".to_owned(),
                "surface-token".to_owned(),
                Some(unused_impact_token()),
            ),
            Err(SurfaceSnapshotError::InvalidInput(
                "impact confirmation token is only valid for save_forms snapshots"
            ))
        ));
    }

    #[test]
    fn legacy_snapshot_bundle_without_impact_fields_defaults_to_surface_only() {
        let initialized = initialize_snapshot(
            input(Uuid::now_v7(), 1, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "surface-token".to_owned(),
            None,
        )
        .unwrap();
        let mut legacy = serde_json::to_value(initialized.bundle).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("issue_impact_confirmation_token");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("terminal_impact_token_digest");
        let restored: SurfaceSnapshotBundle = serde_json::from_value(legacy).unwrap();
        assert!(!restored.issue_impact_confirmation_token);
        assert!(restored.terminal_impact_token_digest.is_none());
    }

    #[test]
    fn literal_page_fields_reject_false_and_non_null_terminal_cursor() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 1, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "terminal-token".to_owned(),
            None,
        )
        .unwrap();

        let mut page = serde_json::to_value(&initialized.page).unwrap();
        assert!(page["next_cursor"].is_null());
        page["next_cursor"] = serde_json::json!("not-null");
        assert!(serde_json::from_value::<SurfaceMatchPageV2>(page).is_err());

        let mut item = serde_json::to_value(&initialized.bundle.items[0]).unwrap();
        assert_eq!(item["can_continue"], true);
        item["can_continue"] = serde_json::json!(false);
        assert!(serde_json::from_value::<LexiconSurfaceMatchV2>(item).is_err());
    }

    #[test]
    fn terminal_token_binds_owner_command_content_evidence_digest_epoch_and_ttl() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 1, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "terminal-token".to_owned(),
            None,
        )
        .unwrap();
        let bundle = initialized.bundle;
        assert_eq!(terminal_token(&initialized.page), "terminal-token");
        assert_eq!(terminal_impact_token(&initialized.page), None);
        assert!(
            !serde_json::to_string(&bundle)
                .unwrap()
                .contains("terminal-token")
        );
        let expected = ExpectedSurfaceConfirmation {
            binding: bundle.binding.clone(),
            current_policy: SurfaceCreationPolicy {
                enabled: true,
                name: bundle.binding.policy_name,
                epoch: bundle.binding.policy_epoch,
            },
        };
        let token_digest = hash_token("terminal-token");
        let verified = verify_bundle(&bundle, &token_digest, &expected, 4_000).unwrap();
        assert_eq!(verified.owner_bundle, bundle.owner_bundle);
        assert_eq!(verified.match_ids, vec!["match-00"]);

        for mutate in [
            |binding: &mut SurfaceConfirmationBinding| binding.actor_id = Uuid::now_v7(),
            |binding: &mut SurfaceConfirmationBinding| {
                binding.command = SurfaceConsumptionCommand::SaveForms
            },
            |binding: &mut SurfaceConfirmationBinding| binding.owner_context.push_str("-other"),
            |binding: &mut SurfaceConfirmationBinding| {
                binding.canonical_content_digest.push_str("-other")
            },
            |binding: &mut SurfaceConfirmationBinding| {
                binding.owner_evidence_digest.push_str("-other")
            },
            |binding: &mut SurfaceConfirmationBinding| binding.normalization_version += 1,
        ] {
            let mut changed = expected.clone();
            mutate(&mut changed.binding);
            assert!(matches!(
                verify_bundle(&bundle, &token_digest, &changed, 4_000),
                Err(SurfaceSnapshotError::BindingMismatch)
            ));
        }

        let mut changed_policy = expected.clone();
        changed_policy.current_policy.epoch += 1;
        assert!(matches!(
            verify_bundle(&bundle, &token_digest, &changed_policy, 4_000),
            Err(SurfaceSnapshotError::PolicyChanged(_))
        ));
        assert!(matches!(
            verify_bundle(&bundle, &token_digest, &expected, 5_000),
            Err(SurfaceSnapshotError::Expired)
        ));
    }

    #[test]
    fn owner_bundle_outlives_original_detection_ttl_until_terminal_token_expires() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 1, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "terminal-token".to_owned(),
            None,
        )
        .unwrap();
        let expected = ExpectedSurfaceConfirmation {
            binding: initialized.bundle.binding.clone(),
            current_policy: SurfaceCreationPolicy {
                enabled: true,
                name: initialized.bundle.binding.policy_name,
                epoch: initialized.bundle.binding.policy_epoch,
            },
        };
        let verified = verify_bundle(
            &initialized.bundle,
            &hash_token("terminal-token"),
            &expected,
            2_000,
        )
        .unwrap();
        assert_eq!(
            verified.owner_bundle["canonical_detection"]["headword"],
            "workspace"
        );
    }

    #[test]
    fn terminal_token_recovers_owner_bundle_without_detection_store_and_binds_owner_command() {
        let actor = Uuid::now_v7();
        let initialized = initialize_snapshot(
            input(actor, 1, 1, true),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "terminal-token".to_owned(),
            None,
        )
        .unwrap();
        let token_digest = hash_token("terminal-token");
        let expected = ExpectedSurfaceOwner {
            actor_id: actor,
            command: SurfaceConsumptionCommand::CreateEntry,
            owner_context: initialized.bundle.binding.owner_context.clone(),
        };
        let verified =
            verify_owner_bundle(&initialized.bundle, &token_digest, &expected, 2_000).unwrap();
        assert_eq!(
            verified.owner_bundle["canonical_detection"]["headword"],
            "workspace"
        );

        for changed in [
            ExpectedSurfaceOwner {
                actor_id: Uuid::now_v7(),
                ..expected.clone()
            },
            ExpectedSurfaceOwner {
                command: SurfaceConsumptionCommand::PublishEntry,
                ..expected.clone()
            },
            ExpectedSurfaceOwner {
                owner_context: "different-detection".to_owned(),
                ..expected.clone()
            },
        ] {
            assert!(matches!(
                verify_owner_bundle(&initialized.bundle, &token_digest, &changed, 2_000),
                Err(SurfaceSnapshotError::BindingMismatch)
            ));
        }
    }

    #[test]
    fn confirmation_tracks_match_membership_and_display_context_with_separate_digests() {
        let actor = Uuid::now_v7();
        let mut ordinary = input(actor, 1, 1, true);
        let mut composite = ordinary.clone();
        composite.confirmation_reasons = vec![
            SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
            SurfaceConfirmationReasonV2::VisibilityActivation,
        ];
        composite.items[0].confirmation_reasons = composite.confirmation_reasons.clone();
        let ordinary_bundle = initialize_snapshot(
            ordinary.clone(),
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "token".to_owned(),
            None,
        )
        .unwrap()
        .bundle;
        let composite_bundle = initialize_snapshot(
            composite,
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "token".to_owned(),
            None,
        )
        .unwrap()
        .bundle;
        assert_ne!(ordinary_bundle.match_digest, composite_bundle.match_digest);

        ordinary.matched_entry_contexts[0].gloss_previews = vec!["changed context".to_owned()];
        let context_changed = initialize_snapshot(
            ordinary,
            0,
            IDLE_TTL,
            TOKEN_TTL,
            "unused".to_owned(),
            "token".to_owned(),
            None,
        )
        .unwrap()
        .bundle;
        assert_eq!(ordinary_bundle.match_digest, context_changed.match_digest);
        assert_ne!(
            ordinary_bundle.context_digest,
            context_changed.context_digest
        );
    }

    #[test]
    fn snapshot_rejects_owner_evidence_or_duplicate_match_ids_that_break_binding() {
        let actor = Uuid::now_v7();
        let mut changed_evidence = input(actor, 1, 1, true);
        changed_evidence.owner_bundle["canonical_detection"]["headword"] = json!("changed");
        assert!(matches!(
            initialize_snapshot(
                changed_evidence,
                0,
                IDLE_TTL,
                TOKEN_TTL,
                "cursor".to_owned(),
                "token".to_owned(),
                None,
            ),
            Err(SurfaceSnapshotError::InvalidInput(
                "owner bundle digest does not match binding"
            ))
        ));

        let mut duplicate = input(actor, 2, 1, true);
        duplicate.items[1].match_id = duplicate.items[0].match_id.clone();
        assert!(matches!(
            initialize_snapshot(
                duplicate,
                0,
                IDLE_TTL,
                TOKEN_TTL,
                "cursor".to_owned(),
                "token".to_owned(),
                None,
            ),
            Err(SurfaceSnapshotError::InvalidInput(
                "snapshot match IDs must be unique"
            ))
        ));
    }

    #[test]
    fn v3_page_projection_preserves_cursor_membership_and_terminal_tokens() {
        let actor = Uuid::now_v7();
        let mut snapshot_input = input(actor, 2, 1, true);
        let page_data = V3SurfaceSnapshotPageData {
            items: (0..2)
                .map(|index| V3SurfaceSnapshotItem {
                    match_id: format!("match-{index:02}"),
                    item: serde_json::from_value(if index == 0 {
                        json!({
                            "match_kind": "legacy_v2",
                            "match": {
                                "source_schema_version": 2,
                                "existing": {
                                    "word_id": Uuid::from_u128(0x1000 + index),
                                    "headword": "workspace",
                                    "kind": "word",
                                    "status": "draft",
                                    "source": {
                                        "source_kind": "headword",
                                        "source_id": "headword:common",
                                        "content_scope": "draft",
                                        "surface": "workspace",
                                        "dialect": "common"
                                    }
                                }
                            }
                        })
                    } else {
                        json!({
                            "match_kind": "form_variant_v3",
                            "match": {
                                "source_schema_version": 3,
                                "entry_id": Uuid::from_u128(0x1000 + index),
                                "status": "draft",
                                "content_scope": "draft",
                                "pos_id": Uuid::from_u128(0x2000 + index),
                                "group_ids": [Uuid::from_u128(0x3000 + index)],
                                "form_id": Uuid::from_u128(0x4000 + index),
                                "variant_id": Uuid::from_u128(0x5000 + index),
                                "form_type": "base",
                                "dialect": "common",
                                "spelling": "workspace"
                            }
                        })
                    })
                    .unwrap(),
                })
                .collect(),
            matched_entry_contexts: (0..2)
                .map(|index| {
                    serde_json::from_value(json!({
                        "entry_id": Uuid::from_u128(0x1000 + index),
                        "presentation": {
                            "label": format!("workspace-{index}"),
                            "matched_surfaces": ["workspace"],
                            "strategy_version": "surface_summary_v1"
                        },
                        "pos_labels": ["noun"],
                        "gloss_previews": [],
                        "updated_at": Utc.timestamp_opt(1_700_000_000 + index as i64, 0).unwrap(),
                        "inbound_relations": {
                            "total": 0,
                            "by_type": {"synonym": 0, "antonym": 0, "derivative": 0},
                            "previews": [],
                            "truncated": false
                        }
                    }))
                    .unwrap()
                })
                .collect(),
        };
        snapshot_input.owner_bundle[V3_SURFACE_PAGE_DATA_KEY] =
            serde_json::to_value(page_data).unwrap();
        snapshot_input.binding.owner_evidence_digest =
            surface_owner_bundle_digest(&snapshot_input.owner_bundle).unwrap();
        let mut initialized = initialize_snapshot(
            snapshot_input,
            1_000,
            IDLE_TTL,
            TOKEN_TTL,
            "cursor-1".to_owned(),
            "unused".to_owned(),
            None,
        )
        .unwrap();

        let first = surface_page_v3(initialized.page, &initialized.bundle.owner_bundle).unwrap();
        let SurfaceMatchPageV3::EnabledNext(first) = first else {
            panic!("expected V3 next page");
        };
        assert_eq!(first.page.schema_version, 3);
        assert_eq!(first.page.items.len(), 1);
        assert!(matches!(
            first.page.items[0],
            SurfaceMatchItemV3::LegacyV2(_)
        ));
        assert_eq!(first.page.matched_entry_contexts.len(), 1);
        assert_eq!(first.next_cursor, "cursor-1");

        let terminal_v2 = advance_bundle(
            &mut initialized.bundle,
            actor,
            "cursor-1",
            1_100,
            IDLE_TTL,
            TOKEN_TTL,
            "unused-next".to_owned(),
            "terminal-token".to_owned(),
            Uuid::now_v7(),
        )
        .unwrap();
        let terminal = surface_page_v3(terminal_v2, &initialized.bundle.owner_bundle).unwrap();
        let SurfaceMatchPageV3::EnabledTerminal(terminal) = terminal else {
            panic!("expected V3 terminal page");
        };
        assert_eq!(terminal.page.items.len(), 1);
        assert!(matches!(
            terminal.page.items[0],
            SurfaceMatchItemV3::FormVariantV3(_)
        ));
        assert_eq!(terminal.surface_confirmation_token, "terminal-token");
    }
}
