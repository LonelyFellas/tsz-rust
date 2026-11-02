use super::dto::LearningCapabilities;
use crate::state::AppState;
use axum::{Json, Router, routing::get};
pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/me/learning-capabilities", get(capabilities))
}
#[utoipa::path(get,path="/api/v1/me/learning-capabilities",tag="learning_plans",security(("bearer_auth"=[])),responses((status=200,description="支持的学习题型",body=LearningCapabilities),(status=401,description="需要登录")))]
pub async fn capabilities(
    _auth: crate::auth::extract::AuthUser,
    headers: axum::http::HeaderMap,
) -> Json<LearningCapabilities> {
    Json(LearningCapabilities {
        practice_features: super::learning::practice_config_capable(&headers)
            .then(|| vec!["mixed_types_v1".into(), "resume_wordlist_v1".into()]),
        choice_policies: super::learning::strict_capable(&headers).then(|| {
            let mut policies = vec![super::learning::STRICT_POLICY.into()];
            if super::learning::practice_config_capable(&headers) {
                policies.push(super::learning::MIXED_POLICY.into());
            }
            policies
        }),
        practice_modes: crate::learning_tasks::mastery::capable(&headers)
            .then(|| vec![crate::learning_tasks::mastery::MODE.into()]),
        question_types: vec!["spelling_zh_to_en".into(), "choice_zh_to_en".into()],
    })
}
