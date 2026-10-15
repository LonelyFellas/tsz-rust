use super::service::Tx;
use crate::error::AppError;
use uuid::Uuid;

// The caller's transaction makes delivery atomic with the new like or transfer.
pub(super) async fn notify(
    tx: &mut Tx<'_>,
    author: Uuid,
    actor: Uuid,
    wordlist: Uuid,
    amount: Option<i64>,
) -> Result<(), AppError> {
    let kind = if amount.is_some() {
        "wordlist_tipped"
    } else {
        "wordlist_liked"
    };
    sqlx::query("INSERT INTO user_notifications(id,user_id,kind,wordlist_id,actor_user_id,amount) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::now_v7()).bind(author).bind(kind).bind(wordlist).bind(actor).bind(amount)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
