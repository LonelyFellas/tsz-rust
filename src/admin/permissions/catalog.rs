use std::collections::BTreeSet;

use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::error::{AppError, ErrorCode};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PermissionDefinition {
    pub key: &'static str,
    pub module_key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: &'static str,
    pub requires: &'static [&'static str],
    pub risk_level: &'static str,
}

macro_rules! permission {
    ($key:literal, $module:literal, $label:literal, $kind:literal, [$($requires:literal),*], $risk:literal) => {
        permission!($key, $module, $label, $label, $kind, [$($requires),*], $risk)
    };
    ($key:literal, $module:literal, $label:literal, $description:literal, $kind:literal, [$($requires:literal),*], $risk:literal) => {
        PermissionDefinition {
            key: $key, module_key: $module, label: $label, description: $description,
            kind: $kind, requires: &[$($requires),*], risk_level: $risk,
        }
    };
}

pub const CATALOG: &[PermissionDefinition] = &[
    permission!(
        "wordlists.create",
        "wordlists",
        "创建词表",
        "action",
        ["wordlists.access"],
        "medium"
    ),
    permission!(
        "wordlists.edit",
        "wordlists",
        "编辑词表",
        "action",
        ["wordlists.access"],
        "medium"
    ),
    permission!(
        "wordlists.visibility",
        "wordlists",
        "设置词表公开状态",
        "action",
        ["wordlists.access"],
        "high"
    ),
    permission!(
        "wordlists.recommend",
        "wordlists",
        "推荐词表",
        "action",
        ["wordlists.access"],
        "medium"
    ),
    permission!(
        "wordlists.delete",
        "wordlists",
        "删除词表",
        "action",
        ["wordlists.access"],
        "high"
    ),
    permission!(
        "wordlists.access",
        "wordlists",
        "词表查看",
        "page",
        [],
        "low"
    ),
    permission!(
        "wordlists.review",
        "wordlists",
        "词表审核",
        "action",
        ["wordlists.access"],
        "medium"
    ),
    permission!(
        "wordlists.withdraw",
        "wordlists",
        "词表下架",
        "action",
        ["wordlists.access"],
        "high"
    ),
    permission!("coins.access", "coins", "天生币查询", "page", [], "medium"),
    permission!(
        "coins.credit",
        "coins",
        "人工入账",
        "action",
        ["coins.access"],
        "high"
    ),
    permission!(
        "coins.reverse",
        "coins",
        "人工入账冲正",
        "action",
        ["coins.access"],
        "high"
    ),
    permission!(
        "words.access",
        "words",
        "词条查看",
        "查看后台词条",
        "page",
        [],
        "low"
    ),
    permission!(
        "words.detect",
        "words",
        "词形识别",
        "识别词形，为新建词条做准备",
        "action",
        ["words.access"],
        "medium"
    ),
    permission!(
        "words.validate",
        "words",
        "词条校验",
        "校验词条内容，预览形态变更的影响",
        "action",
        ["words.access"],
        "low"
    ),
    permission!(
        "words.create",
        "words",
        "词条创建",
        "创建归属于自己的词条",
        "action",
        ["words.access"],
        "medium"
    ),
    permission!(
        "words.edit",
        "words",
        "词条文本与音频编辑",
        "编辑自己的词条文本、音频及其他内容；单词和短语关联需另行授权",
        "action",
        ["words.access"],
        "medium"
    ),
    permission!(
        "words.associate",
        "words",
        "词条单词与短语关联",
        "编辑自己词条正文中的单词、短语及词形关联；不授予文本和音频编辑权限",
        "action",
        ["words.access"],
        "medium"
    ),
    permission!(
        "words.edit_others",
        "words",
        "可编辑他人的词条",
        "将已获授权的内容编辑和关联操作扩展至他人的词条；不授予操作权限",
        "scope",
        ["words.access"],
        "high"
    ),
    permission!(
        "words.publish",
        "words",
        "词条发布",
        "发布自己的词条",
        "action",
        ["words.access"],
        "high"
    ),
    permission!(
        "words.archive",
        "words",
        "词条归档",
        "归档自己的词条",
        "action",
        ["words.access"],
        "high"
    ),
    permission!(
        "words.restore",
        "words",
        "从垃圾桶恢复",
        "将已删入垃圾桶的词条恢复到智能词库中，重返可编辑状态",
        "action",
        ["words.access"],
        "high"
    ),
    permission!(
        "words.rollback",
        "words",
        "恢复到历史版本",
        "将可编辑词条恢复到它的某个历史版本",
        "action",
        ["words.access"],
        "high"
    ),
    permission!(
        "sentences.access",
        "sentences",
        "例句查看",
        "查看后台共享例句",
        "page",
        [],
        "low"
    ),
    permission!(
        "sentences.create",
        "sentences",
        "例句创建",
        "创建归属于自己的共享例句",
        "action",
        ["sentences.access", "words.edit"],
        "medium"
    ),
    permission!(
        "sentences.edit",
        "sentences",
        "例句文本与音频编辑",
        "编辑自己的例句文本、音频及其他内容；单词和短语关联需另行授权",
        "action",
        ["sentences.access", "words.access"],
        "medium"
    ),
    permission!(
        "sentences.associate",
        "sentences",
        "例句单词与短语关联",
        "编辑自己例句正文中的单词、短语及词形关联；不授予文本和音频编辑权限",
        "action",
        ["sentences.access", "words.access"],
        "medium"
    ),
    permission!(
        "sentences.edit_others",
        "sentences",
        "可编辑他人的例句",
        "将已获授权的内容编辑和关联操作扩展至他人的例句；不授予操作权限",
        "scope",
        ["sentences.access"],
        "high"
    ),
    permission!(
        "sentences.publish",
        "sentences",
        "例句发布",
        "发布自己的共享例句",
        "action",
        ["sentences.access"],
        "high"
    ),
    permission!(
        "sentences.withdraw",
        "sentences",
        "例句撤回",
        "撤回自己已发布的共享例句",
        "action",
        ["sentences.access"],
        "high"
    ),
    permission!(
        "sentences.restore",
        "sentences",
        "恢复例句展示",
        "恢复自己已下架的共享例句展示",
        "action",
        ["sentences.access"],
        "high"
    ),
    permission!(
        "sentences.rollback",
        "sentences",
        "恢复到历史版本",
        "将自己的共享例句恢复到历史发布版本",
        "action",
        ["sentences.access"],
        "high"
    ),
    permission!(
        "users.access",
        "users",
        "查看全部用户名单",
        "查看平台内所有用户的昵称和注册时间",
        "page",
        [],
        "medium"
    ),
    permission!(
        "users.read_basic",
        "users",
        "查看用户基本信息",
        "查看用户 ID、头像、昵称、角色、认证和账号状态",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.read_coin_balance",
        "users",
        "查看用户天生币余额",
        "查看用户当前天生币余额",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.read_coin_entries",
        "users",
        "查看用户天生币账单",
        "查看用户天生币收支账单；逐笔余额另需余额权限",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.read_identity",
        "users",
        "查看用户身份证件",
        "查看用户最近一次教师认证申请中的身份证正反面",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.read_sensitive",
        "users",
        "查看用户电话和邮箱",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.edit",
        "users",
        "编辑用户资料",
        "action",
        ["users.access"],
        "high"
    ),
    permission!(
        "users.set_status",
        "users",
        "封禁用户",
        "封禁或解除封禁用户账号",
        "action",
        ["users.read_basic"],
        "high"
    ),
    permission!(
        "teacherapply.access",
        "teacherapply",
        "查看教师资格申请列表",
        "查看平台所有申请教师认证的名单，包括姓名、联系方式、认证说明、申请时间及审核和撤销结果",
        "page",
        [],
        "medium"
    ),
    permission!(
        "teacherapply.review",
        "teacherapply",
        "审核教师资格",
        "有权审核教师资格，通过或驳回教师资格申请",
        "action",
        ["teacherapply.access"],
        "high"
    ),
    permission!(
        "teacherapply.revoke",
        "teacherapply",
        "撤销教师资格",
        "有权撤销已通过认证的教师资格",
        "action",
        ["teacherapply.access"],
        "high"
    ),
    permission!(
        "teacherapply.read_certificates",
        "teacherapply",
        "查看教师资格证书",
        "有权查看教师认证过程中上传的学历和语言资格证书",
        "action",
        ["teacherapply.access"],
        "high"
    ),
    permission!(
        "teacherapply.read_identity",
        "teacherapply",
        "查看个人身份证",
        "有权查看教师认证过程中提供的个人身份证正反面",
        "action",
        ["teacherapply.access"],
        "high"
    ),
    permission!(
        "lexicon_settings.access",
        "lexicon_settings",
        "查看全部词性配置",
        "可以查看平台已经配置好的全部词性，包括基本词性、细分词性和词形变化",
        "page",
        [],
        "low"
    ),
    permission!(
        "lexicon_settings.edit_basic",
        "lexicon_settings",
        "编辑基本词性配置",
        "可以新增和编辑平台的基本词性配置",
        "action",
        ["lexicon_settings.access"],
        "high"
    ),
    permission!(
        "lexicon_settings.edit_sub",
        "lexicon_settings",
        "编辑细分词性配置",
        "可以新增和编辑平台的细分词性配置",
        "action",
        ["lexicon_settings.access"],
        "high"
    ),
    permission!(
        "lexicon_settings.edit_forms",
        "lexicon_settings",
        "编辑词形变化配置",
        "可以新增和编辑平台的词形变化配置",
        "action",
        ["lexicon_settings.access"],
        "high"
    ),
    permission!(
        "lexicon_settings.delete",
        "lexicon_settings",
        "删除词性或词形",
        "可以删除平台已经配置好的细分词性和词形变化；基本词性仅超级管理员可删除",
        "action",
        ["lexicon_settings.access"],
        "high"
    ),
    permission!(
        "speech.upload_audio",
        "words",
        "选择发音 – 上传音频",
        "能够在智能词库选择发音时，直接上传音频给文本配音。",
        "action",
        [],
        "medium"
    ),
    permission!(
        "speech.generate",
        "speech",
        "语音预览",
        "生成语音预览，会消耗语音服务资源。",
        "action",
        ["words.access"],
        "medium"
    ),
];

pub fn definition(key: &str) -> Option<&'static PermissionDefinition> {
    CATALOG.iter().find(|permission| permission.key == key)
}

pub fn catalog_version() -> String {
    let bytes = serde_json::to_vec(CATALOG).expect("static permission catalog serializes");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn all_keys() -> BTreeSet<String> {
    CATALOG
        .iter()
        .map(|permission| permission.key.to_owned())
        .collect()
}

pub fn validate_keys(keys: &[String]) -> Result<BTreeSet<String>, AppError> {
    if keys.len() > CATALOG.len() || keys.iter().any(|key| definition(key).is_none()) {
        return Err(invalid("unknown permission or excessive selection"));
    }
    let set: BTreeSet<_> = keys.iter().cloned().collect();
    if set.len() != keys.len() {
        return Err(invalid("duplicate permission"));
    }
    Ok(set)
}

pub fn validate_closed(keys: &BTreeSet<String>) -> Result<(), AppError> {
    for key in keys {
        let permission = definition(key).ok_or_else(|| invalid("unknown permission"))?;
        if permission
            .requires
            .iter()
            .any(|required| !keys.contains(*required))
        {
            return Err(invalid("permission dependency missing"));
        }
    }
    Ok(())
}

pub fn effective_keys(stored: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    let mut keys: BTreeSet<_> = stored
        .into_iter()
        .filter(|key| {
            if definition(key).is_some() {
                true
            } else {
                tracing::warn!("unknown administrator permission ignored");
                false
            }
        })
        .collect();
    loop {
        let invalid: Vec<_> = keys
            .iter()
            .filter(|key| {
                definition(key).is_some_and(|permission| {
                    permission
                        .requires
                        .iter()
                        .any(|required| !keys.contains(*required))
                })
            })
            .cloned()
            .collect();
        if invalid.is_empty() {
            return keys;
        }
        for key in invalid {
            keys.remove(&key);
        }
    }
}

pub fn expand_grants(keys: &mut BTreeSet<String>) {
    loop {
        let required: Vec<_> = keys
            .iter()
            .flat_map(|key| {
                definition(key)
                    .into_iter()
                    .flat_map(|permission| permission.requires)
            })
            .map(|key| (*key).to_owned())
            .collect();
        let before = keys.len();
        keys.extend(required);
        if keys.len() == before {
            break;
        }
    }
}

pub fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_permission_labels_are_separate_from_scope_descriptions() {
        let content: Vec<_> = CATALOG
            .iter()
            .filter(|permission| matches!(permission.module_key, "words" | "sentences"))
            .collect();
        assert_eq!(content.len(), 21);
        for permission in content {
            assert_ne!(
                permission.label, permission.description,
                "{}",
                permission.key
            );
            assert!(!permission.label.contains("本人"));
            assert!(!permission.label.contains("不包含"));
        }
        for (key, label, scope) in [
            ("words.edit", "词条文本与音频编辑", "自己的"),
            ("words.edit_others", "可编辑他人的词条", "不授予操作权限"),
            ("sentences.edit", "例句文本与音频编辑", "自己的"),
            (
                "sentences.edit_others",
                "可编辑他人的例句",
                "不授予操作权限",
            ),
        ] {
            let permission = definition(key).unwrap();
            assert_eq!(permission.label, label);
            assert!(permission.description.contains(scope));
        }
    }

    #[test]
    fn speech_permission_keeps_resource_notice_in_description() {
        let permission = definition("speech.generate").unwrap();
        assert_eq!(permission.label, "语音预览");
        assert_eq!(permission.description, "生成语音预览，会消耗语音服务资源。");
        assert_eq!(permission.requires, &["words.access"]);
    }

    #[test]
    fn catalog_is_unique_closed_and_acyclic() {
        assert_eq!(all_keys().len(), CATALOG.len());
        validate_closed(&all_keys()).unwrap();
        for permission in CATALOG {
            let mut visited = BTreeSet::new();
            fn visit(key: &str, visited: &mut BTreeSet<String>) {
                assert!(visited.insert(key.to_owned()), "cyclic dependency: {key}");
                for required in definition(key).unwrap().requires {
                    visit(required, visited);
                }
                visited.remove(key);
            }
            visit(permission.key, &mut visited);
        }
    }

    #[test]
    fn sentence_creation_requires_source_entry_edit_without_expanding_ownership() {
        let mut keys = BTreeSet::from(["sentences.create".to_owned()]);
        expand_grants(&mut keys);
        assert_eq!(
            keys,
            BTreeSet::from([
                "sentences.access".to_owned(),
                "sentences.create".to_owned(),
                "words.access".to_owned(),
                "words.edit".to_owned(),
            ])
        );
        let preview = crate::admin::permissions::service::preview_change(
            uuid::Uuid::nil(),
            0,
            &keys,
            &[],
            &["words.edit".to_owned()],
        )
        .unwrap();
        assert_eq!(preview.dependency_revocations, vec!["sentences.create"]);
        assert_eq!(preview.after, vec!["sentences.access", "words.access"]);
    }

    #[test]
    fn sentence_edit_preview_includes_target_reads_and_revocation_cascades() {
        use crate::admin::{
            authorization::{RoutePolicy, route_policy},
            permissions::{AdminAuthorization, service::preview_change},
        };

        let actor = uuid::Uuid::nil();
        let preview = preview_change(
            actor,
            0,
            &BTreeSet::new(),
            &["sentences.edit".to_owned()],
            &[],
        )
        .unwrap();
        assert_eq!(
            preview.after,
            vec!["sentences.access", "sentences.edit", "words.access"]
        );
        assert_eq!(
            preview.dependency_grants,
            vec!["sentences.access", "words.access"]
        );
        let permissions = preview.after.into_iter().collect();
        let authorization = AdminAuthorization {
            admin_id: actor,
            is_super_admin: false,
            permission_version: 1,
            permissions,
        };
        let Some(RoutePolicy::All(required)) = route_policy(
            "POST",
            "/api/v1/admin/lexicon/entries/component-targets/search",
        ) else {
            panic!("sentence association picker needs an explicit target-read policy");
        };
        for key in required {
            authorization.require(key).unwrap();
        }
        authorization
            .require_owned_action("sentences.edit", actor)
            .unwrap();
        assert!(
            authorization
                .require_owned_action("sentences.edit", uuid::Uuid::now_v7())
                .is_err()
        );
        assert!(!authorization.has("words.edit"));
        assert!(!authorization.has("words.publish"));

        let expanded = preview_change(
            actor,
            1,
            &authorization.permissions,
            &[
                "sentences.edit_others".to_owned(),
                "sentences.publish".to_owned(),
            ],
            &[],
        )
        .unwrap();
        let before = expanded.after.into_iter().collect();
        let revoked = preview_change(actor, 2, &before, &[], &["words.access".to_owned()]).unwrap();
        assert_eq!(revoked.dependency_revocations, vec!["sentences.edit"]);
        assert_eq!(
            revoked.after,
            vec![
                "sentences.access",
                "sentences.edit_others",
                "sentences.publish"
            ]
        );
        assert!(
            validate_closed(&BTreeSet::from([
                "sentences.access".to_owned(),
                "sentences.edit".to_owned(),
            ]))
            .is_err()
        );
    }

    #[test]
    fn scope_requires_access_and_does_not_imply_actions() {
        let mut keys = BTreeSet::from(["words.edit_others".to_owned()]);
        expand_grants(&mut keys);
        assert_eq!(
            keys,
            BTreeSet::from(["words.access".to_owned(), "words.edit_others".to_owned()])
        );
        assert!(effective_keys(["words.edit_others".to_owned(), "unknown".to_owned()]).is_empty());
    }
}
