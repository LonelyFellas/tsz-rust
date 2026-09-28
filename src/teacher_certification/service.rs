use super::model::*;
use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    platform::{Email, Phone},
};
use sqlx::{PgConnection, PgPool};
use std::collections::HashSet;
use uuid::Uuid;

pub fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

pub fn conflict() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "认证状态已变化，请刷新后重试",
    )
}

async fn lock_user(connection: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(connection)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("用户不存在"))?;
    Ok(())
}

pub async fn submit(
    pool: &PgPool,
    auth: &AuthUser,
    input: SubmitApplication,
) -> Result<TeacherApplication, AppError> {
    let name = input.real_name.trim();
    let statement = input.statement.trim();
    let contact = input.contact.trim();
    if name.is_empty()
        || name.chars().count() > 50
        || statement.is_empty()
        || statement.chars().count() > 2000
    {
        return Err(invalid("姓名为1至50字，认证说明为1至2000字"));
    }
    if Email::parse(contact).is_err() && Phone::parse(contact).is_err() {
        return Err(invalid("请填写有效的手机号或邮箱"));
    }
    if input.education_files.is_empty()
        || input.education_files.len() > 10
        || input.language_files.is_empty()
        || input.language_files.len() > 10
    {
        return Err(invalid("学历证书和语言成绩各需1至10张图片"));
    }
    let mut files = vec![(input.id_front, "id_front"), (input.id_back, "id_back")];
    files.extend(input.education_files.iter().map(|id| (*id, "education")));
    files.extend(input.language_files.iter().map(|id| (*id, "language")));
    if files.iter().map(|(id, _)| id).collect::<HashSet<_>>().len() != files.len() {
        return Err(invalid("同一材料不能重复提交"));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth.subject).await?;
    let allowed = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users u JOIN user_roles r ON r.user_id = u.id AND r.role = 'student' WHERE u.id = $1 AND u.status = 'active' AND u.security_version = $2)")
        .bind(auth.subject).bind(auth.security_version).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if !allowed {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "当前账号不能申请",
        ));
    }
    let blocked = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM teacher_applications WHERE user_id = $1 AND status IN ('pending','approved')) OR EXISTS(SELECT 1 FROM teacher_profiles WHERE user_id = $1 AND verified)")
        .bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if blocked {
        return Err(conflict());
    }
    let mut sorted = files.clone();
    sorted.sort_by_key(|(id, _)| *id);
    for (id, kind) in sorted {
        let valid = sqlx::query_scalar::<_, Uuid>("SELECT id FROM teacher_certification_files WHERE id = $1 AND user_id = $2 AND kind = $3 AND state = 'ready' AND (expires_at IS NULL OR expires_at > now()) FOR UPDATE")
            .bind(id).bind(auth.subject).bind(kind).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
        if valid.is_none() {
            return Err(invalid("材料无效、已过期或不属于当前账号"));
        }
    }
    let id = Uuid::now_v7();
    let application = sqlx::query_as::<_, TeacherApplication>("INSERT INTO teacher_applications (id,user_id,real_name,contact,statement) VALUES ($1,$2,$3,$4,$5) RETURNING *")
        .bind(id).bind(auth.subject).bind(name).bind(contact).bind(statement).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    for (position, (file, _)) in files.into_iter().enumerate() {
        sqlx::query("INSERT INTO teacher_application_files (application_id,file_id,position) VALUES ($1,$2,$3)")
            .bind(id).bind(file).bind(position as i16).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE teacher_certification_files SET expires_at = NULL WHERE id = $1")
            .bind(file)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(application)
}

pub async fn review(
    pool: &PgPool,
    id: Uuid,
    reviewer: Uuid,
    input: ReviewApplication,
) -> Result<TeacherApplication, AppError> {
    let reason = input
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if matches!(input.decision, ReviewDecision::Reject) && reason.is_none() {
        return Err(invalid("驳回必须填写原因"));
    }
    if reason.is_some_and(|s| s.chars().count() > 2000) {
        return Err(invalid("原因不得超过2000字"));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let owner =
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM teacher_applications WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("申请不存在"))?;
    lock_user(&mut tx, owner).await?;
    let (status, kind) = match input.decision {
        ReviewDecision::Approve => ("approved", "teacher_approved"),
        ReviewDecision::Reject => ("rejected", "teacher_rejected"),
    };
    let application = sqlx::query_as::<_, TeacherApplication>("UPDATE teacher_applications SET status = $2, reviewed_at = now(), reviewed_by = $3, review_reason = $4 WHERE id = $1 AND status = 'pending' RETURNING *")
        .bind(id).bind(status).bind(reviewer).bind(reason).fetch_optional(&mut *tx).await.map_err(AppError::internal)?
        .ok_or_else(conflict)?;
    if status == "approved" {
        sqlx::query("INSERT INTO teacher_profiles (user_id,verified) VALUES ($1,true) ON CONFLICT (user_id) DO UPDATE SET verified = true")
            .bind(owner).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query(
            "INSERT INTO user_roles (user_id,role) VALUES ($1,'teacher') ON CONFLICT DO NOTHING",
        )
        .bind(owner)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    }
    notify(&mut tx, owner, Some(id), kind, reason, reviewer).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(application)
}

pub async fn revoke(
    pool: &PgPool,
    owner: Uuid,
    reviewer: Uuid,
    reason: &str,
) -> Result<(), AppError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 2000 {
        return Err(invalid("撤销原因需填写1至2000字"));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, owner).await?;
    let changed =
        sqlx::query("UPDATE teacher_profiles SET verified = false WHERE user_id = $1 AND verified")
            .bind(owner)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .rows_affected();
    if changed == 0 {
        return Err(conflict());
    }
    let application = sqlx::query_scalar::<_, Uuid>("UPDATE teacher_applications SET status = 'revoked', revoked_at = now(), revoked_by = $2, revoke_reason = $3 WHERE user_id = $1 AND status = 'approved' RETURNING id")
        .bind(owner).bind(reviewer).bind(reason).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role = 'teacher'")
        .bind(owner)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE users SET last_active_role = 'student' WHERE id = $1 AND last_active_role = 'teacher'")
        .bind(owner).execute(&mut *tx).await.map_err(AppError::internal)?;
    notify(
        &mut tx,
        owner,
        application,
        "teacher_revoked",
        Some(reason),
        reviewer,
    )
    .await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}

async fn notify(
    connection: &mut PgConnection,
    user: Uuid,
    application: Option<Uuid>,
    kind: &str,
    reason: Option<&str>,
    actor: Uuid,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO user_notifications (id,user_id,application_id,kind,reason,actor_id) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::now_v7()).bind(user).bind(application).bind(kind).bind(reason).bind(actor)
        .execute(connection).await.map_err(AppError::internal)?;
    Ok(())
}
