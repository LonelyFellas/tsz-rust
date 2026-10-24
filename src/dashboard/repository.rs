use chrono::Utc;
use sqlx::{FromRow, PgPool};

use super::dto::{
    AdminDashboardOverview, DashboardCoinFlowPoint, DashboardCoins, DashboardDailyPoint,
    DashboardLearningProgress, DashboardLexicon, DashboardTasks, DashboardUsers,
    DashboardWeeklyPoint,
};

#[derive(FromRow)]
struct UsersRow {
    total: i64,
    today: i64,
    last_3_days: i64,
    last_7_days: i64,
}

#[derive(FromRow)]
struct TasksRow {
    total: i64,
    completed: i64,
    today: i64,
    week: i64,
}

#[derive(FromRow)]
struct LexiconRow {
    total: i64,
    today: i64,
    week: i64,
    month: i64,
}

#[derive(FromRow)]
struct CoinsRow {
    circulation: String,
    today_net: String,
    week_net: String,
    month_net: String,
}

#[derive(FromRow)]
struct LearningProgressRow {
    completed: i64,
    in_progress: i64,
    not_started: i64,
}

#[derive(FromRow)]
struct GrowthRow {
    date: String,
    value: i64,
}

#[derive(FromRow)]
struct WeeklyRow {
    week_start: String,
    value: i64,
}

#[derive(FromRow)]
struct CoinFlowRow {
    date: String,
    grant: String,
    deduct: String,
}

/// 读取看板聚合数据。所有查询在同一个只读、可重复读事务内执行，
/// 保证同一次响应里的数字来自同一数据库快照。
pub async fn load(pool: &PgPool, include_v3: bool) -> Result<AdminDashboardOverview, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;

    let users = sqlx::query_as::<_, UsersRow>(USERS_SQL)
        .fetch_one(&mut *tx)
        .await?;
    let user_growth = sqlx::query_as::<_, GrowthRow>(USER_GROWTH_SQL)
        .fetch_all(&mut *tx)
        .await?;
    let tasks = sqlx::query_as::<_, TasksRow>(TASKS_SQL)
        .fetch_one(&mut *tx)
        .await?;
    let lexicon = sqlx::query_as::<_, LexiconRow>(LEXICON_SQL)
        .bind(include_v3)
        .fetch_one(&mut *tx)
        .await?;
    let lexicon_creation = sqlx::query_as::<_, WeeklyRow>(LEXICON_CREATION_SQL)
        .bind(include_v3)
        .fetch_all(&mut *tx)
        .await?;
    let coins = sqlx::query_as::<_, CoinsRow>(COINS_SQL)
        .fetch_one(&mut *tx)
        .await?;
    let coin_flow = sqlx::query_as::<_, CoinFlowRow>(COIN_FLOW_SQL)
        .fetch_all(&mut *tx)
        .await?;
    let learning_progress = sqlx::query_as::<_, LearningProgressRow>(LEARNING_PROGRESS_SQL)
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(AdminDashboardOverview {
        generated_at: Utc::now(),
        users: DashboardUsers {
            total: users.total,
            today: users.today,
            last_3_days: users.last_3_days,
            last_7_days: users.last_7_days,
        },
        tasks: DashboardTasks {
            total: tasks.total,
            completed: tasks.completed,
            today: tasks.today,
            week: tasks.week,
        },
        lexicon: DashboardLexicon {
            total: lexicon.total,
            today: lexicon.today,
            week: lexicon.week,
            month: lexicon.month,
        },
        coins: DashboardCoins {
            circulation: coins.circulation,
            today_net: coins.today_net,
            week_net: coins.week_net,
            month_net: coins.month_net,
        },
        user_growth: user_growth
            .into_iter()
            .map(|point| DashboardDailyPoint {
                date: point.date,
                value: point.value,
            })
            .collect(),
        coin_flow: coin_flow
            .into_iter()
            .map(|point| DashboardCoinFlowPoint {
                date: point.date,
                grant: point.grant,
                deduct: point.deduct,
            })
            .collect(),
        learning_progress: DashboardLearningProgress {
            completed: learning_progress.completed,
            in_progress: learning_progress.in_progress,
            not_started: learning_progress.not_started,
        },
        lexicon_creation: lexicon_creation
            .into_iter()
            .map(|point| DashboardWeeklyPoint {
                week_start: point.week_start,
                value: point.value,
            })
            .collect(),
    })
}

const USERS_SQL: &str = r#"
WITH bounds AS (
    SELECT (now() AT TIME ZONE 'Asia/Shanghai')::date AS today
)
SELECT
    count(*)::bigint AS total,
    count(*) FILTER (
        WHERE (u.created_at AT TIME ZONE 'Asia/Shanghai')::date = b.today
    )::bigint AS today,
    count(*) FILTER (
        WHERE (u.created_at AT TIME ZONE 'Asia/Shanghai')::date
              BETWEEN b.today - 2 AND b.today
    )::bigint AS last_3_days,
    count(*) FILTER (
        WHERE (u.created_at AT TIME ZONE 'Asia/Shanghai')::date
              BETWEEN b.today - 6 AND b.today
    )::bigint AS last_7_days
FROM users u
CROSS JOIN bounds b
"#;

const USER_GROWTH_SQL: &str = r#"
WITH bounds AS (
    SELECT (now() AT TIME ZONE 'Asia/Shanghai')::date AS today
), days AS (
    SELECT generate_series(
        (b.today - 13)::timestamp,
        b.today::timestamp,
        interval '1 day'
    )::date AS day
    FROM bounds b
)
SELECT
    to_char(d.day, 'YYYY-MM-DD') AS date,
    count(u.id)::bigint AS value
FROM days d
LEFT JOIN users u
    ON (u.created_at AT TIME ZONE 'Asia/Shanghai')::date = d.day
GROUP BY d.day
ORDER BY d.day
"#;

const TASKS_SQL: &str = r#"
WITH bounds AS (
    SELECT
        (now() AT TIME ZONE 'Asia/Shanghai')::date AS today,
        date_trunc('week', now() AT TIME ZONE 'Asia/Shanghai') AS week_start
)
SELECT
    count(*)::bigint AS total,
    count(*) FILTER (
        WHERE EXISTS (
            SELECT 1 FROM learning_completions c WHERE c.task_id = t.id
        )
    )::bigint AS completed,
    count(*) FILTER (
        WHERE (t.created_at AT TIME ZONE 'Asia/Shanghai')::date = b.today
    )::bigint AS today,
    count(*) FILTER (
        WHERE (t.created_at AT TIME ZONE 'Asia/Shanghai') >= b.week_start
    )::bigint AS week
FROM learning_tasks t
CROSS JOIN bounds b
"#;

const LEXICON_SQL: &str = r#"
WITH bounds AS (
    SELECT
        (now() AT TIME ZONE 'Asia/Shanghai')::date AS today,
        date_trunc('week', now() AT TIME ZONE 'Asia/Shanghai') AS week_start,
        date_trunc('month', now() AT TIME ZONE 'Asia/Shanghai') AS month_start
)
SELECT
    count(*)::bigint AS total,
    count(*) FILTER (
        WHERE (e.created_at AT TIME ZONE 'Asia/Shanghai')::date = b.today
    )::bigint AS today,
    count(*) FILTER (
        WHERE (e.created_at AT TIME ZONE 'Asia/Shanghai') >= b.week_start
    )::bigint AS week,
    count(*) FILTER (
        WHERE (e.created_at AT TIME ZONE 'Asia/Shanghai') >= b.month_start
    )::bigint AS month
FROM lexicon.entries e
CROSS JOIN bounds b
WHERE e.archived_at IS NULL
  AND ($1 OR e.content_schema_version = 2)
"#;

const LEXICON_CREATION_SQL: &str = r#"
WITH bounds AS (
    SELECT date_trunc('week', now() AT TIME ZONE 'Asia/Shanghai')::date AS current_week
), weeks AS (
    SELECT generate_series(
        (b.current_week - interval '5 weeks')::timestamp,
        b.current_week::timestamp,
        interval '1 week'
    )::date AS week_start
    FROM bounds b
)
SELECT
    to_char(w.week_start, 'YYYY-MM-DD') AS week_start,
    count(e.id)::bigint AS value
FROM weeks w
LEFT JOIN lexicon.entries e
    ON date_trunc('week', e.created_at AT TIME ZONE 'Asia/Shanghai')::date = w.week_start
   AND e.archived_at IS NULL
   AND ($1 OR e.content_schema_version = 2)
GROUP BY w.week_start
ORDER BY w.week_start
"#;

const COINS_SQL: &str = r#"
WITH bounds AS (
    SELECT
        (now() AT TIME ZONE 'Asia/Shanghai')::date AS today,
        date_trunc('week', now() AT TIME ZONE 'Asia/Shanghai') AS week_start,
        date_trunc('month', now() AT TIME ZONE 'Asia/Shanghai') AS month_start
), platform_entries AS (
    SELECT e.delta, e.created_at
    FROM coin_entries e
    JOIN coin_operations o ON o.id = e.operation_id
    JOIN coin_wallets w ON w.id = e.wallet_id
    WHERE w.owner_type = 'user'
      AND o.source_type <> 'wordlist_tip'
)
SELECT
    (SELECT COALESCE(sum(balance), 0)::bigint::text
       FROM coin_wallets WHERE owner_type = 'user') AS circulation,
    COALESCE(sum(p.delta) FILTER (
        WHERE (p.created_at AT TIME ZONE 'Asia/Shanghai')::date = b.today
    ), 0)::bigint::text AS today_net,
    COALESCE(sum(p.delta) FILTER (
        WHERE (p.created_at AT TIME ZONE 'Asia/Shanghai') >= b.week_start
    ), 0)::bigint::text AS week_net,
    COALESCE(sum(p.delta) FILTER (
        WHERE (p.created_at AT TIME ZONE 'Asia/Shanghai') >= b.month_start
    ), 0)::bigint::text AS month_net
FROM platform_entries p
CROSS JOIN bounds b
"#;

const COIN_FLOW_SQL: &str = r#"
WITH bounds AS (
    SELECT (now() AT TIME ZONE 'Asia/Shanghai')::date AS today
), days AS (
    SELECT generate_series(
        (b.today - 6)::timestamp,
        b.today::timestamp,
        interval '1 day'
    )::date AS day
    FROM bounds b
), flows AS (
    SELECT
        (e.created_at AT TIME ZONE 'Asia/Shanghai')::date AS day,
        sum(e.delta) FILTER (WHERE e.delta > 0) AS grant,
        sum(-e.delta) FILTER (WHERE e.delta < 0) AS deduct
    FROM coin_entries e
    JOIN coin_operations o ON o.id = e.operation_id
    JOIN coin_wallets w ON w.id = e.wallet_id
    CROSS JOIN bounds b
    WHERE w.owner_type = 'user'
      AND o.source_type <> 'wordlist_tip'
      AND e.created_at >= ((b.today - 6)::timestamp AT TIME ZONE 'Asia/Shanghai')
    GROUP BY 1
)
SELECT
    to_char(d.day, 'YYYY-MM-DD') AS date,
    COALESCE(f.grant, 0)::bigint::text AS grant,
    COALESCE(f.deduct, 0)::bigint::text AS deduct
FROM days d
LEFT JOIN flows f ON f.day = d.day
ORDER BY d.day
"#;

const LEARNING_PROGRESS_SQL: &str = r#"
WITH bounds AS (
    SELECT
        ((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date AS business_day,
        (((((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date)::timestamp)
            + time '04:00') AT TIME ZONE 'Asia/Shanghai' AS day_start,
        (((((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date + 1)::timestamp)
            + time '04:00') AT TIME ZONE 'Asia/Shanghai' AS day_end
), students AS (
    SELECT DISTINCT t.user_id
    FROM learning_tasks t
    JOIN users u ON u.id = t.user_id
    WHERE t.state = 'active'
      AND u.status = 'active'
      AND NOT EXISTS (
          SELECT 1
          FROM account_deletion_requests d
          WHERE d.user_id = u.id
            AND d.status = 'pending'
            AND d.effective_at <= clock_timestamp()
      )
), completed AS (
    SELECT DISTINCT c.user_id
    FROM learning_completions c
    CROSS JOIN bounds b
    WHERE c.completed_at >= b.day_start
      AND c.completed_at < b.day_end
), started AS (
    SELECT DISTINCT r.user_id
    FROM learning_runs r
    CROSS JOIN bounds b
    WHERE r.state = 'active'
      AND (
          (r.started_at >= b.day_start AND r.started_at < b.day_end)
          OR (r.task_type = 'daily' AND r.business_day = b.business_day)
      )
), answered AS (
    SELECT DISTINCT r.user_id
    FROM learning_answers a
    JOIN learning_runs r ON r.id = a.run_id
    CROSS JOIN bounds b
    WHERE r.state = 'active'
      AND a.accepted_at >= b.day_start
      AND a.accepted_at < b.day_end
), active AS (
    SELECT user_id FROM started
    UNION
    SELECT user_id FROM answered
)
SELECT
    count(*) FILTER (
        WHERE s.user_id IN (SELECT user_id FROM completed)
    )::bigint AS completed,
    count(*) FILTER (
        WHERE s.user_id NOT IN (SELECT user_id FROM completed)
          AND s.user_id IN (SELECT user_id FROM active)
    )::bigint AS in_progress,
    count(*) FILTER (
        WHERE s.user_id NOT IN (SELECT user_id FROM completed)
          AND s.user_id NOT IN (SELECT user_id FROM active)
    )::bigint AS not_started
FROM students s
"#;
