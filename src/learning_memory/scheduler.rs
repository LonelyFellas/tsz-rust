use chrono::{DateTime, Duration, Utc};

pub(super) const SCHEDULER_VERSION: &str = "memory_ladder_v1";
const INTERVAL_DAYS: [i64; 7] = [1, 3, 7, 14, 30, 60, 90];

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub(super) enum Stage {
    Unverified,
    Learning,
    Reviewing,
    Relearning,
}
impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Learning => "learning",
            Self::Reviewing => "reviewing",
            Self::Relearning => "relearning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Schedule {
    pub stage: Stage,
    pub interval_step: i16,
    pub due_at: DateTime<Utc>,
    pub last_feedback_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Effect {
    InitialCorrect,
    InitialIncorrect,
    ReviewPassed,
    RelearningPassed,
    Lapsed,
    RelearningFailed,
    PracticeEarly,
}
impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialCorrect => "initial_correct",
            Self::InitialIncorrect => "initial_incorrect",
            Self::ReviewPassed => "review_passed",
            Self::RelearningPassed => "relearning_passed",
            Self::Lapsed => "lapsed",
            Self::RelearningFailed => "relearning_failed",
            Self::PracticeEarly => "practice_early",
        }
    }
}

pub(super) fn advance(
    current: Option<&Schedule>,
    correct: bool,
    at: DateTime<Utc>,
) -> (Schedule, Effect) {
    let Some(current) = current.filter(|s| s.stage != Stage::Unverified) else {
        return (
            Schedule {
                stage: if correct {
                    Stage::Learning
                } else {
                    Stage::Relearning
                },
                interval_step: 0,
                due_at: at
                    + if correct {
                        Duration::days(1)
                    } else {
                        Duration::minutes(10)
                    },
                last_feedback_at: Some(at),
            },
            if correct {
                Effect::InitialCorrect
            } else {
                Effect::InitialIncorrect
            },
        );
    };
    let mut next = current.clone();
    next.last_feedback_at = Some(at);
    if !correct {
        if current.stage == Stage::Relearning {
            // Keep the original due time; feedback still enforces a fresh cooldown.
            return (next, Effect::RelearningFailed);
        }
        next.stage = Stage::Relearning;
        next.interval_step = 0;
        next.due_at = at + Duration::minutes(10);
        return (next, Effect::Lapsed);
    }
    if at < current.due_at
        || current
            .last_feedback_at
            .is_some_and(|last| at < last + Duration::minutes(10))
    {
        return (next, Effect::PracticeEarly);
    }
    if current.stage == Stage::Relearning {
        next.stage = Stage::Learning;
        next.interval_step = 0;
        next.due_at = at + Duration::days(1);
        return (next, Effect::RelearningPassed);
    }
    next.stage = Stage::Reviewing;
    next.interval_step = (current.interval_step + 1).min(6);
    next.due_at = at + Duration::days(INTERVAL_DAYS[next.interval_step as usize]);
    (next, Effect::ReviewPassed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-23T19:59:00Z".parse().unwrap()
    }

    #[test]
    fn first_answer_and_four_am_do_not_imply_spaced_recall() {
        let at = now();
        let (initial, effect) = advance(None, true, at);
        assert_eq!(effect, Effect::InitialCorrect);
        assert_eq!(initial.due_at, at + Duration::days(1));
        let (early, effect) = advance(Some(&initial), true, at + Duration::minutes(2));
        assert_eq!(effect, Effect::PracticeEarly);
        assert_eq!(early.interval_step, 0);
        assert_eq!(early.due_at, initial.due_at);
        let (wrong, effect) = advance(None, false, at);
        assert_eq!(effect, Effect::InitialIncorrect);
        assert_eq!(wrong.stage, Stage::Relearning);
        assert_eq!(wrong.due_at, at + Duration::minutes(10));
    }

    #[test]
    fn due_success_advances_once_even_after_long_overdue_period() {
        let (mut state, _) = advance(None, true, now());
        for expected in [3, 7, 14, 30, 60, 90, 90] {
            let at = state.due_at + Duration::days(100);
            let (next, effect) = advance(Some(&state), true, at);
            assert_eq!(effect, Effect::ReviewPassed);
            assert_eq!(next.due_at, at + Duration::days(expected));
            state = next;
        }
        assert_eq!(state.interval_step, 6);
    }

    #[test]
    fn relearning_success_always_restarts_at_one_day() {
        let at = now();
        let mature = Schedule {
            stage: Stage::Reviewing,
            interval_step: 5,
            due_at: at,
            last_feedback_at: Some(at - Duration::days(60)),
        };
        let (failed, effect) = advance(Some(&mature), false, at);
        assert_eq!(effect, Effect::Lapsed);
        let (recovered, effect) = advance(Some(&failed), true, at + Duration::minutes(11));
        assert_eq!(effect, Effect::RelearningPassed);
        assert_eq!(recovered.stage, Stage::Learning);
        assert_eq!(recovered.interval_step, 0);
        assert_eq!(
            recovered.due_at,
            at + Duration::minutes(11) + Duration::days(1)
        );
    }

    #[test]
    fn repeated_failures_keep_due_time_but_refresh_feedback_cooldown() {
        let at = now();
        let (failed, _) = advance(None, false, at);
        let (again, effect) = advance(Some(&failed), false, at + Duration::minutes(20));
        assert_eq!(effect, Effect::RelearningFailed);
        assert_eq!(again.due_at, failed.due_at);
        let (early, effect) = advance(Some(&again), true, at + Duration::minutes(21));
        assert_eq!(effect, Effect::PracticeEarly);
        assert_eq!(early.stage, Stage::Relearning);
        assert_eq!(early.due_at, failed.due_at);
        let (recovered, effect) = advance(Some(&early), true, at + Duration::minutes(31));
        assert_eq!(effect, Effect::RelearningPassed);
        assert_eq!(
            recovered.due_at,
            at + Duration::minutes(31) + Duration::days(1)
        );
    }

    #[test]
    fn unverified_content_needs_a_new_first_answer() {
        let at = now();
        let unknown = Schedule {
            stage: Stage::Unverified,
            interval_step: 0,
            due_at: at - Duration::days(10),
            last_feedback_at: None,
        };
        for correct in [true, false] {
            assert_eq!(
                advance(Some(&unknown), correct, at),
                advance(None, correct, at)
            );
        }
    }
}
