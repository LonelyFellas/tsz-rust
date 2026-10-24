use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

/// 管理端首页看板的一次性聚合响应。
///
/// 所有「今日 / 本周 / 本月」均按北京时间（Asia/Shanghai）自然日 / 自然周 / 自然月。
/// 学习进度使用学习业务的换日边界（北京时间 04:00），与学习任务本身保持一致。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminDashboardOverview {
    pub generated_at: DateTime<Utc>,
    pub users: DashboardUsers,
    pub tasks: DashboardTasks,
    pub lexicon: DashboardLexicon,
    pub coins: DashboardCoins,
    /// 近 14 个自然日的新增用户，含零值日期。
    pub user_growth: Vec<DashboardDailyPoint>,
    /// 近 7 个自然日的平台天生币发放 / 扣除，含零值日期。
    pub coin_flow: Vec<DashboardCoinFlowPoint>,
    pub learning_progress: DashboardLearningProgress,
    /// 近 6 个自然周的词条创建量，按周一开始。
    pub lexicon_creation: Vec<DashboardWeeklyPoint>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardUsers {
    pub total: i64,
    pub today: i64,
    pub last_3_days: i64,
    pub last_7_days: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardTasks {
    pub total: i64,
    /// 至少完成过一轮的学习任务数。
    pub completed: i64,
    pub today: i64,
    pub week: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardLexicon {
    pub total: i64,
    pub today: i64,
    pub week: i64,
    pub month: i64,
}

/// 天生币金额使用十进制字符串，避免前端 JavaScript 精度损失。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardCoins {
    /// 所有用户钱包的当前余额合计。
    pub circulation: String,
    /// 平台净投放（发放 - 扣除），可为负。
    pub today_net: String,
    pub week_net: String,
    pub month_net: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardDailyPoint {
    /// 北京时间自然日，YYYY-MM-DD。
    pub date: String,
    pub value: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardWeeklyPoint {
    /// 北京时间周一，YYYY-MM-DD。
    pub week_start: String,
    pub value: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardCoinFlowPoint {
    pub date: String,
    /// 平台发放量，非负。
    pub grant: String,
    /// 平台扣除量，非负。
    pub deduct: String,
}

/// 有活跃学习任务的学生按今日学习状态划分。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardLearningProgress {
    /// 今日已完成至少一轮。
    pub completed: i64,
    /// 今日已开始作答但尚未完成。
    pub in_progress: i64,
    /// 今日没有开始学习。
    pub not_started: i64,
}
