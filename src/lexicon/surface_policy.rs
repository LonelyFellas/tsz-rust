use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

use crate::lexicon::dto::SurfacePolicyNameV2;

pub(crate) const SURFACE_POLICY_PREFIX: &str = "lexicon:surface-policy:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCreationPolicy {
    pub enabled: bool,
    pub name: SurfacePolicyNameV2,
    pub epoch: u64,
}

pub const fn exact_headword_creation_policy() -> SurfaceCreationPolicy {
    default_policy(SurfacePolicyNameV2::AllowNewExactHeadwordEntries)
}

pub const fn default_policy(name: SurfacePolicyNameV2) -> SurfaceCreationPolicy {
    SurfaceCreationPolicy {
        enabled: matches!(name, SurfacePolicyNameV2::SurfaceWarningAcknowledgement),
        name,
        epoch: 1,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SurfacePolicyStoreError {
    #[error(transparent)]
    Pool(#[from] deadpool_redis::PoolError),
    #[error(transparent)]
    Redis(#[from] deadpool_redis::redis::RedisError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("surface policy epoch overflow")]
    EpochOverflow,
    #[error("surface policy writer barrier failed")]
    Database(#[source] sqlx::Error),
}

#[derive(Clone)]
pub struct SurfacePolicyStore {
    redis: Pool,
    prefix: String,
}

impl SurfacePolicyStore {
    pub fn new(redis: Pool) -> Self {
        Self {
            redis,
            prefix: SURFACE_POLICY_PREFIX.to_owned(),
        }
    }

    #[doc(hidden)]
    pub fn with_prefix_for_test(redis: Pool, prefix: String) -> Self {
        Self { redis, prefix }
    }

    pub(crate) fn with_prefix(redis: Pool, prefix: String) -> Self {
        Self { redis, prefix }
    }

    /// Load the persisted policy, atomically seeding the expand-phase default
    /// when the key does not exist. The default is intentionally disabled.
    pub async fn exact_headword_creation(
        &self,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        self.policy(SurfacePolicyNameV2::AllowNewExactHeadwordEntries)
            .await
    }

    pub async fn multiple_active_exact_headword_publications(
        &self,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        self.policy(SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications)
            .await
    }

    pub async fn policy(
        &self,
        name: SurfacePolicyNameV2,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        let mut connection = self.redis.get().await?;
        let default = serde_json::to_string(&default_policy(name))?;
        let payload: String = deadpool_redis::redis::Script::new(
            r#"
            local current = redis.call('GET', KEYS[1])
            if current then
                return current
            end
            redis.call('SET', KEYS[1], ARGV[1], 'NX')
            return redis.call('GET', KEYS[1])
            "#,
        )
        .key(policy_key(&self.prefix, name))
        .arg(default)
        .invoke_async(&mut connection)
        .await?;
        Ok(serde_json::from_str(&payload)?)
    }

    /// Change capability state and advance the monotonic epoch exactly once
    /// per actual transition. Repeating the same state is idempotent.
    pub async fn transition_exact_headword_creation(
        &self,
        database: &PgPool,
        enabled: bool,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        self.transition(
            database,
            SurfacePolicyNameV2::AllowNewExactHeadwordEntries,
            enabled,
        )
        .await
    }

    pub async fn transition(
        &self,
        database: &PgPool,
        name: SurfacePolicyNameV2,
        enabled: bool,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        if enabled {
            // Enabling must serialize with the cutover's exclusive barrier
            // before Redis can publish a new epoch. Otherwise a concurrent
            // cutover could observe disabled, drop the legacy UNIQUE, and only
            // then let the already-started enable become visible.
            let barrier = lock_policy_enable(database).await?;
            let persisted = self.transition_redis(name, true).await?;
            barrier
                .commit()
                .await
                .map_err(SurfacePolicyStoreError::Database)?;
            Ok(persisted)
        } else {
            // Stop token issuance first. An enable that already owns the
            // shared barrier may still publish a newer enabled epoch, so wait
            // for the exclusive barrier and reassert disabled while holding
            // it before reporting success.
            self.transition_redis(name, false).await?;
            let barrier = lock_policy_disable(database).await?;
            let persisted = self.transition_redis(name, false).await?;
            barrier
                .commit()
                .await
                .map_err(SurfacePolicyStoreError::Database)?;
            Ok(persisted)
        }
    }

    async fn transition_redis(
        &self,
        name: SurfacePolicyNameV2,
        enabled: bool,
    ) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
        loop {
            let current = self.policy(name).await?;
            let next = transition_policy(current, enabled)?;
            if next == current {
                return Ok(current);
            }

            let mut connection = self.redis.get().await?;
            let current_payload = serde_json::to_string(&current)?;
            let next_payload = serde_json::to_string(&next)?;
            let payload: Option<String> = deadpool_redis::redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current ~= ARGV[1] then
                    return nil
                end
                redis.call('SET', KEYS[1], ARGV[2])
                return ARGV[2]
                "#,
            )
            .key(policy_key(&self.prefix, name))
            .arg(current_payload)
            .arg(next_payload)
            .invoke_async(&mut connection)
            .await?;

            if let Some(payload) = payload {
                return Ok(serde_json::from_str(&payload)?);
            }
        }
    }
}

async fn lock_policy_enable(
    database: &PgPool,
) -> Result<Transaction<'static, Postgres>, SurfacePolicyStoreError> {
    let mut transaction = database
        .begin()
        .await
        .map_err(SurfacePolicyStoreError::Database)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock_shared(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *transaction)
    .await
    .map_err(SurfacePolicyStoreError::Database)?;
    Ok(transaction)
}

async fn lock_policy_disable(
    database: &PgPool,
) -> Result<Transaction<'static, Postgres>, SurfacePolicyStoreError> {
    let mut transaction = database
        .begin()
        .await
        .map_err(SurfacePolicyStoreError::Database)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *transaction)
    .await
    .map_err(SurfacePolicyStoreError::Database)?;
    Ok(transaction)
}

fn policy_key(prefix: &str, name: SurfacePolicyNameV2) -> String {
    let suffix = match name {
        SurfacePolicyNameV2::SurfaceWarningAcknowledgement => "surface_warning_acknowledgement",
        SurfacePolicyNameV2::AllowNewExactHeadwordEntries => "allow_new_exact_headword_entries",
        SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications => {
            "allow_multiple_active_exact_headword_publications"
        }
    };
    format!("{prefix}{suffix}")
}

fn transition_policy(
    current: SurfaceCreationPolicy,
    enabled: bool,
) -> Result<SurfaceCreationPolicy, SurfacePolicyStoreError> {
    if current.enabled == enabled {
        return Ok(current);
    }
    Ok(SurfaceCreationPolicy {
        enabled,
        epoch: current
            .epoch
            .checked_add(1)
            .ok_or(SurfacePolicyStoreError::EpochOverflow)?,
        ..current
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_phase_exact_headword_creation_gate_is_disabled() {
        let policy = exact_headword_creation_policy();
        assert!(!policy.enabled);
        assert_eq!(
            policy.name,
            SurfacePolicyNameV2::AllowNewExactHeadwordEntries
        );
        assert_eq!(policy.epoch, 1);
    }

    #[test]
    fn every_state_change_advances_epoch_and_same_state_is_idempotent() {
        let initial = exact_headword_creation_policy();
        let enabled = transition_policy(initial, true).unwrap();
        let repeated = transition_policy(enabled, true).unwrap();
        let disabled = transition_policy(repeated, false).unwrap();

        assert!(enabled.enabled);
        assert_eq!(enabled.epoch, 2);
        assert_eq!(repeated, enabled);
        assert!(!disabled.enabled);
        assert_eq!(disabled.epoch, 3);
    }

    #[test]
    fn ordinary_warning_acknowledgement_is_enabled_but_both_capability_gates_default_off() {
        assert!(default_policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement).enabled);
        assert!(!exact_headword_creation_policy().enabled);
        assert!(
            !default_policy(SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications)
                .enabled
        );
    }

    #[test]
    fn persisted_policy_keys_match_wire_names_used_by_atomic_snapshot_scripts() {
        for (name, suffix) in [
            (
                SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
                "surface_warning_acknowledgement",
            ),
            (
                SurfacePolicyNameV2::AllowNewExactHeadwordEntries,
                "allow_new_exact_headword_entries",
            ),
            (
                SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
                "allow_multiple_active_exact_headword_publications",
            ),
        ] {
            assert_eq!(
                policy_key(SURFACE_POLICY_PREFIX, name),
                format!("{SURFACE_POLICY_PREFIX}{suffix}")
            );
            assert_eq!(
                serde_json::to_string(&name).unwrap(),
                format!("\"{suffix}\"")
            );
        }
    }

    #[test]
    fn epoch_never_wraps() {
        let current = SurfaceCreationPolicy {
            enabled: false,
            epoch: u64::MAX,
            ..exact_headword_creation_policy()
        };
        assert!(matches!(
            transition_policy(current, true),
            Err(SurfacePolicyStoreError::EpochOverflow)
        ));
    }
}
