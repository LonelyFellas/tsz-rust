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
        practice_modes: crate::learning_tasks::mastery::capable(&headers)
            .then(|| vec![crate::learning_tasks::mastery::MODE.into()]),
        question_types: vec!["spelling_zh_to_en".into(), "choice_zh_to_en".into()],
    })
}
