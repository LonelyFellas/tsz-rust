//! V1 templates render structured records at read time, never at creation/retry time.
use super::{InterfaceLocaleV1 as Locale, output::current_locale};
use crate::teacher_certification::model::UserNotification;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalizedNotification {
    pub locale: Locale,
    pub title: String,
    pub body: String,
}

fn text(locale: Locale, zh: &'static str, hant: &'static str, en: &'static str) -> &'static str {
    match locale {
        Locale::ZhHans => zh,
        Locale::ZhHant => hant,
        Locale::EnGb | Locale::EnUs => en,
    }
}

impl UserNotification {
    pub fn localize(&mut self) {
        self.localized = current_locale().and_then(|locale| self.render(locale));
    }

    fn render(&self, locale: Locale) -> Option<LocalizedNotification> {
        let t = |zh, hant, en| text(locale, zh, hant, en);
        let title = match self.kind.as_str() {
            "teacher_approved" => t(
                "教师认证已通过",
                "教師認證已通過",
                "Teacher certification approved",
            ),
            "teacher_rejected" => t(
                "教师申请已驳回",
                "教師申請已駁回",
                "Teacher application rejected",
            ),
            "teacher_revoked" => t(
                "教师资格已撤销",
                "教師資格已撤銷",
                "Teacher qualification revoked",
            ),
            "wordlist_liked" => t("词表收到点赞", "詞表收到讚", "Word list liked"),
            "wordlist_tipped" => t("词表收到投币", "詞表收到投幣", "Word list tipped"),
            "wordlist_deleted" => t(
                "使用中的词表已删除",
                "使用中的詞表已刪除",
                "A word list you use was deleted",
            ),
            _ => return None,
        };
        let actor = self
            .actor_name
            .as_deref()
            .unwrap_or_else(|| t("账号已注销", "帳號已註銷", "Deleted account"));
        let wordlist = self
            .wordlist_name
            .as_deref()
            .unwrap_or_else(|| t("（已删除）", "（已刪除）", "(deleted)"));
        // Format the decimal string without numeric conversion or loss of precision.
        let amount = self.amount.as_deref().map(|value| {
            let grouped: String = value
                .chars()
                .enumerate()
                .flat_map(|(i, c)| {
                    let separator = i > 0 && (value.len() - i).is_multiple_of(3);
                    separator
                        .then_some(',')
                        .into_iter()
                        .chain(std::iter::once(c))
                })
                .collect();
            format!(
                "{grouped} {}",
                t(
                    "天生币",
                    "天生幣",
                    if value == "1" {
                        "Tiansheng coin"
                    } else {
                        "Tiansheng coins"
                    }
                )
            )
        });
        let body = match self.kind.as_str() {
            "teacher_approved" => t("你已获得教师身份，可在账号菜单中切换工作台，原有学生身份保持不变。", "你已獲得教師身分，可在帳號選單中切換工作臺，原有學生身分保持不變。", "You now have a teacher role. Switch workspaces in the account menu; your student role is unchanged.").to_owned(),
            "wordlist_liked" => match locale {
                Locale::ZhHans => format!("{actor}赞了你的词表「{wordlist}」。"),
                Locale::ZhHant => format!("{actor}讚了你的詞表「{wordlist}」。"),
                Locale::EnGb | Locale::EnUs => format!("{actor} liked your word list “{wordlist}”."),
            },
            "wordlist_tipped" => match (locale, amount) {
                (Locale::ZhHans, Some(amount)) => format!("{actor}向你的词表「{wordlist}」投了 {amount}。"),
                (Locale::ZhHant, Some(amount)) => format!("{actor}向你的詞表「{wordlist}」投了 {amount}。"),
                (Locale::EnGb | Locale::EnUs, Some(amount)) => format!("{actor} tipped {amount} to your word list “{wordlist}”."),
                (Locale::ZhHans, None) => format!("{actor}向你的词表「{wordlist}」投币。"),
                (Locale::ZhHant, None) => format!("{actor}向你的詞表「{wordlist}」投幣。"),
                (Locale::EnGb | Locale::EnUs, None) => format!("{actor} tipped your word list “{wordlist}”."),
            },
            "wordlist_deleted" => self.reason.as_deref().and_then(|reason| deletion_body(reason, locale)).unwrap_or_default(),
            // Review reasons remain separate, unmodified user content.
            _ => String::new(),
        };
        Some(LocalizedNotification {
            locale,
            title: title.to_owned(),
            body,
        })
    }
}

// These are versioned legacy system templates, not user prose. Only the exact
// whole template for this notification kind is recognized; unknown records keep
// their original reason. Stored timestamps and names are never rewritten.
fn deletion_body(reason: &str, locale: Locale) -> Option<String> {
    let content = reason.strip_prefix("词表「")?;
    let (name, rest) = content.rsplit_once("」已被删除。")?;
    let deadline = rest
        .strip_prefix(
            "已开始的学习轮次请在本轮正常截止前完成：日任务仍受当天窗口限制，长期任务最晚可在 ",
        )?
        .strip_suffix("（北京时间）前完成。完成后任务将结束；未开始的轮次无法再开始。")?;
    chrono::NaiveDateTime::parse_from_str(deadline, "%Y-%m-%d %H:%M").ok()?;
    if locale == Locale::ZhHans {
        return Some(reason.to_owned());
    }
    Some(match locale {
        Locale::ZhHant => format!(
            "詞表「{name}」已被刪除。已開始的學習回合請在本回合正常截止前完成：每日任務仍受當天時段限制，長期任務最晚可在 {deadline}（北京時間）前完成。完成後任務將結束；尚未開始的回合無法再開始。"
        ),
        _ => format!(
            "The word list “{name}” has been deleted. Finish any started learning run before its normal deadline: daily tasks still follow their daily window; long-term tasks must finish by {deadline} (Beijing time, UTC+8). The task ends afterwards; new runs cannot be started."
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn old_records_render_in_all_locales_without_rewriting_parameters() {
        let mut item = UserNotification {
            id: Uuid::now_v7(),
            application_id: None,
            wordlist_id: None,
            kind: "wordlist_tipped".into(),
            wordlist_name: Some("原始词表 {{actor}} <b>".into()),
            actor_name: Some("原始姓名 {{wordlist}}".into()),
            amount: Some("9223372036854775807".into()),
            reason: Some("原始原因 <script>".into()),
            created_at: Utc::now(),
            read_at: None,
            localized: None,
        };
        let original = serde_json::to_value(&item).unwrap();
        assert!(original.get("localized").is_none());
        for locale in [Locale::ZhHans, Locale::ZhHant, Locale::EnGb, Locale::EnUs] {
            let rendered = item.render(locale).unwrap();
            assert!(!rendered.title.is_empty());
            assert!(rendered.body.contains("9,223,372,036,854,775,807"));
            assert!(
                rendered
                    .body
                    .contains(item.wordlist_name.as_deref().unwrap())
            );
            assert!(rendered.body.contains(item.actor_name.as_deref().unwrap()));
            assert!(!rendered.body.contains("<script>"));
            for kind in [
                "teacher_approved",
                "teacher_rejected",
                "teacher_revoked",
                "wordlist_liked",
                "wordlist_deleted",
            ] {
                item.kind = kind.into();
                assert!(!item.render(locale).unwrap().title.is_empty());
            }
            item.kind = "wordlist_tipped".into();
        }
        assert_eq!(serde_json::to_value(&item).unwrap(), original);
        item.kind = "future_notification".into();
        assert!(item.render(Locale::EnUs).is_none());
    }
}
