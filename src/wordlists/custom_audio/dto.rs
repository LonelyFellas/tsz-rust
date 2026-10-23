use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioGeneration {
    #[schema(max_items = 10000)]
    pub item_ids: Vec<Uuid>,
    pub expected_cost: String,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct AudioQuery {
    /// Comma-separated current item IDs; at most 100.
    pub item_ids: String,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioStatuses {
    pub items: Vec<AudioStatus>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AudioStatus {
    pub item_id: Uuid,
    pub job_id: Uuid,
    pub word: String,
    pub status: AudioState,
    pub locales: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AudioState {
    Queued,
    Processing,
    Ready,
    Refunding,
    Refunded,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct SavedVoice {
    pub provider_voice_id: String,
    pub locale: String,
    pub provider_version: String,
}
