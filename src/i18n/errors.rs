use super::InterfaceLocaleV1 as Locale;
use crate::error::{ErrorCode, ProblemDetails};

// Stable code keys; a title describes the type, while registered details retain
// the more specific recovery action. Unknown details remain verbatim.
pub fn title(code: ErrorCode, locale: Locale) -> &'static str {
    use ErrorCode::*;
    let (zh, hant) = match code {
        NotFound => ("资源不存在", "找不到資源"),
        InvalidJson => ("JSON 格式无效", "JSON 格式無效"),
        InvalidRequestBody => ("请求内容无效", "請求內容無效"),
        InvalidQuery => ("查询参数无效", "查詢參數無效"),
        InvalidPathParameter => ("路径参数无效", "路徑參數無效"),
        PayloadTooLarge => ("请求内容过大", "請求內容過大"),
        InvalidFormType => ("词形类型无效", "詞形類型無效"),
        FormTypeNotFound => ("词形类型不存在", "找不到詞形類型"),
        FormTypeConflict => ("词形类型已存在", "詞形類型已存在"),
        FormTypeInUse => ("词形类型正在使用", "詞形類型正在使用"),
        FormTypeRequired => ("必须保留基本词形", "必須保留基本詞形"),
        InvalidPartOfSpeech => ("词性无效", "詞性無效"),
        InvalidHeadword => ("词头无效", "詞頭無效"),
        CapabilityUnavailable => ("当前语言或组合尚未开放", "目前語言或組合尚未開放"),
        UnsupportedLanguage => ("不支持该语言", "不支援此語言"),
        UnsupportedSchemaVersion => ("不支持该数据版本", "不支援此資料版本"),
        InvalidPhone => ("手机号无效", "手機號碼無效"),
        InvalidEmail => ("邮箱地址无效", "電郵地址無效"),
        InvalidIdentifier => ("账号标识无效", "帳號識別資料無效"),
        InvalidPassword => ("密码无效", "密碼無效"),
        PasswordMissing => ("请输入密码", "請輸入密碼"),
        PasswordTooShort => ("密码过短", "密碼過短"),
        PasswordTooLong => ("密码过长", "密碼過長"),
        PasswordTooWeak => ("密码强度不足", "密碼強度不足"),
        PasswordCompromised => ("请更换已泄露的密码", "請更換已外洩的密碼"),
        PasswordUnchanged => ("新密码不能与当前密码相同", "新密碼不能與目前密碼相同"),
        InvalidDisplayName => ("显示名称无效", "顯示名稱無效"),
        InvalidOtpCode => ("验证码无效或已过期", "驗證碼無效或已過期"),
        InvalidInvitationCode => ("邀请码无效", "邀請碼無效"),
        LearningTaskConflict => ("学习任务已变化", "學習任務已變更"),
        LearningRunUnavailable => ("学习轮次不可继续", "學習回合無法繼續"),
        LearningSettingsRequired => ("请先完成学习设置", "請先完成學習設定"),
        LearningInsufficientQuestions => ("可用题目不足", "可用題目不足"),
        LearningAlreadyAnswered => ("已记录首次作答", "已記錄首次作答"),
        WordListConflict => ("词表状态已变化", "詞表狀態已變更"),
        WordListTipLimit => ("已达到词表投币上限", "已達到詞表投幣上限"),
        WordListAudioCostChanged => ("语音费用已变化", "語音費用已變更"),
        AccountDeletionUnsettledAudio => ("语音生成或退款尚未完成", "語音產生或退款尚未完成"),
        WordListEntryUnavailable => ("词表条目不可用", "詞表項目無法使用"),
        InvalidCredentials => ("账号或验证信息不正确", "帳號或驗證資料不正確"),
        InvalidToken => ("会话已失效，请重新登录", "工作階段已失效，請重新登入"),
        InvalidRefreshToken => ("会话无法恢复，请重新登录", "無法恢復工作階段，請重新登入"),
        UserNotFound => ("用户不存在", "找不到使用者"),
        AdminNotFound => ("管理员不存在", "找不到管理員"),
        AccountDisabled => ("账号已停用", "帳號已停用"),
        AccountLocked => ("账号暂时锁定", "帳號暫時鎖定"),
        MustChangePassword => ("请先修改密码", "請先修改密碼"),
        Forbidden => ("没有操作权限", "沒有操作權限"),
        PhoneBindingRequired => ("请先绑定手机号", "請先綁定手機號碼"),
        PhoneUnbindForbidden => ("手机号只能更换，不能解绑", "手機號碼只能更換，不能解除綁定"),
        OtpRateLimited => ("验证码请求过于频繁", "驗證碼請求過於頻繁"),
        OtpUnavailable => ("验证码服务暂不可用", "驗證碼服務暫時無法使用"),
        LoginRateLimited => ("登录尝试过于频繁", "登入嘗試過於頻繁"),
        LoginUnavailable => ("登录服务暂不可用", "登入服務暫時無法使用"),
        CoinInvalidAmount => ("金额无效", "金額無效"),
        CoinAccountUnavailable => ("天生币账号不可用", "天生幣帳號無法使用"),
        CoinWalletUnavailable => ("钱包不可用", "錢包無法使用"),
        CoinInsufficientBalance => ("余额不足", "餘額不足"),
        CoinIdempotencyConflict => ("交易请求编号冲突", "交易請求編號衝突"),
        CoinSourceConflict => ("交易来源已记录", "交易來源已記錄"),
        CoinReversalInvalid => ("该交易不能冲正", "此交易無法沖正"),
        AccountDeletionUpgradeRequired => ("请使用新版账号注销流程", "請使用新版帳號註銷流程"),
        AccountDeletionConsentRequired => ("请确认账号注销协议", "請確認帳號註銷協議"),
        AccountDeletionConsentOutdated => ("账号注销协议已更新", "帳號註銷協議已更新"),
        AccountDeletionBalanceChanged => ("余额已变化，请重新确认", "餘額已變更，請重新確認"),
        AccountDeletionPending => ("已有待处理的注销申请", "已有待處理的註銷申請"),
        AccountDeletionExpired => ("注销申请已到期", "註銷申請已到期"),
        AccountDeletionChannelUnavailable => ("所选验证通道不可用", "所選驗證管道無法使用"),
        InvalidAccountDeletionCode => ("注销验证码无效", "註銷驗證碼無效"),
        PasswordHashUnavailable => ("密码服务暂不可用", "密碼服務暫時無法使用"),
        UserAlreadyExists => ("账号已存在", "帳號已存在"),
        PhoneAlreadyRegistered => ("手机号已注册", "手機號碼已註冊"),
        PartOfSpeechNotFound => ("词性不存在", "找不到詞性"),
        SubPartOfSpeechNotFound => ("细分词性不存在", "找不到細分詞性"),
        PartOfSpeechConflict => ("词性已存在", "詞性已存在"),
        SubPartOfSpeechConflict => ("细分词性已存在", "細分詞性已存在"),
        TaxonomyConflict => ("同类别的编码或名称已存在", "同類別的編碼或名稱已存在"),
        AnnotationConflict => ("词条标注需要确认", "詞條標註需要確認"),
        RevisionConflict => ("内容版本已变化", "內容版本已變更"),
        CefrLevelLocked => ("初始 CEFR 等级已锁定", "初始 CEFR 等級已鎖定"),
        ReferenceConflict => ("引用存在冲突", "引用發生衝突"),
        RelationPrebindingFanoutExceeded => ("关联数量超出上限", "關聯數量超出上限"),
        StableNodeIdChanged => ("节点标识已变化", "節點識別碼已變更"),
        FormReferenceConflict => ("词形引用存在冲突", "詞形引用發生衝突"),
        InboundReferenceConflict => ("修改会破坏其他内容的引用", "修改會破壞其他內容的引用"),
        DetectionMismatch => ("检测结果与请求不一致", "偵測結果與請求不一致"),
        DetectionExpired => ("检测结果已过期", "偵測結果已過期"),
        DuplicateWord => ("词条已存在", "詞條已存在"),
        SurfaceMatchAcknowledgementRequired => ("请确认词面匹配", "請確認詞面比對"),
        SurfaceMatchesChanged => ("词面匹配结果已变化", "詞面比對結果已變更"),
        SurfaceMatchSnapshotExpired => ("词面确认已过期", "詞面確認已過期"),
        SurfacePolicyChanged => ("词面规则已变化", "詞面規則已變更"),
        ExactHeadwordCreationTemporarilyDisabled => {
            ("相同词头创建暂未开放", "相同詞頭建立暫未開放")
        }
        MultipleActiveExactHeadwordPublicationsNotEnabled => {
            ("相同词头多份发布暂未开放", "相同詞頭多份發布暫未開放")
        }
        IdempotencyConflict => ("请求编号已用于其他内容", "請求編號已用於其他內容"),
        StepNotReachable => ("请先完成前一步", "請先完成前一步"),
        ValidationFailed => ("内容校验失败", "內容驗證失敗"),
        SmartLexiconV3StorageUnavailable => ("词库存储能力暂不可用", "詞庫儲存能力暫時無法使用"),
        SmartLexiconV3DetectionUnavailable => ("词库检测能力暂不可用", "詞庫偵測能力暫時無法使用"),
        DownstreamConfirmationRequired => ("请确认下游影响", "請確認下游影響"),
        EntryArchived => ("词条已归档", "詞條已封存"),
        EntryNotDeletable => ("词条不符合删除条件", "詞條不符合刪除條件"),
        EntryDeleteForbidden => ("没有删除词条的权限", "沒有刪除詞條的權限"),
        EntryEditForbidden => ("没有编辑词条的权限", "沒有編輯詞條的權限"),
        EntryAnnotationForbidden => ("没有编辑标注的权限", "沒有編輯標註的權限"),
        EntryHasInboundPreboundRelations => {
            ("其他草稿关联仍引用此词条", "其他草稿關聯仍引用此詞條")
        }
        EntryHasInboundPublicationRefs => {
            ("其他已发布内容仍引用此词条", "其他已發布內容仍引用此詞條")
        }
        EntryHasUnavailablePublicationRefs => {
            ("发布内容包含不可用引用", "發布內容含有無法使用的引用")
        }
        WordNotFound => ("单词不存在", "找不到單字"),
        SentenceNotFound => ("例句不存在", "找不到例句"),
        SentenceAssociationsUnresolved => ("例句关联尚未确认", "例句關聯尚未確認"),
        SentenceAssociationClientUpgradeRequired => {
            ("请更新客户端后处理例句关联", "請更新用戶端後處理例句關聯")
        }
        FormContentsClientUpgradeRequired => {
            ("请更新客户端后编辑词形内容", "請更新用戶端後編輯詞形內容")
        }
        PendingSentenceAssociationNotFound => ("待修复例句关联不存在", "找不到待修復的例句關聯"),
        PendingSentenceAssociationClaimed => ("例句关联正在由他人修复", "例句關聯正由他人修復"),
        PublicationNotFound => ("发布版本不存在", "找不到發布版本"),
        PartOfSpeechInUse => ("词性正在使用", "詞性正在使用"),
        SubPartOfSpeechInUse => ("细分词性正在使用", "細分詞性正在使用"),
        SubPartOfSpeechNotAllowed => ("不允许使用该细分词性", "不允許使用此細分詞性"),
        PartOfSpeechHasSubParts => ("请先移除细分词性", "請先移除細分詞性"),
        PartOfSpeechHasFormTypes => ("请先移除词形类型", "請先移除詞形類型"),
        InvalidSpeechPreview => ("语音请求无效", "語音請求無效"),
        SpeechVoiceNotFound => ("音色不可用", "音色無法使用"),
        SpeechPreviewInProgress => ("语音正在生成", "語音正在產生"),
        SpeechProviderUnavailable => ("语音服务暂不可用", "語音服務暫時無法使用"),
        SpeechRateLimited => ("语音请求过于频繁", "語音請求過於頻繁"),
        LexiconAssistantRateLimited => ("助手请求过于频繁", "助手請求過於頻繁"),
        SpeechStorageUnavailable => ("语音存储暂不可用", "語音儲存暫時無法使用"),
        UnsupportedAudioContentType => ("不支持该音频格式", "不支援此音訊格式"),
        AudioFileTooLarge => ("音频文件过大", "音訊檔案過大"),
        AudioUploadNotCompleted => ("音频尚未上传完成", "音訊尚未上傳完成"),
        InvalidAudioKey => ("音频上传凭据无效", "音訊上傳憑證無效"),
        AudioStorageNotConfigured => ("音频存储尚未配置", "音訊儲存尚未設定"),
        UnsupportedAvatarContentType => ("不支持该头像格式", "不支援此頭像格式"),
        InvalidAvatarSize => ("头像尺寸无效", "頭像尺寸無效"),
        AvatarInvalidImage => ("头像图片无效", "頭像圖片無效"),
        AvatarFileTooLarge => ("头像文件过大", "頭像檔案過大"),
        InvalidAvatarKey => ("头像上传凭据无效", "頭像上傳憑證無效"),
        AvatarUploadNotCompleted => ("头像尚未上传完成", "頭像尚未上傳完成"),
        AvatarUploadRateLimited => ("头像上传过于频繁", "頭像上傳過於頻繁"),
        AvatarStorageNotConfigured => ("头像存储尚未配置", "頭像儲存尚未設定"),
        AvatarStorageUnavailable => ("头像存储暂不可用", "頭像儲存暫時無法使用"),
        AudioAssetNotFound => ("音频资源不存在", "找不到音訊資源"),
        ServiceUnavailable => ("服务暂不可用", "服務暫時無法使用"),
        InternalError => ("服务发生错误", "服務發生錯誤"),
    };
    match locale {
        Locale::ZhHans => zh,
        Locale::ZhHant => hant,
        Locale::EnGb | Locale::EnUs => match code {
            ErrorCode::LearningTaskConflict => "Learning task conflict",
            ErrorCode::LearningRunUnavailable => "Learning run unavailable",
            ErrorCode::LearningSettingsRequired => "Learning settings required",
            ErrorCode::LearningInsufficientQuestions => "Not enough questions",
            ErrorCode::LearningAlreadyAnswered => "First answer already recorded",
            _ => code.descriptor().title,
        },
    }
}

/// Internal display context; never serialized or included in OpenAPI.
#[derive(Debug)]
pub(crate) enum DetailTemplate {
    PositiveRevision { field: &'static str },
    InvalidVoiceField { field: &'static str },
    LearningInsufficientQuestions { available: usize, required: i32 },
    LearningPlanReasons(Vec<String>),
}

#[derive(Debug)]
pub(crate) enum IssueTemplate {
    InvalidContent(uuid::Uuid),
    DuplicateContentSlot(uuid::Uuid),
    InvalidVariantBinding(uuid::Uuid),
    IncompletePronunciationMapping(uuid::Uuid),
    DuplicatePronunciationBinding(uuid::Uuid),
    UnknownPronunciationSlot(uuid::Uuid),
    UnknownContent {
        content: uuid::Uuid,
        form: uuid::Uuid,
    },
    RegionalMappingDiffers(uuid::Uuid),
    DuplicateContentLabel(uuid::Uuid),
}

impl IssueTemplate {
    pub(crate) fn render(&self, locale: Locale) -> Option<String> {
        let hant = match locale {
            Locale::ZhHans => false,
            Locale::ZhHant => true,
            Locale::EnGb | Locale::EnUs => return None,
        };
        Some(match self {
            Self::InvalidContent(id) => {
                if hant {
                    format!("內容 {id} 無效")
                } else {
                    format!("内容 {id} 无效")
                }
            }
            Self::DuplicateContentSlot(id) => {
                if hant {
                    format!("內容槽位 {id} 重複")
                } else {
                    format!("内容槽位 {id} 重复")
                }
            }
            Self::InvalidVariantBinding(id) => {
                if hant {
                    format!("變體綁定 {id} 無效")
                } else {
                    format!("变体绑定 {id} 无效")
                }
            }
            Self::IncompletePronunciationMapping(id) => {
                if hant {
                    format!("發音對應 {id} 不完整")
                } else {
                    format!("发音映射 {id} 不完整")
                }
            }
            Self::DuplicatePronunciationBinding(id) => {
                if hant {
                    format!("發音綁定 {id} 重複")
                } else {
                    format!("发音绑定 {id} 重复")
                }
            }
            Self::UnknownPronunciationSlot(id) => {
                if hant {
                    format!("找不到發音槽位 {id}")
                } else {
                    format!("找不到发音槽位 {id}")
                }
            }
            Self::UnknownContent { content, form } => {
                if hant {
                    format!("詞形 {form} 的內容 {content} 不存在")
                } else {
                    format!("词形 {form} 的内容 {content} 不存在")
                }
            }
            Self::RegionalMappingDiffers(id) => {
                if hant {
                    format!("詞形 {id} 的地區對應不一致")
                } else {
                    format!("词形 {id} 的地区映射不一致")
                }
            }
            Self::DuplicateContentLabel(id) => {
                if hant {
                    format!("詞形組 {id} 中的內容標籤重複")
                } else {
                    format!("词形组 {id} 中的内容标签重复")
                }
            }
        })
    }
}

impl DetailTemplate {
    pub(crate) fn render(&self, locale: Locale) -> String {
        match self {
            Self::InvalidVoiceField { field } => match locale {
                Locale::ZhHans => format!("音色字段 {field} 无效"),
                Locale::ZhHant => format!("音色欄位 {field} 無效"),
                Locale::EnGb | Locale::EnUs => format!("voice field '{field}' is invalid"),
            },
            Self::PositiveRevision { field } => match locale {
                Locale::ZhHans => format!("{field} 必须至少为 1"),
                Locale::ZhHant => format!("{field} 必須至少為 1"),
                Locale::EnGb | Locale::EnUs => format!("{field} must be at least 1"),
            },
            Self::LearningInsufficientQuestions {
                available,
                required,
            } => match locale {
                Locale::ZhHans => {
                    format!("仅有 {available} 道可出题内容，需要 {required} 道；请查看预览排除原因")
                }
                Locale::ZhHant => {
                    format!("僅有 {available} 道可出題內容，需要 {required} 道；請查看預覽排除原因")
                }
                Locale::EnGb | Locale::EnUs => format!(
                    "Only {available} {} available; {required} {} required. Check the preview for exclusion reasons.",
                    if *available == 1 {
                        "question is"
                    } else {
                        "questions are"
                    },
                    if *required == 1 { "is" } else { "are" },
                ),
            },
            Self::LearningPlanReasons(reasons) => reasons
                .iter()
                .map(|reason| {
                    detail(ErrorCode::InvalidRequestBody, reason, locale).unwrap_or(reason)
                })
                .collect::<Vec<_>>()
                .join(match locale {
                    Locale::ZhHans | Locale::ZhHant => "；",
                    Locale::EnGb | Locale::EnUs => " ",
                }),
        }
    }
}

pub fn localize(problem: &mut ProblemDetails, locale: Locale) {
    problem.title = title(problem.code, locale);
    if let Some(detail) = detail(problem.code, &problem.detail, locale) {
        problem.detail = detail.to_owned();
    }
    if let Some(issues) = &mut problem.field_issues {
        for issue in issues {
            localize_issue(issue, locale);
        }
    }
}

/// Only first-party diagnostics registered by code and exact source are translated.
pub(crate) fn localize_issue(
    issue: &mut crate::lexicon::dto::V3DraftValidationIssue,
    locale: Locale,
) {
    let audio_limit = crate::lexicon::service::v3::MAX_VARIANT_AUDIO_ASSETS;
    if issue.code.as_str() == "audio_asset_invalid"
        && issue.message
            == format!(
                "a text or pronunciation variant may reference at most {audio_limit} audio assets"
            )
    {
        match locale {
            Locale::ZhHans => {
                issue.message = format!("文本或发音变体最多引用 {audio_limit} 条音频")
            }
            Locale::ZhHant => {
                issue.message = format!("文字或發音變體最多引用 {audio_limit} 條音訊")
            }
            Locale::EnGb | Locale::EnUs => {}
        }
        return;
    }
    let first_party_english = match (issue.code.as_str(), issue.message.as_str()) {
        (
            "phrase_component_not_allowed",
            "component usages are only available for phrase entries",
        ) => Some(("成分用法仅适用于短语词条", "成分用法僅適用於片語詞條")),
        (
            "phrase_component_limit_exceeded",
            "a sense may carry at most 100 phrase component usages",
        ) => Some((
            "一个词义最多包含 100 个短语成分用法",
            "一個詞義最多包含 100 個片語成分用法",
        )),
        (
            "phrase_component_literal_invalid",
            "component literal must be trimmed and 1-200 characters long",
        ) => Some((
            "成分文本须去除首尾空白并包含 1–200 个字符",
            "成分文字須去除首尾空白並包含 1–200 個字元",
        )),
        ("phrase_component_self_target", "phrase component must not target the phrase itself") => {
            Some(("短语成分不能指向短语自身", "片語成分不能指向片語本身"))
        }
        (
            "phrase_component_target_unavailable",
            "phrase component target is missing, archived or not a V3 word",
        ) => Some((
            "短语成分目标不存在、已归档或不是 V3 单词",
            "片語成分目標不存在、已封存或不是 V3 單字",
        )),
        (
            "phrase_component_target_nested",
            "phrase component target must not itself reference another phrase",
        ) => Some((
            "短语成分目标不能再引用其他短语",
            "片語成分目標不能再引用其他片語",
        )),
        (
            "phrase_component_target_stale",
            "resolved component must match the target word or phrase form and sense",
        ) => Some((
            "已解析成分必须匹配目标单词或短语的词形和词义",
            "已解析成分必須符合目標單字或片語的詞形和詞義",
        )),
        ("forbidden_v3_field", "field is not part of the V3 writable contract") => {
            Some(("字段不属于 V3 可写契约", "欄位不屬於 V3 可寫契約"))
        }
        (
            "audio_asset_invalid",
            "a text or pronunciation variant must not reference the same audio asset twice",
        ) => Some((
            "文本或发音变体不能重复引用同一音频",
            "文字或發音變體不能重複引用同一音訊",
        )),
        ("audio_asset_invalid", "audio asset does not exist") => {
            Some(("音频资源不存在", "找不到音訊資源"))
        }
        ("audio_asset_invalid", "audio asset is already used by another entry") => {
            Some(("音频资源已被其他词条使用", "音訊資源已被其他詞條使用"))
        }
        ("form_content_mapping_invalid", "too many pronunciation mappings") => {
            Some(("发音映射过多", "發音對應過多"))
        }
        ("form_content_mapping_invalid", "duplicate pronunciation mapping ID") => {
            Some(("发音映射 ID 重复", "發音對應 ID 重複"))
        }
        ("form_content_mapping_invalid", "invalid pronunciation slot correspondence") => {
            Some(("发音槽位对应关系无效", "發音槽位對應關係無效"))
        }
        ("form_content_mapping_invalid", "pronunciation slot identity differs") => {
            Some(("发音槽位标识不一致", "發音槽位識別碼不一致"))
        }
        ("form_content_mapping_invalid", "pronunciation correspondence requires two regions") => {
            Some(("发音对应关系需要两个地区", "發音對應關係需要兩個地區"))
        }
        (
            "form_content_mapping_invalid",
            "pronunciation mapping ID collides with a content slot",
        ) => Some(("发音映射 ID 与内容槽位冲突", "發音對應 ID 與內容槽位衝突")),
        ("form_content_mapping_invalid", "shared content exceeds node limit") => {
            Some(("共享内容超过节点上限", "共用內容超過節點上限"))
        }
        (
            "form_content_mapping_invalid",
            "each dialect requires a complete nonempty spelling list",
        ) => Some((
            "每个方言都需要完整且非空的拼写列表",
            "每個方言都需要完整且非空的拼寫清單",
        )),
        ("form_content_mapping_invalid", "duplicate spelling mapping") => {
            Some(("拼写映射重复", "拼寫對應重複"))
        }
        ("form_content_mapping_invalid", "missing spelling mapping") => {
            Some(("缺少拼写映射", "缺少拼寫對應"))
        }
        ("form_content_mapping_invalid", "content requires at least one binding") => {
            Some(("内容至少需要一个绑定", "內容至少需要一個綁定"))
        }
        (
            "relation_target_shape_invalid",
            "relation target must provide both entry and sense, or neither",
        ) => Some((
            "关联目标必须同时提供词条和词义，或均不提供",
            "關聯目標必須同時提供詞條和詞義，或均不提供",
        )),
        (
            "content_limit_exceeded",
            "pronunciation field exceeds the shared 200-codepoint limit",
        ) => Some((
            "发音字段超过 200 个 Unicode 码点的上限",
            "發音欄位超過 200 個 Unicode 碼點的上限",
        )),
        (
            "phonetic_rich_text_invalid",
            "phonetic rich text must match the plain field and contain valid annotations",
        ) => Some((
            "音标富文本必须与普通字段一致且标注有效",
            "音標富文字必須與一般欄位一致且標註有效",
        )),
        (
            "phonetic_rich_text_invalid",
            "synthesis settings must match spelling and contain only valid word-boundary pauses",
        ) => Some((
            "合成设置必须与拼写一致，且只包含有效的单词边界停顿",
            "合成設定必須與拼寫一致，且只包含有效的單字邊界停頓",
        )),
        (
            "content_limit_exceeded",
            "synthesis input exceeds its length limit or contains control characters",
        ) => Some((
            "合成输入超过长度上限或包含控制字符",
            "合成輸入超過長度上限或包含控制字元",
        )),
        (
            "pronunciation_required",
            "selected phoneme accent must be confirmed and match the regional variant",
        ) => Some((
            "所选音素口音必须已确认且与地区变体一致",
            "所選音素口音必須已確認且與地區變體一致",
        )),
        ("pronunciation_required", "configured synthesis requires the selected phoneme input") => {
            Some((
                "已配置的合成需要所选音素输入",
                "已設定的合成需要所選音素輸入",
            ))
        }
        ("pronunciation_required", "a complete pronunciation requires all fields") => {
            Some(("完整发音需要填写所有字段", "完整發音需要填寫所有欄位"))
        }
        ("duplicate_node_id", "the same stable UUID appears more than once") => {
            Some(("同一稳定 UUID 出现多次", "同一穩定 UUID 出現多次"))
        }
        ("duplicate_node_id", "the same stable UUID is used for different node roles") => Some((
            "同一稳定 UUID 被用于不同节点角色",
            "同一穩定 UUID 被用於不同節點角色",
        )),
        ("sense_form_group_invalid", "too many form group bindings") => {
            Some(("词形组绑定过多", "詞形組綁定過多"))
        }
        (
            "sense_form_group_invalid",
            "form groups must be distinct dedicated groups of the same part of speech",
        ) => Some((
            "词形组必须是同一词性下互不重复的专用组",
            "詞形組必須是同一詞性下互不重複的專用組",
        )),
        (
            "sense_form_group_required",
            "a sense without a dedicated form group requires a general form group in its part of speech",
        ) => Some((
            "没有专用词形组的词义需要其词性下的通用词形组",
            "沒有專用詞形組的詞義需要其詞性下的通用詞形組",
        )),
        ("content_limit_exceeded", "entry content exceeds the shared 2000-node limit") => Some((
            "词条内容超过 2000 个节点的共享上限",
            "詞條內容超過 2000 個節點的共用上限",
        )),
        (
            "meanings_storage_unsafe",
            "sense group rich text must match name_en and contain valid annotations",
        ) => Some((
            "语义区间富文本必须与 name_en 一致且标注有效",
            "語義區間富文字必須與 name_en 一致且標註有效",
        )),
        ("content_limit_exceeded", "sense group name exceeds the shared 200-codepoint limit") => {
            Some((
                "语义区间名称超过 200 个 Unicode 码点的上限",
                "語義區間名稱超過 200 個 Unicode 碼點的上限",
            ))
        }
        ("sentence_translation_required", "Chinese translation content is required") => {
            Some(("请填写中文译文", "請填寫中文譯文"))
        }
        ("sentence_translation_invalid", "Chinese translation RichText is invalid") => {
            Some(("中文译文富文本无效", "中文譯文富文字無效"))
        }
        ("definition_invalid", "invalid reading link ranges") => {
            Some(("读法关联范围无效", "讀法關聯範圍無效"))
        }
        ("definition_invalid", "invalid text link ranges") => {
            Some(("文本关联范围无效", "文字關聯範圍無效"))
        }
        (
            "voice_profile_invalid",
            "voice profile must list at most 2000 distinct non-empty voice ids, each with a rate within -50..=100",
        ) => Some((
            "音色配置最多包含 2000 个不重复且非空的音色 ID，语速均须在 -50 到 100 之间",
            "音色設定最多包含 2000 個不重複且非空的音色 ID，語速均須在 -50 到 100 之間",
        )),
        ("content_limit_exceeded", "rich text exceeds the shared 5000-codepoint limit") => Some((
            "富文本超过 5000 个 Unicode 码点的共享上限",
            "富文字超過 5000 個 Unicode 碼點的共用上限",
        )),
        ("content_limit_exceeded", "rich text decorations exceed the shared 2000-node limit") => {
            Some((
                "富文本标注超过 2000 个节点的共享上限",
                "富文字標註超過 2000 個節點的共用上限",
            ))
        }
        ("pos_required", "a complete entry requires at least one part of speech") => {
            Some(("完整词条至少需要一个词性", "完整詞條至少需要一個詞性"))
        }
        ("duplicate_pos_code", "the same POS code cannot appear twice in one entry") => Some((
            "同一词条不能重复使用同一词性代码",
            "同一詞條不能重複使用同一詞性代碼",
        )),
        ("form_group_required", "a complete POS requires at least one form group") => {
            Some(("完整词性至少需要一个词形组", "完整詞性至少需要一個詞形組"))
        }
        ("duplicate_node_id", "a concrete form ID cannot belong to two POS nodes") => Some((
            "同一具体词形 ID 不能属于两个词性节点",
            "同一具體詞形 ID 不能屬於兩個詞性節點",
        )),
        (
            "invalid_form_type_for_part_of_speech",
            "form_type is not allowed for this part of speech",
        ) => Some((
            "此词性不允许使用该 form_type",
            "此詞性不允許使用該 form_type",
        )),
        ("empty_form_group", "a complete entry cannot retain an empty form group") => {
            Some(("完整词条不能保留空词形组", "完整詞條不能保留空詞形組"))
        }
        ("base_form_required_in_group", "the first form group requires at least one base form") => {
            Some((
                "第一个词形组至少需要一个基本词形",
                "第一個詞形組至少需要一個基本詞形",
            ))
        }
        ("form_group_membership_invalid", "the same group cannot reference one form twice") => {
            Some((
                "同一词形组不能重复引用同一词形",
                "同一詞形組不能重複引用同一詞形",
            ))
        }
        ("form_group_membership_invalid", "a concrete form can belong to only one form group") => {
            Some(("具体词形只能属于一个词形组", "具體詞形只能屬於一個詞形組"))
        }
        (
            "form_group_membership_invalid",
            "a membership cannot reference a form owned by another POS",
        ) => Some((
            "成员不能引用属于其他词性的词形",
            "成員不能引用屬於其他詞性的詞形",
        )),
        (
            "form_group_membership_invalid",
            "membership form_id does not exist in the submitted entry",
        ) => Some((
            "成员的 form_id 在提交的词条中不存在",
            "成員的 form_id 在提交的詞條中不存在",
        )),
        ("orphan_form", "every saved concrete form requires at least one membership") => Some((
            "每个保存的具体词形至少需要一个成员关系",
            "每個儲存的具體詞形至少需要一個成員關係",
        )),
        (
            "dialect_rules_invalid",
            "distinguish/unified is not a valid dialect rule combination",
        ) => Some((
            "distinguish/unified 不是有效的方言规则组合",
            "distinguish/unified 不是有效的方言規則組合",
        )),
        (
            "invalid_regional_variant_shape",
            "regional_variants do not match the dialect_rules of the form's group",
        ) => Some((
            "regional_variants 与词形组的 dialect_rules 不一致",
            "regional_variants 與詞形組的 dialect_rules 不一致",
        )),
        (
            "dedicated_form_group_unused",
            "a dedicated form group must be bound by at least one sense",
        ) => Some((
            "专用词形组至少需要绑定一个词义",
            "專用詞形組至少需要綁定一個詞義",
        )),
        ("variant_spelling_required", "a complete regional variant requires spelling") => {
            Some(("完整地区变体需要拼写", "完整地區變體需要拼寫"))
        }
        ("content_limit_exceeded", "variant spelling exceeds the shared 200-codepoint limit") => {
            Some((
                "变体拼写超过 200 个 Unicode 码点的上限",
                "變體拼寫超過 200 個 Unicode 碼點的上限",
            ))
        }
        (
            "spelling_rich_text_invalid",
            "spelling speech must match spelling and contain only valid pauses",
        ) => Some((
            "拼写语音必须与拼写一致，且只能包含有效停顿",
            "拼寫語音必須與拼寫一致，且只能包含有效停頓",
        )),
        (
            "spelling_rich_text_invalid",
            "spelling rich text must match the normalized spelling and contain only valid italic or liaison annotations",
        ) => Some((
            "拼写富文本必须与规范化拼写一致，且只能包含有效斜体或连读标注",
            "拼寫富文字必須與正規化拼寫一致，且只能包含有效斜體或連讀標註",
        )),
        (
            "pronunciation_required",
            "a complete regional variant requires at least one pronunciation",
        ) => Some((
            "完整地区变体至少需要一个发音",
            "完整地區變體至少需要一個發音",
        )),
        (
            "content_limit_exceeded",
            "UPS word metadata must contain 1-30 bounded, nonempty text/phoneme pairs",
        ) => Some((
            "UPS 单词元数据须包含 1–30 组长度合规且非空的文本与音素",
            "UPS 單字中繼資料須包含 1–30 組長度合規且非空的文字與音素",
        )),
        (
            "pronunciation_required",
            "UPS phrase word boundaries are missing or stale; convert again from actual pronunciation",
        ) => Some((
            "UPS 短语单词边界缺失或已过期，请根据实际发音重新转换",
            "UPS 片語單字邊界缺失或已過期，請根據實際發音重新轉換",
        )),
        (
            "duplicate_pronunciation",
            "complete normalized pronunciation triples must be unique per variant",
        ) => Some((
            "同一变体的完整规范化发音三元组不能重复",
            "同一變體的完整正規化發音三元組不能重複",
        )),
        (
            "dialect_rules_invalid",
            "dialect_rules must be one of unified/unified, unified/distinguish, or distinguish/distinguish",
        ) => Some((
            "dialect_rules 必须是 unified/unified、unified/distinguish 或 distinguish/distinguish",
            "dialect_rules 必須是 unified/unified、unified/distinguish 或 distinguish/distinguish",
        )),
        ("invalid_form_type_for_part_of_speech", "form_type must be a valid catalog code") => {
            Some((
                "form_type 必须是有效的目录代码",
                "form_type 必須是有效的目錄代碼",
            ))
        }
        (
            "invalid_regional_variant_shape",
            "regional_variants must be common xor complete uk_us",
        ) => Some((
            "regional_variants 必须是 common 或完整的 uk_us，不能同时使用",
            "regional_variants 必須是 common 或完整的 uk_us，不能同時使用",
        )),
        _ => None,
    };
    if let Some((hans, hant)) = first_party_english {
        match locale {
            Locale::ZhHans => issue.message = hans.to_owned(),
            Locale::ZhHant => issue.message = hant.to_owned(),
            Locale::EnGb | Locale::EnUs => {}
        }
        return;
    }
    let translations = match (issue.code.as_str(), issue.message.as_str()) {
        ("invalid_form_type_for_part_of_speech", "词形类型不存在或已删除，请刷新配置") => {
            Some((
                "詞形類型不存在或已刪除，請重新整理設定",
                "The form type does not exist or has been deleted. Reload the configuration.",
            ))
        }
        (
            "meanings_storage_unsafe",
            "此内容包含新版加粗或下划线格式，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版粗體或底線格式，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer bold or underline formatting. Preserve your local input and reload before saving.",
        )),
        (
            "voice_profile_invalid",
            "此内容包含新版文本配音版本，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版文字配音版本，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer text dubbing versions. Preserve your local input and reload before saving.",
        )),
        ("node_id_reused", "节点 ID 在请求中重复") => Some((
            "節點 ID 在請求中重複",
            "The node ID is duplicated in the request.",
        )),
        ("node_id_reused", "节点 ID 不能用于不同节点类型") => Some((
            "節點 ID 不能用於不同節點類型",
            "A node ID cannot be used for different node types.",
        )),
        ("node_id_reused", "节点 ID 不能跨步骤复用") => Some((
            "節點 ID 不能跨步驟重複使用",
            "A node ID cannot be reused across steps.",
        )),
        ("grammar_form_link_invalid", "关联词形的第一个发音未配置，请先完善该发音") => {
            Some((
                "關聯詞形的第一個發音未設定，請先完善該發音",
                "The first pronunciation of the linked form is not configured. Complete it first.",
            ))
        }
        ("grammar_form_link_invalid", "关联词形的第一个发音与当前口音不匹配，请先完善该发音") => {
            Some((
                "關聯詞形的第一個發音與目前口音不符，請先完善該發音",
                "The first pronunciation of the linked form does not match the current accent. Complete it first.",
            ))
        }
        ("sentence_context_target_unavailable", "例句 context 必须指向当前草稿中的有效词义") => {
            Some((
                "例句 context 必須指向目前草稿中的有效詞義",
                "The sentence context must point to a valid sense in the current draft.",
            ))
        }
        (
            "sentence_context_target_unavailable",
            "例句 context 必须是目标词条当前发布版本中的有效词义",
        ) => Some((
            "例句 context 必須是目標詞條目前發布版本中的有效詞義",
            "The sentence context must be a valid sense in the target entry's current publication.",
        )),
        ("relation_self_target", "关联词不能指向当前词条自身") => Some((
            "關聯詞不能指向目前詞條本身",
            "A related word cannot point to the current entry.",
        )),
        ("relation_target_unavailable", "关联词目标词义尚未发布，请先发布具体目标词义") => {
            Some((
                "關聯詞目標詞義尚未發布，請先發布具體目標詞義",
                "The target sense is not published. Publish the specific target sense first.",
            ))
        }
        (
            "relation_target_unavailable",
            "关联词目标必须是未归档词条当前草稿或当前发布中的有效词义",
        ) => Some((
            "關聯詞目標必須是未封存詞條目前草稿或目前發布中的有效詞義",
            "The related word target must be a valid sense in the current draft or publication of an unarchived entry.",
        )),
        ("relation_target_stale", "关联词目标的当前发布内容已变化，请重新保存词义步骤") => {
            Some((
                "關聯詞目標的目前發布內容已變更，請重新儲存詞義步驟",
                "The target's current publication has changed. Save the meanings step again.",
            ))
        }
        ("relation_pending_gloss_without_headword", "文本注释必须跟随关联词文本") => {
            Some((
                "文字註解必須跟隨關聯詞文字",
                "The text note requires related word text.",
            ))
        }
        ("relation_pending_gloss_invalid", "预定义词义不能超过 5000 个字符") => Some((
            "預定義詞義不能超過 5000 個字元",
            "A predefined sense cannot exceed 5000 characters.",
        )),
        ("relation_pending_headword_invalid", "关联词文本不能含控制字符，且不超过 200 个字符") => {
            Some((
                "關聯詞文字不能含控制字元，且不超過 200 個字元",
                "Related word text cannot contain control characters or exceed 200 characters.",
            ))
        }
        (
            "relation_target_unavailable"
            | "sentence_context_target_unavailable"
            | "phrase_component_target_unavailable"
            | "definition_invalid",
            "具体引用目标尚未发布或已不在当前发布内容中，请先发布目标或修复引用",
        ) => Some((
            "具體引用目標尚未發布或已不在目前發布內容中，請先發布目標或修復引用",
            "The referenced target is unpublished or no longer in the current publication. Publish the target or fix the reference first.",
        )),
        ("definition_invalid", "请选择语法结构") => {
            Some(("請選擇語法結構", "Select a grammar structure."))
        }
        ("meanings_storage_unsafe", "词义内容无法安全存储") => Some((
            "詞義內容無法安全儲存",
            "The meaning content cannot be stored safely.",
        )),
        ("sense_group_required", "至少需要一个语义区间") => Some((
            "至少需要一個語義區間",
            "At least one sense group is required.",
        )),
        ("sense_group_name_required", "语义区间名称不能为空") => {
            Some(("語義區間名稱不能為空", "Enter a sense group name."))
        }
        ("sense_group_name_too_long", "语义区间名称不能超过 200 个字符") => Some((
            "語義區間名稱不能超過 200 個字元",
            "The sense group name must not exceed 200 characters.",
        )),
        ("pos_not_found", "词义引用了不存在的基本词性") => Some((
            "詞義引用了不存在的基本詞性",
            "The sense refers to a part of speech that does not exist.",
        )),
        ("duplicate_pos_meanings", "同一基本词性只能有一组词义数据") => Some((
            "同一基本詞性只能有一組詞義資料",
            "Each part of speech can have only one set of meanings.",
        )),
        ("grammar_required", "每个词性至少需要一条语法结构") => Some((
            "每個詞性至少需要一條語法結構",
            "Each part of speech requires at least one grammar structure.",
        )),
        ("grammar_variants_invalid", "语法结构必须包含当前方言的完整文本") => {
            Some((
                "語法結構必須包含目前方言的完整文字",
                "The grammar structure must contain complete text for the current dialect.",
            ))
        }
        ("sense_required", "每个词性至少需要一个词义") => Some((
            "每個詞性至少需要一個詞義",
            "Each part of speech requires at least one sense.",
        )),
        ("level_invalid", "CEFR 等级无效") => Some(("CEFR 等級無效", "Invalid CEFR level.")),
        ("sub_pos_required", "请选择细分词性") => {
            Some(("請選擇細分詞性", "Select a sub-part of speech."))
        }
        ("invalid_sub_part_of_speech", "细分词性不属于当前基本词性") => Some((
            "細分詞性不屬於目前基本詞性",
            "The sub-part of speech does not belong to the selected part of speech.",
        )),
        ("frequency_invalid", "词义词频必须是 0–100 且最多两位小数") => Some((
            "詞義詞頻必須是 0–100 且最多兩位小數",
            "Sense frequency must be between 0 and 100 with at most two decimal places.",
        )),
        ("sense_group_not_found", "请选择有效的语义区间") => {
            Some(("請選擇有效的語義區間", "Select a valid sense group."))
        }
        ("definition_required", "每个词义至少需要一条释义") => Some((
            "每個詞義至少需要一條釋義",
            "Each sense requires at least one definition.",
        )),
        ("definition_level_invalid", "释义 CEFR 等级无效") => {
            Some(("釋義 CEFR 等級無效", "Invalid definition CEFR level."))
        }
        ("definition_invalid", "释义文本或语法结构引用无效") => Some((
            "釋義文字或語法結構引用無效",
            "The definition text or grammar structure reference is invalid.",
        )),
        ("native_definition_required", "至少填写一条中文释义") => Some((
            "至少填寫一條中文釋義",
            "Enter at least one Chinese definition.",
        )),
        ("sentence_level_invalid", "例句 CEFR 等级无效") => {
            Some(("例句 CEFR 等級無效", "Invalid example sentence CEFR level."))
        }
        ("sentence_link_role_invalid", "例句链接角色必须是 focus 或 context") => Some((
            "例句連結角色必須是 focus 或 context",
            "The sentence link role must be focus or context.",
        )),
        ("duplicate_sentence_link", "同一例句不能重复链接同一词义") => Some((
            "同一例句不能重複連結同一詞義",
            "The same sentence cannot be linked to the same sense twice.",
        )),
        ("sentence_incomplete", "例句需包含完整中英文和唯一当前词义焦点") => {
            Some((
                "例句需包含完整中英文和唯一目前詞義焦點",
                "The sentence requires complete Chinese and English text and one focus on the current sense.",
            ))
        }
        ("relation_score_invalid", "关联度必须是 0–100 且最多两位小数") => Some((
            "關聯度必須是 0–100 且最多兩位小數",
            "The relation score must be between 0 and 100 with at most two decimal places.",
        )),
        ("relation_type_invalid", "关联词类型无效") => {
            Some(("關聯詞類型無效", "Invalid related word type."))
        }
        (
            "relation_target_shape_invalid",
            "关联词要么同时给出目标词条与目标词义，要么只留待关联词面",
        ) => Some((
            "關聯詞須同時提供目標詞條與目標詞義，或只保留待關聯詞面",
            "Provide both the target entry and sense, or only the text to link later.",
        )),
        ("relation_target_shape_invalid", "已绑定关联词不能再携带待建词面或预定义词义") => {
            Some((
                "已綁定關聯詞不能再包含待建詞面或預定義詞義",
                "A bound related word cannot also include pending text or a predefined sense.",
            ))
        }
        ("pos_meanings_required", "每个基本词性都需要词义数据") => Some((
            "每個基本詞性都需要詞義資料",
            "Each part of speech requires meaning data.",
        )),
        ("node_id_reused", "节点 ID 已属于其他词条或节点类型") => Some((
            "節點 ID 已屬於其他詞條或節點類型",
            "The node ID already belongs to another entry or node type.",
        )),
        ("node_binding_unknown", "历史节点缺少可验证的父子绑定，不能重新用于草稿内容") => {
            Some((
                "歷史節點缺少可驗證的父子綁定，不能重新用於草稿內容",
                "This historical node has no verifiable parent binding and cannot be reused in the draft.",
            ))
        }
        ("node_binding_changed", "节点 ID 不能更换父节点或内容槽位") => Some((
            "節點 ID 不能更換父節點或內容槽位",
            "A node ID cannot change its parent or content slot.",
        )),
        ("stable_node_id_changed", "已有内容槽位必须保留原节点 ID") => Some((
            "既有內容槽位必須保留原節點 ID",
            "An existing content slot must retain its original node ID.",
        )),
        ("definition_invalid", "正文已改变，请使用支持关联的编辑器重新确认关联") => {
            Some((
                "正文已變更，請使用支援關聯的編輯器重新確認關聯",
                "The text has changed. Reconfirm the links in an editor that supports linking.",
            ))
        }
        ("definition_invalid", "关联词段无效、重叠或超过上限，请重新选择") => {
            Some((
                "關聯詞段無效、重疊或超過上限，請重新選擇",
                "The linked text spans are invalid, overlap or exceed the limit. Select them again.",
            ))
        }
        ("definition_invalid", "关联标识重复，请重新选择") => Some((
            "關聯識別碼重複，請重新選擇",
            "Link identifiers are duplicated. Select them again.",
        )),
        ("definition_invalid", "当前词条请直接关联，不通过短语成分中转") => {
            Some((
                "目前詞條請直接關聯，不透過片語成分中轉",
                "Link the current entry directly, without an intermediate phrase component.",
            ))
        }
        ("definition_invalid", "关联的词形或词义已不在当前词条里，请重新选择") => {
            Some((
                "關聯的詞形或詞義已不在目前詞條中，請重新選擇",
                "The linked form or sense is no longer in the current entry. Select it again.",
            ))
        }
        ("definition_invalid", "关联目标已不可用，请重新选择") => Some((
            "關聯目標已無法使用，請重新選擇",
            "The linked target is no longer available. Select it again.",
        )),
        ("definition_invalid", "同一正文的同一目标词义须使用同一发布版本") => {
            Some((
                "同一正文的同一目標詞義須使用同一發布版本",
                "The same target sense in one text must use the same published version.",
            ))
        }
        ("definition_invalid", "关联的词形或词义已不在目标词条里，请重新选择") => {
            Some((
                "關聯的詞形或詞義已不在目標詞條中，請重新選擇",
                "The linked form or sense is no longer in the target entry. Select it again.",
            ))
        }
        ("definition_invalid", "短语成分与关联目标不一致，请重新选择") => Some((
            "片語成分與關聯目標不一致，請重新選擇",
            "The phrase component does not match the linked target. Select it again.",
        )),
        ("grammar_form_link_invalid", "正文已改变，请使用支持关联词形的编辑器重新确认关联") => {
            Some((
                "正文已變更，請使用支援關聯詞形的編輯器重新確認關聯",
                "The text has changed. Reconfirm the links in an editor that supports form links.",
            ))
        }
        ("grammar_form_link_invalid", "正文已改变，请重新确认读法关联") => Some((
            "正文已變更，請重新確認讀法關聯",
            "The text has changed. Reconfirm the pronunciation link.",
        )),
        ("grammar_form_link_invalid", "请以完整单词关联词形，不得重复或重叠关联") => {
            Some((
                "請以完整單字關聯詞形，不得重複或重疊關聯",
                "Link forms to complete words without duplicate or overlapping links.",
            ))
        }
        ("grammar_form_link_invalid", "请重新打开语法结构编辑器确认词形关联") => {
            Some((
                "請重新開啟語法結構編輯器確認詞形關聯",
                "Reopen the grammar structure editor to confirm the form links.",
            ))
        }
        ("grammar_form_link_invalid", "关联词形只支持单词") => {
            Some(("關聯詞形只支援單字", "Form links support words only."))
        }
        ("grammar_form_link_invalid", "读法义项不属于当前词性或词形") => Some((
            "讀法義項不屬於目前詞性或詞形",
            "The pronunciation sense does not belong to the current part of speech or form.",
        )),
        ("grammar_form_link_invalid", "关联词形目标已失效，请重新关联") => Some((
            "關聯詞形目標已失效，請重新關聯",
            "The linked form target is no longer valid. Link it again.",
        )),
        ("grammar_form_link_invalid", "请先发布目标单词，再关联其已发布词形") => {
            Some((
                "請先發布目標單字，再關聯其已發布詞形",
                "Publish the target word before linking its published form.",
            ))
        }
        ("grammar_form_link_invalid", "关联词形已失效，请重新关联") => Some((
            "關聯詞形已失效，請重新關聯",
            "The linked form is no longer valid. Link it again.",
        )),
        (
            "grammar_form_link_invalid",
            "关联词形的第一个发音包含不可用于合成的音素，请检查发音配置",
        ) => Some((
            "關聯詞形的第一個發音包含無法用於合成的音素，請檢查發音設定",
            "The first pronunciation of the linked form contains phonemes that cannot be synthesised. Check the pronunciation settings.",
        )),
        ("grammar_form_link_invalid", "读法关联必须绑定完整单词且不能重叠") => {
            Some((
                "讀法關聯必須綁定完整單字且不能重疊",
                "Pronunciation links must bind complete words and cannot overlap.",
            ))
        }
        ("grammar_form_link_invalid", "读法目标已失效") => Some((
            "讀法目標已失效",
            "The pronunciation target is no longer valid.",
        )),
        ("grammar_form_link_invalid", "请先发布读法目标单词") => Some((
            "請先發布讀法目標單字",
            "Publish the pronunciation target word first.",
        )),
        ("grammar_form_link_invalid", "读法词形已失效") => Some((
            "讀法詞形已失效",
            "The pronunciation form is no longer valid.",
        )),
        ("grammar_form_link_invalid", "请选择具体读法") => {
            Some(("請選擇具體讀法", "Select a specific pronunciation."))
        }
        _ => None,
    };
    if let Some((hant, english)) = translations {
        match locale {
            Locale::ZhHans => {}
            Locale::ZhHant => issue.message = hant.to_owned(),
            Locale::EnGb | Locale::EnUs => issue.message = english.to_owned(),
        }
    }
}

fn english_detail(code: ErrorCode, source: &str, locale: Locale) -> Option<&'static str> {
    let (hans, hant) = match (code, source) {
        (ErrorCode::NotFound, "avatar not found") => Some(("头像不存在", "找不到頭像")),
        (ErrorCode::NotFound, "target admin not found") => {
            Some(("目标管理员不存在", "找不到目標管理員"))
        }
        (ErrorCode::NotFound, "tag not found") => Some(("标签不存在", "找不到標籤")),
        (ErrorCode::NotFound, "user not found") => Some(("用户不存在", "找不到使用者")),
        (ErrorCode::NotFound, "admin not found") => Some(("管理员不存在", "找不到管理員")),
        (ErrorCode::InvalidDisplayName, "display name cannot be empty") => {
            Some(("显示名称不能为空", "顯示名稱不能為空"))
        }
        (ErrorCode::InvalidDisplayName, "display name cannot be longer than 50 characters") => {
            Some(("显示名称不能超过 50 个字符", "顯示名稱不能超過 50 個字元"))
        }
        (ErrorCode::InvalidDisplayName, "display name contains forbidden characters") => {
            Some(("显示名称包含不允许的字符", "顯示名稱包含不允許的字元"))
        }
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "speech style is not supported by this voice",
        ) => Some(("该音色不支持此语音风格", "此音色不支援此語音風格")),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "speech rate must be between -50 and 100 percent",
        ) => Some(("语速须在 -50% 到 100% 之间", "語速須在 -50% 到 100% 之間")),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "speech pitch must be between -12 and 12 semitones",
        ) => Some(("音调须在 -12 到 12 半音之间", "音調須在 -12 到 12 半音之間")),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "speech volume must be between 0 and 100 percent",
        ) => Some(("音量须在 0% 到 100% 之间", "音量須在 0% 到 100% 之間")),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "only canonical RichTextV2 can be synthesized",
        ) => Some((
            "仅支持合成规范化的 RichTextV2",
            "僅支援合成正規化的 RichTextV2",
        )),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "there is nothing left to read in this text",
        ) => Some(("文本中没有可朗读的内容", "文字中沒有可朗讀的內容")),
        (ErrorCode::InvalidRequestBody, "invalid pagination") => {
            Some(("分页参数无效", "分頁參數無效"))
        }
        (ErrorCode::InvalidQuery, "base_revision must be at least 1") => {
            Some(("base_revision 必须至少为 1", "base_revision 必須至少為 1"))
        }
        (ErrorCode::ValidationFailed, "q must not have leading or trailing whitespace") => {
            Some(("q 不能以空白字符开头或结尾", "q 不能以空白字元開頭或結尾"))
        }
        (ErrorCode::ValidationFailed, "q must contain between 1 and 100 codepoints") => Some((
            "q 须包含 1–100 个 Unicode 码点",
            "q 須包含 1–100 個 Unicode 碼點",
        )),
        (ErrorCode::ValidationFailed, "q must not contain NUL characters") => {
            Some(("q 不能包含 NUL 字符", "q 不能包含 NUL 字元"))
        }
        (ErrorCode::Forbidden, "unregistered admin route") => {
            Some(("后台路由未注册", "後台路由未註冊"))
        }
        (ErrorCode::Forbidden, "learning settings require a student profile") => {
            Some(("学习设置需要学生身份", "學習設定需要學生身分"))
        }
        (ErrorCode::SurfaceMatchSnapshotExpired, "surface confirmation snapshot expired") => {
            Some(("词面确认快照已过期", "詞面確認快照已過期"))
        }
        (ErrorCode::EntryDeleteForbidden, "only a super admin may permanently delete entries") => {
            Some(("仅超管可以永久删除词条", "僅超管可以永久刪除詞條"))
        }
        (
            ErrorCode::EntryEditForbidden,
            "entry editing requires ownership or words.edit_others",
        ) => Some((
            "编辑词条需要创建者身份或 words.edit_others 权限",
            "編輯詞條需要建立者身分或 words.edit_others 權限",
        )),
        (
            ErrorCode::EntryAnnotationForbidden,
            "annotation editing requires ownership or words.edit_others",
        ) => Some((
            "编辑标注需要创建者身份或 words.edit_others 权限",
            "編輯標註需要建立者身分或 words.edit_others 權限",
        )),
        (ErrorCode::ServiceUnavailable, "detection service unavailable") => {
            Some(("检测服务暂不可用", "偵測服務暫時無法使用"))
        }
        (ErrorCode::ServiceUnavailable, "impact confirmation service unavailable") => {
            Some(("影响确认服务暂不可用", "影響確認服務暫時無法使用"))
        }
        (ErrorCode::ServiceUnavailable, "surface snapshot service unavailable") => {
            Some(("词面快照服务暂不可用", "詞面快照服務暫時無法使用"))
        }
        (ErrorCode::ServiceUnavailable, "surface policy service unavailable") => {
            Some(("词面规则服务暂不可用", "詞面規則服務暫時無法使用"))
        }
        (ErrorCode::Forbidden, "audio asset permission required") => {
            Some(("需要音频资源权限", "需要音訊資源權限"))
        }
        (ErrorCode::Forbidden, "audio upload is not owned by this admin") => {
            Some(("音频上传不属于当前管理员", "音訊上傳不屬於目前管理員"))
        }
        (ErrorCode::Forbidden, "lexicon action permission required") => {
            Some(("需要词库操作权限", "需要詞庫操作權限"))
        }
        (ErrorCode::Forbidden, "content editing permission required") => {
            Some(("需要内容编辑权限", "需要內容編輯權限"))
        }
        (ErrorCode::Forbidden, "content ownership required") => {
            Some(("需要内容所有者身份", "需要內容擁有者身分"))
        }
        (ErrorCode::InvalidHeadword, "headword is required") => Some(("请输入词头", "請輸入詞頭")),
        (ErrorCode::InvalidHeadword, "headword is too long") => Some(("词头过长", "詞頭過長")),
        (ErrorCode::InvalidHeadword, "headword contains control characters") => {
            Some(("词头不能包含控制字符", "詞頭不能包含控制字元"))
        }
        (
            ErrorCode::InvalidHeadword,
            "headword must contain only Latin letters, digits and - ' . & / , characters",
        ) => Some((
            "词头只能包含拉丁字母、数字和 - ' . & / , 字符",
            "詞頭只能包含拉丁字母、數字和 - ' . & / , 字元",
        )),
        (ErrorCode::InvalidHeadword, "headword must contain at least one Latin letter") => {
            Some(("词头至少需要一个拉丁字母", "詞頭至少需要一個拉丁字母"))
        }
        (ErrorCode::PartOfSpeechInUse, "part of speech is in use") => {
            Some(("词性正在使用", "詞性正在使用"))
        }
        (ErrorCode::SubPartOfSpeechInUse, "sub part of speech is in use") => {
            Some(("细分词性正在使用", "細分詞性正在使用"))
        }
        (ErrorCode::FormTypeConflict, "form type already exists") => {
            Some(("词形类型已存在", "詞形類型已存在"))
        }
        (ErrorCode::InvalidFormType, "base form type must not belong to a part of speech") => {
            Some((
                "基本词形类型不能属于某个词性",
                "基本詞形類型不能屬於某個詞性",
            ))
        }
        (ErrorCode::FormTypeInUse, "form type is in use") => {
            Some(("词形类型正在使用", "詞形類型正在使用"))
        }
        (ErrorCode::InvalidFormType, "invalid text length") => {
            Some(("文本长度无效", "文字長度無效"))
        }
        (ErrorCode::InvalidFormType, "form types belong to word parts of speech only") => {
            Some(("词形类型仅适用于单词词性", "詞形類型僅適用於單字詞性"))
        }
        (ErrorCode::InvalidFormType, "invalid form type code") => {
            Some(("词形类型代码无效", "詞形類型代碼無效"))
        }
        (ErrorCode::InvalidQuery, "revision must be positive") => {
            Some(("revision 必须为正数", "revision 必須為正數"))
        }
        (ErrorCode::InvalidFormType, "base form type must remain in the default template") => {
            Some((
                "默认模板必须保留基本词形类型",
                "預設範本必須保留基本詞形類型",
            ))
        }
        (ErrorCode::InvalidFormType, "base form type is global") => {
            Some(("基本词形类型是全局类型", "基本詞形類型是全域類型"))
        }
        (ErrorCode::InvalidFormType, "form type must belong to a part of speech") => {
            Some(("词形类型必须属于一个词性", "詞形類型必須屬於一個詞性"))
        }
        (ErrorCode::FormTypeRequired, "base form type cannot be deleted") => {
            Some(("不能删除基本词形类型", "不能刪除基本詞形類型"))
        }
        (
            ErrorCode::SmartLexiconV3StorageUnavailable,
            "Smart Lexicon V3 storage or projection capability is disabled",
        ) => Some((
            "Smart Lexicon V3 存储或投影能力未启用",
            "Smart Lexicon V3 儲存或投影功能未啟用",
        )),
        (
            ErrorCode::SmartLexiconV3DetectionUnavailable,
            "Smart Lexicon V3 form-surface detection requires the C2 projection capability",
        ) => Some((
            "Smart Lexicon V3 词形检测需要 C2 投影能力",
            "Smart Lexicon V3 詞形偵測需要 C2 投影功能",
        )),
        (ErrorCode::UnsupportedSchemaVersion, "unsupported lexicon schema version") => {
            Some(("不支持该词库数据版本", "不支援此詞庫資料版本"))
        }
        (ErrorCode::DetectionMismatch, "detection does not match create request") => {
            Some(("检测结果与创建请求不一致", "偵測結果與建立請求不一致"))
        }
        (
            ErrorCode::SurfaceMatchAcknowledgementRequired,
            "surface match acknowledgement is required",
        ) => Some(("请确认词面匹配", "請確認詞面比對")),
        (ErrorCode::SurfaceMatchesChanged, "surface matches changed since confirmation") => {
            Some(("确认后词面匹配结果已变化", "確認後詞面比對結果已變更"))
        }
        (
            ErrorCode::SurfaceMatchesChanged,
            "surface matches changed since confirmation; retry without the stale token",
        ) => Some((
            "确认后词面匹配结果已变化，请不使用过期令牌重试",
            "確認後詞面比對結果已變更，請不使用過期權杖重試",
        )),
        (ErrorCode::SurfacePolicyChanged, "surface policy changed since confirmation") => {
            Some(("确认后词面规则已变化", "確認後詞面規則已變更"))
        }
        (
            ErrorCode::MultipleActiveExactHeadwordPublicationsNotEnabled,
            "multiple active exact headword publications are not enabled",
        ) => Some((
            "尚未启用相同词头的多份有效发布",
            "尚未啟用相同詞頭的多份有效發布",
        )),
        (ErrorCode::IdempotencyConflict, "idempotency key was reused with a different request") => {
            Some(("幂等键已用于不同请求", "冪等鍵已用於不同請求"))
        }
        (ErrorCode::EntryArchived, "entry is archived and must be restored before editing") => {
            Some(("词条已归档，编辑前请先恢复", "詞條已封存，編輯前請先恢復"))
        }
        (
            ErrorCode::EntryNotDeletable,
            "only never-published entries without inbound references can be deleted",
        ) => Some((
            "只有从未发布且没有外部引用的词条可以删除",
            "只有從未發布且沒有外部引用的詞條可以刪除",
        )),
        (
            ErrorCode::EntryHasInboundPreboundRelations,
            "entry is selected by another draft relation; remove that prebinding first",
        ) => Some((
            "其他草稿关联已选择此词条，请先解除该预绑定",
            "其他草稿關聯已選擇此詞條，請先解除該預綁定",
        )),
        (
            ErrorCode::EntryHasInboundPublicationRefs,
            "entry is referenced by another active current publication",
        ) => Some((
            "其他当前有效发布引用了此词条",
            "其他目前有效發布引用了此詞條",
        )),
        (
            ErrorCode::EntryHasUnavailablePublicationRefs,
            "entry current publication references an archived or unavailable target",
        ) => Some((
            "词条当前发布引用了已归档或不可用的目标",
            "詞條目前發布引用了已封存或無法使用的目標",
        )),
        (ErrorCode::ReferenceConflict, "a referenced target is changing; retry the command") => {
            Some((
                "引用目标正在变化，请重试操作",
                "引用目標正在變更，請重試操作",
            ))
        }
        (
            ErrorCode::RelationPrebindingFanoutExceeded,
            "relation prebinding reconciliation exceeds the 500 relation limit",
        ) => Some((
            "关联预绑定处理超过 500 条关联的上限",
            "關聯預綁定處理超過 500 條關聯的上限",
        )),
        (ErrorCode::StableNodeIdChanged, "a stable V3 node identity changed") => {
            Some(("稳定的 V3 节点标识已变化", "穩定的 V3 節點識別碼已變更"))
        }
        (ErrorCode::StepNotReachable, "previous step is incomplete") => {
            Some(("请先完成前一步", "請先完成前一步"))
        }
        (ErrorCode::ValidationFailed, "V3 draft validation failed") => {
            Some(("V3 草稿校验失败", "V3 草稿驗證失敗"))
        }
        (ErrorCode::ValidationFailed, "invalid shared form mapping") => {
            Some(("共享词形映射无效", "共用詞形對應無效"))
        }
        (ErrorCode::InvalidRequestBody, "schema_version is required") => {
            Some(("必须提供 schema_version", "必須提供 schema_version"))
        }
        (ErrorCode::InvalidRequestBody, "schema_version must be integer 3") => {
            Some(("schema_version 必须是整数 3", "schema_version 必須是整數 3"))
        }
        (ErrorCode::UnsupportedSchemaVersion, "unsupported schema_version") => {
            Some(("不支持该 schema_version", "不支援此 schema_version"))
        }
        (ErrorCode::ValidationFailed, "V3 contract validation failed") => {
            Some(("V3 契约校验失败", "V3 契約驗證失敗"))
        }
        (ErrorCode::InvalidQuery, "invalid invitation pagination") => {
            Some(("邀请分页参数无效", "邀請分頁參數無效"))
        }
        (ErrorCode::InvalidQuery, "snapshot cursor is invalid or reused") => {
            Some(("快照游标无效或已使用", "快照游標無效或已使用"))
        }
        (ErrorCode::AudioFileTooLarge, "audio file exceeds the size limit") => {
            Some(("音频文件超过大小上限", "音訊檔案超過大小上限"))
        }
        (
            ErrorCode::InvalidAudioKey,
            "invalid or unowned audio upload key; request a new upload URL and upload again",
        ) => Some((
            "音频上传凭据无效或不属于当前账号，请重新获取上传地址并上传",
            "音訊上傳憑證無效或不屬於目前帳號，請重新取得上傳網址並上傳",
        )),
        (ErrorCode::AudioUploadNotCompleted, "audio upload was not completed") => {
            Some(("音频尚未上传完成", "音訊尚未上傳完成"))
        }
        (
            ErrorCode::InvalidRequestBody,
            "original_name must be 1 to 120 characters without control characters",
        ) => Some((
            "original_name 须为 1–120 个字符且不能包含控制字符",
            "original_name 須為 1–120 個字元且不能包含控制字元",
        )),
        (ErrorCode::ServiceUnavailable, "audio storage unavailable") => {
            Some(("音频存储暂不可用", "音訊儲存暫時無法使用"))
        }
        (ErrorCode::InvalidRequestBody, "invalid voice library settings") => {
            Some(("音色库设置无效", "音色庫設定無效"))
        }
        (ErrorCode::SpeechVoiceNotFound, "voice not available") => {
            Some(("音色不可用", "音色無法使用"))
        }
        (ErrorCode::InvalidSpeechPreview, "invalid synthesis request") => {
            Some(("语音合成请求无效", "語音合成請求無效"))
        }
        (ErrorCode::InvalidPathParameter, "invalid preview hash") => {
            Some(("试听摘要无效", "試聽摘要無效"))
        }
        (ErrorCode::InvalidSpeechPreview, "speech provider rejected request") => {
            Some(("语音供应商拒绝了请求", "語音供應商拒絕了請求"))
        }
        (ErrorCode::ServiceUnavailable, "speech preview coordination unavailable") => {
            Some(("语音试听协调服务暂不可用", "語音試聽協調服務暫時無法使用"))
        }
        (ErrorCode::InvalidQuery, "cursor is invalid for this search") => {
            Some(("游标对本次搜索无效", "游標對本次搜尋無效"))
        }
        (ErrorCode::ValidationFailed, "q must normalize to a valid word or phrase surface") => {
            Some((
                "q 规范化后必须是有效的单词或短语",
                "q 正規化後必須是有效的單字或片語",
            ))
        }
        (ErrorCode::InvalidQuery, "page_size must be between 1 and 200") => Some((
            "page_size 必须在 1 到 200 之间",
            "page_size 必須在 1 到 200 之間",
        )),
        (ErrorCode::InvalidHeadword, "surface normalization does not match detection") => {
            Some(("词面规范化结果与检测不一致", "詞面正規化結果與偵測不一致"))
        }
        (
            ErrorCode::InvalidRequestBody,
            "refresh the editor before saving multi-group sense bindings",
        ) => Some((
            "保存多组词义绑定前请刷新编辑器",
            "儲存多組詞義綁定前請重新整理編輯器",
        )),
        (ErrorCode::ValidationFailed, "meanings must belong to a POS in the current V3 forms") => {
            Some((
                "词义必须属于当前 V3 词形中的词性",
                "詞義必須屬於目前 V3 詞形中的詞性",
            ))
        }
        (
            ErrorCode::InvalidRequestBody,
            "component usages are only available for phrase entries",
        ) => Some(("成分用法仅适用于短语词条", "成分用法僅適用於片語詞條")),
        (ErrorCode::InvalidRequestBody, "phrase component must not target the phrase itself") => {
            Some(("短语成分不能指向短语自身", "片語成分不能指向片語本身"))
        }
        (
            ErrorCode::InvalidRequestBody,
            "phrase component target must not itself reference another phrase",
        ) => Some((
            "短语成分目标不能再引用其他短语",
            "片語成分目標不能再引用其他片語",
        )),
        (
            ErrorCode::InvalidRequestBody,
            "resolved component must match a published word or phrase form and sense",
        ) => Some((
            "已解析成分必须匹配已发布单词或短语的词形和词义",
            "已解析成分必須符合已發布單字或片語的詞形和詞義",
        )),
        (
            ErrorCode::ValidationFailed,
            "spelling must contain between 1 and 200 valid codepoints",
        ) => Some((
            "拼写须包含 1–200 个有效 Unicode 码点",
            "拼寫須包含 1–200 個有效 Unicode 碼點",
        )),
        (
            ErrorCode::InvalidRequestBody,
            "select 1 to 50 distinct entries and sentences with positive revisions",
        ) => Some((
            "请选择 1–50 个不重复且版本为正数的词条和例句",
            "請選擇 1–50 個不重複且版本為正數的詞條和例句",
        )),
        (
            ErrorCode::InvalidRequestBody,
            "annotation must contain at most 20 characters and no control characters",
        ) => Some((
            "标注最多 20 个字符且不能包含控制字符",
            "標註最多 20 個字元且不能包含控制字元",
        )),
        (ErrorCode::InvalidRequestBody, "entry ids must be unique and revisions positive") => {
            Some((
                "词条 ID 不能重复且版本必须为正数",
                "詞條 ID 不能重複且版本必須為正數",
            ))
        }
        (ErrorCode::InvalidRequestBody, "form type is not in the catalog") => {
            Some(("词形类型不在目录中", "詞形類型不在目錄中"))
        }
        (ErrorCode::ValidationFailed, "base_lifecycle_revision must be at least 1") => Some((
            "base_lifecycle_revision 必须至少为 1",
            "base_lifecycle_revision 必須至少為 1",
        )),
        (ErrorCode::InvalidQuery, "page_size and deprecated limit must not be sent together") => {
            Some((
                "不能同时提供 page_size 和已弃用的 limit",
                "不能同時提供 page_size 和已棄用的 limit",
            ))
        }
        (ErrorCode::InvalidQuery, "q must not contain NUL characters") => {
            Some(("q 不能包含 NUL 字符", "q 不能包含 NUL 字元"))
        }
        (ErrorCode::InvalidQuery, "q is not a valid headword search") => {
            Some(("q 不是有效的词头搜索", "q 不是有效的詞頭搜尋"))
        }
        (ErrorCode::InvalidQuery, "cursor does not match this search") => {
            Some(("游标与本次搜索不匹配", "游標與本次搜尋不符"))
        }
        (ErrorCode::InvalidQuery, "page must be at least 1") => {
            Some(("page 必须至少为 1", "page 必須至少為 1"))
        }
        (ErrorCode::InvalidQuery, "page_size must be between 1 and 100") => Some((
            "page_size 必须在 1 到 100 之间",
            "page_size 必須在 1 到 100 之間",
        )),
        (ErrorCode::InvalidQuery, "invalid CEFR level") => Some(("CEFR 等级无效", "CEFR 等級無效")),
        (ErrorCode::InvalidQuery, "created_to must be later than created_from") => Some((
            "created_to 必须晚于 created_from",
            "created_to 必須晚於 created_from",
        )),
        (ErrorCode::InvalidRequestBody, "bindings must reference distinct existing senses") => {
            Some((
                "绑定必须引用不同且已存在的词义",
                "綁定必須引用不同且已存在的詞義",
            ))
        }
        (ErrorCode::InvalidRequestBody, "unknown permission") => Some(("未知权限", "未知權限")),
        (ErrorCode::InvalidRequestBody, "unknown permission or excessive selection") => {
            Some(("权限未知或选择数量过多", "權限未知或選擇數量過多"))
        }
        (ErrorCode::InvalidRequestBody, "duplicate permission") => Some(("权限重复", "權限重複")),
        (ErrorCode::InvalidRequestBody, "permission dependency missing") => {
            Some(("缺少权限依赖", "缺少權限依賴"))
        }
        (ErrorCode::InvalidRequestBody, "select 1 to 100 distinct targets") => {
            Some(("请选择 1–100 个不重复的目标", "請選擇 1–100 個不重複的目標"))
        }
        (ErrorCode::InvalidRequestBody, "grant and revoke overlap") => {
            Some(("授予和撤销的权限不能重叠", "授予和撤銷的權限不能重疊"))
        }
        (
            ErrorCode::InvalidRequestBody,
            "cannot grant a permission while revoking its dependency",
        ) => Some(("不能在撤销依赖时授予该权限", "不能在撤銷依賴時授予該權限")),
        (ErrorCode::InvalidRequestBody, "super admin permissions are intrinsic") => {
            Some(("超管权限由身份决定", "超管權限由身分決定"))
        }
        (ErrorCode::InvalidRequestBody, "tag name must contain 1 to 50 characters") => {
            Some(("标签名称须为 1–50 个字符", "標籤名稱須為 1–50 個字元"))
        }
        (ErrorCode::InvalidRequestBody, "invalid tag color") => {
            Some(("标签颜色无效", "標籤顏色無效"))
        }
        _ => return None,
    }?;
    match locale {
        Locale::ZhHans => Some(hans),
        Locale::ZhHant => Some(hant),
        Locale::EnGb | Locale::EnUs => None,
    }
}

fn extra_detail(code: ErrorCode, source: &str, locale: Locale) -> Option<&'static str> {
    let (hant, english) = match (code, source) {
        (ErrorCode::NotFound, "申请不存在") => Some(("找不到申請", "Application not found.")),
        (ErrorCode::NotFound, "用户不存在") => Some(("找不到使用者", "User not found.")),
        (ErrorCode::NotFound, "音频暂不可用") => {
            Some(("音訊暫時無法使用", "Audio is temporarily unavailable."))
        }
        (
            ErrorCode::InvalidRequestBody,
            "此内容包含新版加粗或下划线格式，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版粗體或底線格式，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer bold or underline formatting. Preserve your local input and reload before saving.",
        )),
        (
            ErrorCode::InvalidRequestBody,
            "此内容包含新版文本配音版本，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版文字配音版本，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer text dubbing versions. Preserve your local input and reload before saving.",
        )),
        (
            ErrorCode::InvalidSpeechPreview | ErrorCode::InvalidRequestBody,
            "该模型暂仅支持原速、原调、原音量的自然朗读，不支持风格、停顿或指定音素",
        ) => Some((
            "此模型暫僅支援原速、原調、原音量的自然朗讀，不支援風格、停頓或指定音素",
            "This model currently supports natural reading at the original rate, pitch and volume only; styles, pauses and specified phonemes are unavailable.",
        )),
        (
            ErrorCode::InvalidRequestBody,
            "指定音素尚未验证适用于该原生地区，请明确选择自然朗读或使用匹配地区的音色",
        ) => Some((
            "指定音素尚未驗證適用於該原生地區，請明確選擇自然朗讀或使用符合地區的音色",
            "The specified phoneme has not been verified for this native locale. Choose natural reading or a voice with a matching locale.",
        )),
        (ErrorCode::InvalidRequestBody, "分页参数无效") => {
            Some(("分頁參數無效", "The pagination parameters are invalid."))
        }
        (ErrorCode::AudioStorageNotConfigured, "音频存储尚未配置") => {
            Some(("音訊儲存尚未設定", "Audio storage is not configured."))
        }
        (
            ErrorCode::ValidationFailed,
            "此内容包含新版加粗或下划线格式，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版粗體或底線格式，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer bold or underline formatting. Preserve your local input and reload before saving.",
        )),
        (
            ErrorCode::ValidationFailed,
            "此内容包含新版文本配音版本，请保留本地输入并刷新页面后再保存",
        ) => Some((
            "此內容包含新版文字配音版本，請保留本機輸入並重新整理頁面後再儲存",
            "This content uses newer text dubbing versions. Preserve your local input and reload before saving.",
        )),
        (ErrorCode::InvalidRequestBody, "问题或上下文过长，请缩小问题范围后重试") => {
            Some((
                "問題或上下文過長，請縮小問題範圍後重試",
                "The question or context is too long. Narrow the question and try again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "归档词条不能生成导入候选") => Some((
            "封存詞條不能產生匯入候選",
            "Import candidates cannot be generated for an archived entry.",
        )),
        (ErrorCode::InvalidRequestBody, "词条或当前编辑内容无效，请缩小范围后重试") => {
            Some((
                "詞條或目前編輯內容無效，請縮小範圍後重試",
                "The entry or current edit is invalid. Narrow the scope and try again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "当前编辑内容过大，请缩小问题范围后重试") => {
            Some((
                "目前編輯內容過大，請縮小問題範圍後重試",
                "The current edit is too large. Narrow the question and try again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "查询资料过多，请缩小问题范围后重试") => {
            Some((
                "查詢資料過多，請縮小問題範圍後重試",
                "Too much material was retrieved. Narrow the question and try again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "当前编辑结构无效，请核对草稿后重试") => {
            Some((
                "目前編輯結構無效，請核對草稿後重試",
                "The current edit structure is invalid. Check the draft and try again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "当前编辑内容过多，请缩小问题范围后重试") => {
            Some((
                "目前編輯內容過多，請縮小問題範圍後重試",
                "The current edit contains too much content. Narrow the question and try again.",
            ))
        }
        (ErrorCode::PayloadTooLarge, "图片不能超过10MB") => {
            Some(("圖片不能超過 10MB", "The image cannot exceed 10MB."))
        }
        (ErrorCode::InvalidRequestBody, "无法读取图片") => {
            Some(("無法讀取圖片", "The image could not be read."))
        }
        (ErrorCode::AudioStorageNotConfigured, "音频存储未配置") => {
            Some(("音訊儲存尚未設定", "Audio storage is not configured."))
        }
        (ErrorCode::InvalidQuery, "请输入有效的英文词条名称") => Some((
            "請輸入有效的英文詞條名稱",
            "Enter a valid English entry name.",
        )),
        (ErrorCode::LexiconAssistantRateLimited, "助手正在回答或提问过于频繁，请稍后重试") => {
            Some((
                "助手正在回答或提問過於頻繁，請稍後重試",
                "The assistant is responding or requests are too frequent. Try again later.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "音频清单摘要无效") => {
            Some(("音訊清單摘要無效", "The audio manifest digest is invalid."))
        }
        (ErrorCode::InvalidQuery, "待关联修复搜索须显式使用 draft 视图") => Some((
            "待關聯修復搜尋須明確使用 draft 檢視",
            "Pending link repair searches must explicitly use the draft view.",
        )),
        (ErrorCode::InvalidQuery, "发布序号必须为正数") => Some((
            "發布序號必須為正數",
            "The publication number must be positive.",
        )),
        (ErrorCode::SentenceNotFound, "例句不存在或已删除") => Some((
            "例句不存在或已刪除",
            "The sentence does not exist or has been deleted.",
        )),
        (ErrorCode::InvalidQuery, "分页或查询参数无效") => Some((
            "分頁或查詢參數無效",
            "The pagination or query parameters are invalid.",
        )),
        (ErrorCode::NotFound, "复习不存在") => Some(("找不到複習", "Review not found.")),
        (ErrorCode::NotFound, "复习功能尚未开放") => {
            Some(("複習功能尚未開放", "Review is not available yet."))
        }
        (ErrorCode::SpeechProviderUnavailable, "正式音频生成暂不可用，请稍后重试") => {
            Some((
                "正式音訊產生暫時無法使用，請稍後重試",
                "Published audio generation is temporarily unavailable. Try again later.",
            ))
        }
        (ErrorCode::SpeechStorageUnavailable, "音频存储暂不可用，请使用相同请求重试") => {
            Some((
                "音訊儲存暫時無法使用，請使用相同請求重試",
                "Audio storage is temporarily unavailable. Retry the same request.",
            ))
        }
        (ErrorCode::SpeechPreviewInProgress, "音频正在生成，请稍后重试") => Some((
            "音訊正在產生，請稍後重試",
            "Audio is being generated. Try again later.",
        )),
        (ErrorCode::Forbidden, "生成正式音频需要内容编辑权限") => Some((
            "產生正式音訊需要內容編輯權限",
            "Content editing permission is required to generate published audio.",
        )),
        (ErrorCode::InvalidSpeechPreview, "音色或模型能力未经当前资源确认，不能生成") => {
            Some((
                "音色或模型能力未經目前資源確認，無法產生",
                "The voice or model capability has not been confirmed for this resource and cannot be generated.",
            ))
        }
        (ErrorCode::IdempotencyConflict, "请求编号已用于其他配音内容") => Some((
            "請求編號已用於其他配音內容",
            "The request ID was already used for different dubbing content.",
        )),
        (ErrorCode::InvalidSpeechPreview, "该模型需要新版配音能力，请刷新页面") => {
            Some((
                "此模型需要新版配音功能，請重新整理頁面",
                "This model requires updated dubbing capabilities. Reload the page.",
            ))
        }
        (ErrorCode::InvalidSpeechPreview, "指定发音必须提供源音素地区") => Some((
            "指定發音必須提供來源音素地區",
            "A source phoneme locale is required for a specified pronunciation.",
        )),
        (ErrorCode::InvalidSpeechPreview, "该音色需要新版配音能力，请刷新页面") => {
            Some((
                "此音色需要新版配音功能，請重新整理頁面",
                "This voice requires updated dubbing capabilities. Reload the page.",
            ))
        }
        (ErrorCode::InvalidSpeechPreview, "每种口音只能选择一条默认播放录音") => {
            Some((
                "每種口音只能選擇一條預設播放錄音",
                "Choose only one default recording per accent.",
            ))
        }
        (ErrorCode::InvalidSpeechPreview, "生成音频须绑定配音版本，不能作为上传录音保存") => {
            Some((
                "產生的音訊須綁定配音版本，不能作為上傳錄音儲存",
                "Generated audio must be bound to a dubbing version and cannot be saved as an uploaded recording.",
            ))
        }
        (ErrorCode::InvalidSpeechPreview, "正式音频与当前声音效果不一致，请重新生成或移除音频") => {
            Some((
                "正式音訊與目前聲音效果不一致，請重新產生或移除音訊",
                "The published audio does not match the current voice effect. Regenerate or remove it.",
            ))
        }
        (
            ErrorCode::InvalidSpeechPreview,
            "正式音频与当前正文、读法或参数不一致，请重新生成或移除音频",
        ) => Some((
            "正式音訊與目前正文、讀法或參數不一致，請重新產生或移除音訊",
            "The published audio does not match the current text, pronunciation or settings. Regenerate or remove it.",
        )),
        (ErrorCode::InvalidRequestBody, "等待时长必须为 0–3600 秒") => Some((
            "等待時間必須為 0–3600 秒",
            "The wait must be between 0 and 3600 seconds.",
        )),
        (ErrorCode::NotFound, "分组不存在") => Some(("找不到分組", "Group not found.")),
        (ErrorCode::InvalidSpeechPreview, "配音内容或参数无效，请重新打开编辑器检查") => {
            Some((
                "配音內容或參數無效，請重新開啟編輯器檢查",
                "The dubbing content or settings are invalid. Reopen the editor and check them.",
            ))
        }
        (ErrorCode::InvalidSpeechPreview, "音素地区必须与音色原生地区完全一致") => {
            Some((
                "音素地區必須與音色原生地區完全一致",
                "The phoneme locale must exactly match the voice's native locale.",
            ))
        }
        (
            ErrorCode::InvalidSpeechPreview,
            "指定音素或关联词形尚未验证适用于该原生地区，请明确选择自然朗读或使用匹配地区的音色",
        ) => Some((
            "指定音素或關聯詞形尚未驗證適用於該原生地區，請明確選擇自然朗讀或使用符合地區的音色",
            "The specified phoneme or linked form has not been verified for this native locale. Choose natural reading or a voice with a matching locale.",
        )),
        (
            ErrorCode::InvalidSpeechPreview,
            "当前口音没有满足声音、参数和读法能力的已选推荐，请明确选择或修复配置",
        ) => Some((
            "目前口音沒有符合聲音、參數和讀法能力的已選推薦，請明確選擇或修復設定",
            "No selected recommendation for this accent supports the voice, settings and pronunciation. Choose one or fix the configuration.",
        )),
        (
            ErrorCode::InvalidSpeechPreview,
            "局部池同一供应商官方音色只能保留一个配置，请明确处理历史冲突",
        ) => Some((
            "局部池中同一供應商官方音色只能保留一個設定，請明確處理歷史衝突",
            "Only one configuration per provider voice is allowed in the local pool. Resolve the existing conflict.",
        )),
        (
            ErrorCode::InvalidSpeechPreview,
            "新增直接音素仅允许试听；请使用结构化读法引用或特殊读法保存",
        ) => Some((
            "新增直接音素僅允許試聽；請使用結構化讀法引用或特殊讀法儲存",
            "New direct phonemes are only available for preview. Save a structured pronunciation reference or special pronunciation instead.",
        )),
        (ErrorCode::NotFound, "通知不存在") => Some(("找不到通知", "Notification not found.")),
        (ErrorCode::Forbidden, "当前账号不能申请") => {
            Some(("目前帳號無法申請", "This account cannot apply."))
        }
        (ErrorCode::ServiceUnavailable, "认证材料存储暂不可用") => Some((
            "認證資料儲存暫時無法使用",
            "Certification document storage is temporarily unavailable.",
        )),
        (ErrorCode::NotFound, "材料不存在") => Some(("找不到資料", "Document not found.")),
        (ErrorCode::NotFound, "词条暂不可用") => {
            Some(("詞條暫時無法使用", "The entry is temporarily unavailable."))
        }
        (ErrorCode::ServiceUnavailable, "词库助手暂时不可用，请稍后重试；当前编辑已保留") => {
            Some((
                "詞庫助手暫時無法使用，請稍後重試；目前編輯已保留",
                "The lexicon assistant is temporarily unavailable. Try again later; your current edits are preserved.",
            ))
        }
        (ErrorCode::Forbidden, "需要词库发布权限，且仅超管可以发布他人的草稿") => {
            Some((
                "需要詞庫發布權限，且僅超管可以發布他人的草稿",
                "Lexicon publishing permission is required; only super administrators can publish another person's draft.",
            ))
        }
        (
            ErrorCode::ReferenceConflict,
            "词条被共享例句标注，请先在多维例句库解除对应标注，再移入垃圾桶。",
        ) => Some((
            "詞條被共享例句標註，請先在多維例句庫解除對應標註，再移入垃圾桶。",
            "This entry is annotated by a shared sentence. Remove the annotation in the sentence library before moving it to the trash.",
        )),
        (
            ErrorCode::InboundReferenceConflict,
            "本次修改会破坏其他内容对本词条的引用，请先解除或调整这些引用。",
        ) => Some((
            "本次修改會破壞其他內容對本詞條的引用，請先解除或調整這些引用。",
            "This change would break references to this entry. Remove or adjust those references first.",
        )),
        (ErrorCode::InvalidQuery, "请输入 1–200 字的词条名称") => Some((
            "請輸入 1–200 字的詞條名稱",
            "Enter an entry name of 1–200 characters.",
        )),
        (ErrorCode::NotFound, "关联词已不可用") => Some((
            "關聯詞已無法使用",
            "The related word is no longer available.",
        )),
        (ErrorCode::InvalidRequestBody, "上传录音地区须为 en-GB 或 en-US") => Some((
            "上傳錄音地區須為 en-GB 或 en-US",
            "The uploaded recording locale must be en-GB or en-US.",
        )),
        (ErrorCode::NotFound, "词条不存在") => Some(("找不到詞條", "Entry not found.")),
        (ErrorCode::ReferenceConflict, "词条正在被修改，请稍后重试") => Some((
            "詞條正在修改，請稍後重試",
            "The entry is being modified. Try again later.",
        )),
        (ErrorCode::Forbidden, "仅超管可以永久删除例句") => Some((
            "僅超管可以永久刪除例句",
            "Only super administrators can permanently delete sentences.",
        )),
        (ErrorCode::ReferenceConflict, "已有发布历史的例句不能删除，请使用全局下架") => {
            Some((
                "已有發布歷史的例句不能刪除，請使用全域下架",
                "A sentence with publication history cannot be deleted. Unpublish it globally instead.",
            ))
        }
        (ErrorCode::IdempotencyConflict, "此创建 ID 已用于其他内容") => Some((
            "此建立 ID 已用於其他內容",
            "This creation ID was already used for different content.",
        )),
        (ErrorCode::Forbidden, "仅创建者或超管可以改变例句发布状态") => Some((
            "僅建立者或超管可以變更例句發布狀態",
            "Only the creator or a super administrator can change the sentence publication status.",
        )),
        (ErrorCode::IdempotencyConflict, "幂等键已用于其他请求") => Some((
            "冪等鍵已用於其他請求",
            "The idempotency key was already used for another request.",
        )),
        (ErrorCode::ReferenceConflict, "引用正在被修改，请重试") => Some((
            "引用正在修改，請重試",
            "The reference is being modified. Try again.",
        )),
        (ErrorCode::ReferenceConflict, "宿主正在修改，请重试") => Some((
            "宿主正在修改，請重試",
            "The host is being modified. Try again.",
        )),
        (ErrorCode::Forbidden, "仅超管可以管理例句垃圾桶") => Some((
            "僅超管可以管理例句垃圾桶",
            "Only super administrators can manage the sentence trash.",
        )),
        (ErrorCode::ReferenceConflict, "已有发布历史的例句不能移入垃圾桶，请使用撤回") => {
            Some((
                "已有發布歷史的例句不能移入垃圾桶，請使用撤回",
                "A sentence with publication history cannot be moved to the trash. Withdraw it instead.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "推荐套身份不完整或重复") => Some((
            "推薦套識別資料不完整或重複",
            "Recommendation set identities are incomplete or duplicated.",
        )),
        (ErrorCode::InvalidRequestBody, "分组身份不匹配") => {
            Some(("分組識別資料不符", "The group identity does not match."))
        }
        (ErrorCode::InvalidRequestBody, "配对请求必须使用 X-TSZ-Dubbing-Groups: v3") => {
            Some((
                "配對請求必須使用 X-TSZ-Dubbing-Groups: v3",
                "Pairing requests require X-TSZ-Dubbing-Groups: v3.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "转换配对须保留所有历史推荐身份，请明确补齐配对") => {
            Some((
                "轉換配對須保留所有歷史推薦識別資料，請明確補齊配對",
                "Conversion must preserve all historical recommendation identities. Complete the pairs.",
            ))
        }
        (ErrorCode::NotFound, "内容不存在或已归档") => Some((
            "找不到內容或內容已封存",
            "The content does not exist or has been archived.",
        )),
        (
            ErrorCode::InvalidSpeechPreview,
            "指定音素尚未验证适用于该原生地区，请明确选择自然朗读或使用匹配地区的音色",
        ) => Some((
            "指定音素尚未驗證適用於該原生地區，請明確選擇自然朗讀或使用符合地區的音色",
            "The specified phoneme has not been verified for this native locale. Choose natural reading or a voice with a matching locale.",
        )),
        (ErrorCode::InvalidRequestBody, "英文语音或富文本设置无效") => Some((
            "英文語音或富文字設定無效",
            "The English audio or rich text settings are invalid.",
        )),
        (ErrorCode::InvalidRequestBody, "请选择例句等级") => {
            Some(("請選擇例句等級", "Select a sentence level."))
        }
        (ErrorCode::InvalidRequestBody, "请通过句内标注关联具体词条") => Some((
            "請透過句內標註關聯具體詞條",
            "Link a specific entry using an annotation in the sentence.",
        )),
        (ErrorCode::InvalidRequestBody, "至少填写一条译文") => {
            Some(("至少填寫一條譯文", "Enter at least one translation."))
        }
        (ErrorCode::InvalidRequestBody, "译文不能为空或重复") => Some((
            "譯文不能為空或重複",
            "Translations cannot be empty or duplicated.",
        )),
        (ErrorCode::InvalidRequestBody, "译文标注无效") => {
            Some(("譯文標註無效", "The translation annotation is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "译文主项不一致") => {
            Some(("譯文主項不一致", "The primary translation does not match."))
        }
        (ErrorCode::InvalidRequestBody, "英文例句不能为空或过长") => Some((
            "英文例句不能為空或過長",
            "The English sentence cannot be empty or too long.",
        )),
        (ErrorCode::InvalidRequestBody, "英文格式无效") => {
            Some(("英文格式無效", "The English formatting is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "英文标注无效") => {
            Some(("英文標註無效", "The English annotation is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "例句或标注数量无效") => Some((
            "例句或標註數量無效",
            "The number of sentences or annotations is invalid.",
        )),
        (ErrorCode::InvalidRequestBody, "标注 ID 或片段无效") => Some((
            "標註 ID 或片段無效",
            "The annotation ID or segment is invalid.",
        )),
        (ErrorCode::InvalidRequestBody, "标注方言不存在") => {
            Some(("標註方言不存在", "The annotation dialect does not exist."))
        }
        (ErrorCode::InvalidRequestBody, "标注位置与例句不一致，请重新选择") => {
            Some((
                "標註位置與例句不一致，請重新選擇",
                "The annotation position does not match the sentence. Select it again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "句内标注不能重叠") => {
            Some(("句內標註不能重疊", "Sentence annotations cannot overlap."))
        }
        (ErrorCode::InvalidRequestBody, "待关联词条信息无效") => Some((
            "待關聯詞條資訊無效",
            "The pending entry information is invalid.",
        )),
        (ErrorCode::InvalidRequestBody, "关联词条不存在或已归档") => Some((
            "關聯詞條不存在或已封存",
            "The linked entry does not exist or has been archived.",
        )),
        (ErrorCode::InvalidRequestBody, "待关联词面无效") => {
            Some(("待關聯詞面無效", "The pending text to link is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "垃圾桶只支持草稿视图") => Some((
            "垃圾桶只支援草稿檢視",
            "The trash supports only the draft view.",
        )),
        (ErrorCode::InvalidRequestBody, "目标词条不存在、已归档或没有可匹配词形") => {
            Some((
                "目標詞條不存在、已封存或沒有可比對詞形",
                "The target entry does not exist, is archived or has no matching form.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "目标词形无效") => {
            Some(("目標詞形無效", "The target form is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "来源词条与词义必须同时提供") => Some((
            "來源詞條與詞義必須同時提供",
            "Provide both the source entry and sense.",
        )),
        (ErrorCode::InvalidRequestBody, "当前词条与词义必须同时提供") => Some((
            "目前詞條與詞義必須同時提供",
            "Provide both the current entry and sense.",
        )),
        (ErrorCode::InvalidRequestBody, "例句 ID 不可更改") => {
            Some(("例句 ID 不可變更", "The sentence ID cannot be changed."))
        }
        (ErrorCode::InvalidRequestBody, "请选择完整单词或短语") => {
            Some(("請選擇完整單字或片語", "Select a complete word or phrase."))
        }
        (ErrorCode::InvalidRequestBody, "请选择完整单词，不能截取单词的一部分") => {
            Some((
                "請選擇完整單字，不能截取單字的一部分",
                "Select the complete word, not part of it.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "关联短语不能跨越句子边界") => Some((
            "關聯片語不能跨越句子邊界",
            "A linked phrase cannot cross sentence boundaries.",
        )),
        (ErrorCode::InvalidRequestBody, "所选文字不能构成单词或短语") => Some((
            "所選文字不能構成單字或片語",
            "The selected text does not form a word or phrase.",
        )),
        (ErrorCode::InvalidRequestBody, "所选词面无效") => {
            Some(("所選詞面無效", "The selected text is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "请先将句中的单词或短语关联到当前词义") => {
            Some((
                "請先將句中的單字或片語關聯到目前詞義",
                "First link the word or phrase in the sentence to the current sense.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "旧关联尚未选择具体词义，请补全或清除") => {
            Some((
                "舊關聯尚未選擇具體詞義，請補全或清除",
                "The old link has no specific sense. Complete or remove it.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "关联词义或词形已失效，请按词条、词形、词义重新选择") => {
            Some((
                "關聯詞義或詞形已失效，請依詞條、詞形、詞義重新選擇",
                "The linked sense or form is no longer valid. Select the entry, form and sense again.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "词条查询参数无效") => Some((
            "詞條查詢參數無效",
            "The entry query parameters are invalid.",
        )),
        (ErrorCode::InvalidRequestBody, "每段正文最多引用 8 条不重复的音频") => {
            Some((
                "每段正文最多引用 8 條不重複的音訊",
                "Each text segment can reference at most 8 distinct audio items.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "音频不存在、不可访问或已被其他内容使用") => {
            Some((
                "音訊不存在、無法存取或已被其他內容使用",
                "The audio does not exist, is inaccessible or is used by other content.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "音频不存在、不可访问或正在回收") => Some((
            "音訊不存在、無法存取或正在回收",
            "The audio does not exist, is inaccessible or is being reclaimed.",
        )),
        (ErrorCode::InvalidRequestBody, "发布前必须补全旧词义关联") => Some((
            "發布前必須補全舊詞義關聯",
            "Complete the old sense links before publishing.",
        )),
        (ErrorCode::InvalidRequestBody, "关联目标尚未发布或已归档，请显式选择依赖一起发布") => {
            Some((
                "關聯目標尚未發布或已封存，請明確選擇依賴一起發布",
                "The linked target is unpublished or archived. Explicitly select the dependency to publish with it.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "已发布目标词义、词形或词面不匹配") => {
            Some((
                "已發布目標詞義、詞形或詞面不符",
                "The published target sense, form or text does not match.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "词面无效") => {
            Some(("詞面無效", "The text is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "关联必须精确到词义") => {
            Some(("關聯必須精確到詞義", "The link must specify a sense."))
        }
        (ErrorCode::InvalidRequestBody, "例句尚未发布") => {
            Some(("例句尚未發布", "The sentence has not been published."))
        }
        (ErrorCode::InvalidRequestBody, "下架原因须为 1–2000 个字符") => Some((
            "下架原因須為 1–2000 個字元",
            "The unpublishing reason must be 1–2000 characters.",
        )),
        (ErrorCode::InvalidRequestBody, "仅支持 V3 词条") => {
            Some(("僅支援 V3 詞條", "Only V3 entries are supported."))
        }
        (ErrorCode::InvalidRequestBody, "词义不属于宿主词条") => Some((
            "詞義不屬於宿主詞條",
            "The sense does not belong to the host entry.",
        )),
        (ErrorCode::InvalidRequestBody, "例句未关联该词义") => Some((
            "例句未關聯該詞義",
            "The sentence is not linked to this sense.",
        )),
        (ErrorCode::InvalidRequestBody, "认证状态无效") => {
            Some(("認證狀態無效", "The certification status is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "姓名为1至50字，认证说明为1至2000字") => {
            Some((
                "姓名為 1 至 50 字，認證說明為 1 至 2000 字",
                "The name must be 1–50 characters and the certification description 1–2000 characters.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "请填写有效的手机号或邮箱") => Some((
            "請填寫有效的手機號碼或電郵地址",
            "Enter a valid phone number or email address.",
        )),
        (ErrorCode::InvalidRequestBody, "学历证书和语言成绩各需1至10张图片") => {
            Some((
                "學歷證書和語言成績各需 1 至 10 張圖片",
                "Provide 1–10 images each for education certificates and language results.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "同一材料不能重复提交") => Some((
            "同一資料不能重複提交",
            "The same document cannot be submitted twice.",
        )),
        (ErrorCode::InvalidRequestBody, "材料无效、已过期或不属于当前账号") => {
            Some((
                "資料無效、已過期或不屬於目前帳號",
                "The document is invalid, expired or does not belong to this account.",
            ))
        }
        (ErrorCode::InvalidRequestBody, "驳回必须填写原因") => {
            Some(("駁回必須填寫原因", "Enter a reason for rejection."))
        }
        (ErrorCode::InvalidRequestBody, "原因不得超过2000字") => Some((
            "原因不得超過 2000 字",
            "The reason cannot exceed 2000 characters.",
        )),
        (ErrorCode::InvalidRequestBody, "撤销原因需填写1至2000字") => Some((
            "撤銷原因需填寫 1 至 2000 字",
            "The withdrawal reason must be 1–2000 characters.",
        )),
        (ErrorCode::InvalidRequestBody, "材料类型无效") => {
            Some(("資料類型無效", "The document type is invalid."))
        }
        (ErrorCode::InvalidRequestBody, "只支持JPEG、PNG和WebP图片") => Some((
            "只支援 JPEG、PNG 和 WebP 圖片",
            "Only JPEG, PNG and WebP images are supported.",
        )),
        (ErrorCode::InvalidRequestBody, "图片需在10MB以内且不能为空") => Some((
            "圖片需在 10MB 以內且不能為空",
            "The image must be nonempty and no larger than 10MB.",
        )),
        (ErrorCode::InvalidRequestBody, "图片格式与内容不一致") => Some((
            "圖片格式與內容不一致",
            "The image format does not match its content.",
        )),
        (ErrorCode::InvalidRequestBody, "图片损坏或像素过大") => Some((
            "圖片損壞或像素過大",
            "The image is corrupt or has too many pixels.",
        )),
        (ErrorCode::InvalidRequestBody, "待提交材料过多，请删除不需要的图片后重试") => {
            Some((
                "待提交資料過多，請刪除不需要的圖片後重試",
                "Too many documents are pending. Remove unneeded images and try again.",
            ))
        }
        _ => return None,
    }?;
    match locale {
        Locale::ZhHans => None,
        Locale::ZhHant => Some(hant),
        Locale::EnGb | Locale::EnUs => Some(english),
    }
}

fn detail(code: ErrorCode, source: &str, locale: Locale) -> Option<&'static str> {
    english_detail(code, source, locale)
        .or_else(|| extra_detail(code, source, locale))
        .or_else(|| {
            details_for(code)
                .iter()
                .find(|m| m.0 == source)
                .map(|m| match locale {
                    Locale::ZhHans => m.1,
                    Locale::ZhHant => m.2,
                    Locale::EnGb | Locale::EnUs => m.3,
                })
                .or_else(|| {
                    source
                        .eq_ignore_ascii_case(code.descriptor().title)
                        .then(|| title(code, locale))
                })
        })
}

fn details_for(
    code: ErrorCode,
) -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    // Exact code + registered source text only; no substring or supplier matching.
    match code {
        ErrorCode::InvalidToken => &[
            (
                "invalid token",
                "会话已失效，请重新登录",
                "工作階段已失效，請重新登入",
                "Your session is invalid. Sign in again.",
            ),
            (
                "会话已失效",
                "会话已失效",
                "工作階段已失效",
                "Your session is invalid. Sign in again.",
            ),
        ],
        ErrorCode::InvalidRefreshToken => &[(
            "invalid refresh token",
            "会话无法恢复，请重新登录",
            "無法恢復工作階段，請重新登入",
            "Your session could not be restored. Sign in again.",
        )],
        ErrorCode::PasswordMissing => &[(
            "password is missing",
            "请输入密码",
            "請輸入密碼",
            "Enter your password.",
        )],
        ErrorCode::PasswordTooShort => &[(
            "password is too short",
            "密码至少需要 15 个字符",
            "密碼至少需要 15 個字元",
            "Use at least 15 characters for your password.",
        )],
        ErrorCode::PasswordTooLong => &[(
            "password is too long",
            "密码不能超过 128 个字符",
            "密碼不能超過 128 個字元",
            "Use no more than 128 characters for your password.",
        )],
        ErrorCode::PasswordTooWeak => &[(
            "password is too weak",
            "密码过于简单，请更换密码",
            "密碼過於簡單，請更換密碼",
            "Choose a stronger password.",
        )],
        ErrorCode::PasswordCompromised => &[(
            "password has appeared in a compromised-password list",
            "该密码已泄露，请更换密码",
            "此密碼已外洩，請更換密碼",
            "This password has appeared in a data breach. Choose a different password.",
        )],
        ErrorCode::InvalidCredentials => &[
            (
                "invalid credentials",
                "账号或验证信息不正确",
                "帳號或驗證資料不正確",
                "Incorrect account or credentials.",
            ),
            (
                "current password is incorrect",
                "当前密码不正确",
                "目前密碼不正確",
                "Your current password is incorrect.",
            ),
        ],
        ErrorCode::InvalidOtpCode => &[
            (
                "invalid code",
                "验证码无效或已过期",
                "驗證碼無效或已過期",
                "The verification code is invalid or has expired.",
            ),
            (
                "invalid or expired verification code",
                "验证码无效或已过期",
                "驗證碼無效或已過期",
                "The verification code is invalid or has expired.",
            ),
        ],
        ErrorCode::InvalidJson => &[(
            "invalid JSON",
            "JSON 格式无效",
            "JSON 格式無效",
            "Invalid JSON.",
        )],
        ErrorCode::InvalidRequestBody => &[
            (
                "模型或 API Key 配置无效，请检查后重试",
                "模型或 API Key 配置无效，请检查后重试",
                "模型或 API Key 設定無效，請檢查後重試",
                "The model or API key configuration is invalid. Check it and try again.",
            ),
            (
                "ChatGPT 账号操作未完成，请检查账号状态或取消正在进行的登录后重试",
                "ChatGPT 账号操作未完成，请检查账号状态或取消正在进行的登录后重试",
                "ChatGPT 帳號操作未完成，請檢查帳號狀態或取消進行中的登入後重試",
                "The ChatGPT account operation did not complete. Check the account status or cancel the ongoing sign-in and try again.",
            ),
            (
                "当前没有可重新确认的词义，请先恢复来源或补足可靠干扰项",
                "当前没有可重新确认的词义，请先恢复来源或补足可靠干扰项",
                "目前沒有可重新確認的詞義，請先恢復來源或補足可靠干擾項",
                "There are no senses available to reconfirm. Restore the sources or add reliable distractors first.",
            ),
            (
                "单选策略无效",
                "单选策略无效",
                "單選策略無效",
                "Invalid multiple-choice policy.",
            ),
            (
                "所选选项不存在",
                "所选选项不存在",
                "找不到所選選項",
                "The selected option does not exist.",
            ),
            (
                "级别或英美档位无效",
                "级别或英美档位无效",
                "級別或英美檔位無效",
                "The level or British/American variant is invalid.",
            ),
            (
                "需要题干、解析和四个选项",
                "需要题干、解析和四个选项",
                "需要題幹、解析和四個選項",
                "A question stem, explanation and four options are required.",
            ),
            (
                "四个选项须来自不同词条且词形、档位与级别有效",
                "四个选项须来自不同词条且词形、档位与级别有效",
                "四個選項須來自不同詞條且詞形、檔位與級別有效",
                "The four options must come from different entries, with valid word forms, variants and levels.",
            ),
            (
                "选项之间存在相同的有效拼写",
                "选项之间存在相同的有效拼写",
                "選項之間存在相同的有效拼寫",
                "The options contain identical valid spellings.",
            ),
            (
                "干扰项也具有目标释义",
                "干扰项也具有目标释义",
                "干擾項也具有目標釋義",
                "A distractor also has the target definition.",
            ),
            (
                "正确项无效",
                "正确项无效",
                "正確項無效",
                "The correct option is invalid.",
            ),
            (
                "正确项必须是委托目标",
                "正确项必须是委托目标",
                "正確項必須是委託目標",
                "The correct option must be the requested target.",
            ),
            (
                "题干泄露正确词形",
                "题干泄露正确词形",
                "題幹洩露正確詞形",
                "The question stem reveals the correct word form.",
            ),
            (
                "text must be nonempty, trimmed and within limit",
                "文本不能为空、含首尾空白或超过长度限制",
                "文字不能為空、含首尾空白或超過長度限制",
                "Text must be nonempty, have no surrounding whitespace and be within the length limit.",
            ),
            (
                "答案过长",
                "答案过长",
                "答案過長",
                "The answer is too long.",
            ),
            (
                "答案不能为空、包含控制字符或超过 200 字",
                "答案不能为空、包含控制字符或超过 200 字",
                "答案不能為空、包含控制字元或超過 200 字",
                "The answer must not be empty, contain control characters or exceed 200 characters.",
            ),
            (
                "练习模式无效",
                "练习模式无效",
                "練習模式無效",
                "Invalid practice mode.",
            ),
            (
                "请选择 1–5 个不重复词表",
                "请选择 1–5 个不重复词表",
                "請選擇 1–5 個不重複詞表",
                "Select 1–5 distinct word lists.",
            ),
            (
                "每日题量须为 1–200；长期任务不设每日题量和截止",
                "每日题量须为 1–200；长期任务不设每日题量和截止",
                "每日題量須為 1–200；長期任務不設每日題量和截止",
                "Daily tasks require 1–200 questions per day. Long-term tasks have no daily question count or end time.",
            ),
            (
                "名称须为 1–100 字",
                "名称须为 1–100 字",
                "名稱須為 1–100 字",
                "The name must contain 1–100 characters.",
            ),
            (
                "来源超过 1000 个词条，请拆分任务",
                "来源超过 1000 个词条，请拆分任务",
                "來源超過 1000 個詞條，請拆分任務",
                "The sources exceed 1,000 entries. Split the task.",
            ),
            (
                "候选超过 2000 题，请拆分任务",
                "候选超过 2000 题，请拆分任务",
                "候選超過 2000 題，請拆分任務",
                "There are more than 2,000 candidate questions. Split the task.",
            ),
            (
                "结束时间必须晚于当前时间",
                "结束时间必须晚于当前时间",
                "結束時間必須晚於目前時間",
                "The end time must be later than the current time.",
            ),
            (
                "不能查询未来奖励日期",
                "不能查询未来奖励日期",
                "不能查詢未來獎勵日期",
                "You cannot query future reward dates.",
            ),
            (
                "严格单选策略只适用于单选任务",
                "严格单选策略只适用于单选任务",
                "嚴格單選策略只適用於單選任務",
                "The strict multiple-choice policy applies only to multiple-choice tasks.",
            ),
            (
                "每次新学和到期复习最多 20 题，含重练总计最多 30 题",
                "每次新学和到期复习最多 20 题，含重练总计最多 30 题",
                "每次新學和到期複習最多 20 題，含重練總計最多 30 題",
                "Each batch allows up to 20 new and due-review questions, or 30 questions including repeat practice.",
            ),
            ("题型无效", "题型无效", "題型無效", "Invalid question type."),
            (
                "长期任务不设日期计划",
                "长期任务不设日期计划",
                "長期任務不設日期計畫",
                "Long-term tasks do not have a date schedule.",
            ),
            (
                "名称须为 1–50 字",
                "名称须为 1–50 字",
                "名稱須為 1–50 字",
                "The name must contain 1–50 characters.",
            ),
            (
                "词表续练仅支持单份词表的长期练习",
                "词表续练仅支持单份词表的长期练习",
                "詞表續練僅支援單份詞表的長期練習",
                "Continued word list practice supports only long-term practice with a single word list.",
            ),
            (
                "部分内容暂不可出单选，请核对预览并确认仅使用可用内容创建",
                "部分内容暂不可出单选，请核对预览并确认仅使用可用内容创建",
                "部分內容暫時無法出單選題，請核對預覽並確認僅使用可用內容建立",
                "Some content cannot currently produce multiple-choice questions. Review the preview and confirm that only available content will be used to create the task.",
            ),
            (
                "部分内容暂不可出单选，请确认仅使用可用内容开启新轮",
                "部分内容暂不可出单选，请确认仅使用可用内容开启新轮",
                "部分內容暫時無法出單選題，請確認僅使用可用內容開啟新輪",
                "Some content cannot currently produce multiple-choice questions. Confirm that only available content will be used to start the new run.",
            ),
            (
                "所选内容暂不可出单选题，请查看暂不可用内容与原因",
                "所选内容暂不可出单选题，请查看暂不可用内容与原因",
                "所選內容暫時無法出單選題，請查看暫時無法使用的內容與原因",
                "The selected content cannot currently produce multiple-choice questions. Check the unavailable content and reasons.",
            ),
            (
                "词表中没有可出题内容，请检查中文释义和英文词面",
                "词表中没有可出题内容，请检查中文释义和英文词面",
                "詞表中沒有可出題內容，請檢查中文釋義和英文詞面",
                "The word lists have no content suitable for questions. Check the Chinese definitions and English headwords.",
            ),
            (
                "开始日期不得早于当前学习日",
                "开始日期不得早于当前学习日",
                "開始日期不得早於目前學習日",
                "The start date must not be earlier than the current learning day.",
            ),
            (
                "结束日期不得早于开始日期",
                "结束日期不得早于开始日期",
                "結束日期不得早於開始日期",
                "The end date must not be earlier than the start date.",
            ),
            (
                "日期计划容量不足，请调整日期或新建任务",
                "日期计划容量不足，请调整日期或新建任务",
                "日期計畫容量不足，請調整日期或新建任務",
                "The schedule has insufficient capacity. Adjust the dates or create a new task.",
            ),
            (
                "invalid coin request",
                "币请求无效",
                "幣請求無效",
                "Invalid coin request.",
            ),
            (
                "invalid coin balance",
                "余额格式无效",
                "餘額格式無效",
                "Invalid balance format.",
            ),
            (
                "invalid request body",
                "请求内容无效",
                "請求內容無效",
                "Invalid request body.",
            ),
            (
                "at least one preference is required",
                "请至少提供一项偏好",
                "請至少提供一項偏好",
                "Provide at least one preference.",
            ),
        ],
        ErrorCode::InvalidQuery => &[
            (
                "每次最多查询 100 项",
                "每次最多查询 100 项",
                "每次最多查詢 100 項",
                "Query at most 100 items at a time.",
            ),
            (
                "item_ids 须为逗号分隔的 UUID",
                "item_ids 须为逗号分隔的 UUID",
                "item_ids 須為逗號分隔的 UUID",
                "item_ids must be comma-separated UUIDs.",
            ),
            (
                "invalid coin pagination",
                "币记录分页参数无效",
                "幣記錄分頁參數無效",
                "Invalid coin pagination.",
            ),
            (
                "account search required",
                "请输入账号搜索条件",
                "請輸入帳號搜尋條件",
                "Enter an account search query.",
            ),
            (
                "invalid operation filters",
                "操作筛选条件无效",
                "操作篩選條件無效",
                "Invalid operation filters.",
            ),
            (
                "operation search exceeds 200 characters",
                "操作搜索不能超过 200 字",
                "操作搜尋不能超過 200 字",
                "The operation search must not exceed 200 characters.",
            ),
            (
                "分页范围无效",
                "分页范围无效",
                "分頁範圍無效",
                "Invalid pagination range.",
            ),
            (
                "invalid pagination or search",
                "分页或搜索参数无效",
                "分頁或搜尋參數無效",
                "Invalid pagination or search parameters.",
            ),
            (
                "每批须包含 1–100 个词条",
                "每批须包含 1–100 个词条",
                "每批須包含 1–100 個詞條",
                "Each batch must contain 1–100 entries.",
            ),
            (
                "需提供 1–100 个词表 ID",
                "需提供 1–100 个词表 ID",
                "需提供 1–100 個詞表 ID",
                "Provide 1–100 word list IDs.",
            ),
            (
                "invalid query parameters",
                "查询参数无效",
                "查詢參數無效",
                "Invalid query parameters.",
            ),
            (
                "invalid pagination",
                "分页参数无效",
                "分頁參數無效",
                "Invalid pagination.",
            ),
            (
                "分页参数无效",
                "分页参数无效",
                "分頁參數無效",
                "Invalid pagination.",
            ),
        ],
        ErrorCode::InvalidPathParameter => &[
            (
                "无效的登录请求",
                "无效的登录请求",
                "無效的登入請求",
                "Invalid sign-in request.",
            ),
            (
                "locale 须为 en-GB 或 en-US",
                "locale 须为 en-GB 或 en-US",
                "locale 須為 en-GB 或 en-US",
                "locale must be en-GB or en-US.",
            ),
            (
                "invalid path parameter",
                "路径参数无效",
                "路徑參數無效",
                "Invalid path parameter.",
            ),
        ],
        ErrorCode::PayloadTooLarge => &[(
            "request body is too large",
            "请求内容过大",
            "請求內容過大",
            "The request body is too large.",
        )],
        ErrorCode::UnsupportedLanguage => &[
            (
                "interface language is not enabled",
                "该界面语言尚未开放",
                "此介面語言尚未開放",
                "This interface language is not enabled.",
            ),
            (
                "unsupported language",
                "不支持该语言",
                "不支援此語言",
                "This language is not supported.",
            ),
        ],
        ErrorCode::InvalidPhone => &[
            (
                "phone is empty",
                "请输入手机号",
                "請輸入手機號碼",
                "Enter a phone number.",
            ),
            (
                "phone is invalid",
                "手机号无效",
                "手機號碼無效",
                "Invalid phone number.",
            ),
            (
                "phone is missing",
                "请输入手机号",
                "請輸入手機號碼",
                "Enter a phone number.",
            ),
            (
                "invalid phone",
                "手机号无效",
                "手機號碼無效",
                "Invalid phone number.",
            ),
        ],
        ErrorCode::InvalidEmail => &[(
            "invalid email",
            "邮箱地址无效",
            "電郵地址無效",
            "Invalid email address.",
        )],
        ErrorCode::PhoneBindingRequired => &[
            (
                "phone binding required",
                "请先绑定手机号",
                "請先綁定手機號碼",
                "Link a phone number first.",
            ),
            (
                "请先绑定手机号",
                "请先绑定手机号",
                "請先綁定手機號碼",
                "Link a phone number first.",
            ),
        ],
        ErrorCode::PhoneUnbindForbidden => &[(
            "phone cannot be unbound; replace it instead",
            "手机号只能更换，不能解绑",
            "手機號碼只能更換，不能解除綁定",
            "You can replace your phone number, but cannot unlink it.",
        )],
        ErrorCode::AccountDisabled => &[
            (
                "forbidden",
                "账号已停用",
                "帳號已停用",
                "This account is disabled.",
            ),
            (
                "user is not active",
                "账号已停用",
                "帳號已停用",
                "This account is disabled.",
            ),
        ],
        ErrorCode::MustChangePassword => &[(
            "password change required",
            "请先修改密码",
            "請先修改密碼",
            "Change your password first.",
        )],
        ErrorCode::AccountLocked => &[
            (
                "account temporarily locked due to too many failed login attempts",
                "登录失败次数过多，账号暂时锁定",
                "登入失敗次數過多，帳號暫時鎖定",
                "This account is temporarily locked after too many failed sign-in attempts.",
            ),
            (
                "admin is locked",
                "管理员账号暂时锁定",
                "管理員帳號暫時鎖定",
                "This administrator account is temporarily locked.",
            ),
        ],
        ErrorCode::OtpRateLimited => &[(
            "too many requests",
            "请求过于频繁，请稍后重试",
            "請求過於頻繁，請稍後重試",
            "Too many requests. Try again later.",
        )],
        ErrorCode::LoginRateLimited => &[(
            "too many login attempts",
            "登录尝试过于频繁，请稍后重试",
            "登入嘗試過於頻繁，請稍後重試",
            "Too many sign-in attempts. Try again later.",
        )],
        ErrorCode::OtpUnavailable => &[(
            "OTP unavailable",
            "验证码服务暂不可用，请稍后重试",
            "驗證碼服務暫時無法使用，請稍後重試",
            "Verification codes are temporarily unavailable. Try again later.",
        )],
        ErrorCode::LoginUnavailable => &[(
            "login temporarily unavailable",
            "登录服务暂不可用，请稍后重试",
            "登入服務暫時無法使用，請稍後重試",
            "Sign-in is temporarily unavailable. Try again later.",
        )],
        ErrorCode::Forbidden => &[
            (
                "only a super admin may operate another admin wallet",
                "只有超级管理员可以操作其他管理员的钱包",
                "只有超級管理員可以操作其他管理員的錢包",
                "Only a super administrator can operate another administrator's wallet.",
            ),
            (
                "请先完成作答",
                "请先完成作答",
                "請先完成作答",
                "Complete your answer first.",
            ),
            (
                "需要学生资格",
                "需要学生资格",
                "需要學生資格",
                "Student eligibility is required.",
            ),
            (
                "cannot unbind last contact",
                "不能解绑最后一种联系方式",
                "不能解除綁定最後一種聯絡方式",
                "You cannot unlink your last contact method.",
            ),
            (
                "不能给自己的词表点赞或收藏",
                "不能给自己的词表点赞或收藏",
                "不能給自己的詞表按讚或收藏",
                "You cannot like or favourite your own word list.",
            ),
            (
                "不能给自己的词表投币",
                "不能给自己的词表投币",
                "不能給自己的詞表投幣",
                "You cannot tip your own word list.",
            ),
            (
                "用户词表须由作者申请公开",
                "用户词表须由作者申请公开",
                "用戶詞表須由作者申請公開",
                "The author must request publication of a user word list.",
            ),
            (
                "permission required",
                "没有操作权限",
                "沒有操作權限",
                "You do not have permission to perform this action.",
            ),
            (
                "super admin required",
                "需要超级管理员权限",
                "需要超級管理員權限",
                "Super administrator access is required.",
            ),
        ],
        ErrorCode::RevisionConflict => &[
            (
                "发布音频缺失、不可用或与当前内容不一致，请先完成音频检查",
                "发布音频缺失、不可用或与当前内容不一致，请先完成音频检查",
                "發布音訊缺失、無法使用或與目前內容不符，請先完成音訊檢查",
                "Published audio is missing, unavailable or inconsistent with the current content. Complete the audio checks first.",
            ),
            (
                "任务或设置已变化，请重新载入",
                "任务或设置已变化，请重新载入",
                "任務或設定已變更，請重新載入",
                "The task or settings have changed. Reload them.",
            ),
            (
                "有效推荐已变化，请重新解析并生成音频",
                "有效推荐已变化，请重新解析并生成音频",
                "有效推薦已變更，請重新解析並產生音訊",
                "The active recommendations have changed. Parse them again and regenerate the audio.",
            ),
            (
                "词条版本已更新，请核对当前编辑后重新提问",
                "词条版本已更新，请核对当前编辑后重新提问",
                "詞條版本已更新，請核對目前編輯後重新提問",
                "The entry version has changed. Review your current edits before asking again.",
            ),
            (
                "only draft entries can restore publication history",
                "只有草稿词条可以恢复发布历史",
                "只有草稿詞條可以恢復發布歷史",
                "Only draft entries can restore publication history.",
            ),
            (
                "only archived entries can be restored as drafts",
                "只有归档词条可以恢复为草稿",
                "只有封存詞條可以恢復為草稿",
                "Only archived entries can be restored as drafts.",
            ),
            (
                "此组已采用英美配对，请更新客户端后编辑，原配置已保留",
                "此组已采用英美配对，请更新客户端后编辑，原配置已保留",
                "此組已採用英美配對，請更新用戶端後編輯，原設定已保留",
                "This group now uses British and American voice pairs. Update your client before editing. Your original settings have been retained.",
            ),
            (
                "voice settings changed; reload before saving",
                "音色设置已变化，请重新加载后保存",
                "音色設定已變更，請重新載入後儲存",
                "Voice settings have changed. Reload before saving.",
            ),
            (
                "分组已升级为单组保存，请保留输入并刷新页面",
                "分组已升级为单组保存，请保留输入并刷新页面",
                "分組已升級為單組儲存，請保留輸入並重新整理頁面",
                "Groups now save individually. Keep your input and reload the page.",
            ),
            (
                "分组名称已被其他组使用，请保留草稿并重新载入",
                "分组名称已被其他组使用，请保留草稿并重新载入",
                "分組名稱已被其他組使用，請保留草稿並重新載入",
                "Another group is using this name. Keep your draft and reload.",
            ),
            (
                "更新期间正文或推荐已变化",
                "更新期间正文或推荐已变化",
                "更新期間正文或推薦已變更",
                "The content or recommendations changed during the update.",
            ),
            (
                "音频探测与当前内容不匹配",
                "音频探测与当前内容不匹配",
                "音訊探測與目前內容不符",
                "The audio probe does not match the current content.",
            ),
            (
                "确认前必须完成本次音频文件核验",
                "确认前必须完成本次音频文件核验",
                "確認前必須完成本次音訊檔案核驗",
                "Complete audio file verification for this batch before confirming.",
            ),
            (
                "音频清单已变化，请重新检查并确认",
                "音频清单已变化，请重新检查并确认",
                "音訊清單已變更，請重新檢查並確認",
                "The audio manifest has changed. Check and confirm it again.",
            ),
            (
                "仍有缺失或不可用音频，不能确认本批",
                "仍有缺失或不可用音频，不能确认本批",
                "仍有缺失或無法使用的音訊，不能確認本批",
                "Some audio is still missing or unavailable. This batch cannot be confirmed.",
            ),
            (
                "音频确认状态发生变化，请重试",
                "音频确认状态发生变化，请重试",
                "音訊確認狀態發生變更，請重試",
                "The audio confirmation state has changed. Try again.",
            ),
            (
                "必须由当前发布人在本次事务中重新核验音频清单",
                "必须由当前发布人在本次事务中重新核验音频清单",
                "必須由目前發布人在本次交易中重新核驗音訊清單",
                "The current publisher must reverify the audio manifest in this transaction.",
            ),
            (
                "请先集中审听并确认当前版本的本批音频",
                "请先集中审听并确认当前版本的本批音频",
                "請先集中試聽並確認目前版本的本批音訊",
                "Review and confirm this batch of audio for the current version first.",
            ),
            (
                "发布版本与音频审听凭据不一致",
                "发布版本与音频审听凭据不一致",
                "發布版本與音訊試聽憑證不符",
                "The publication version does not match the audio review receipt.",
            ),
            (
                "entry lifecycle changed since it was loaded",
                "词条生命周期已变化，请重新加载后核对",
                "詞條生命週期已變更，請重新載入後核對",
                "The entry lifecycle has changed. Reload and review your changes.",
            ),
            (
                "configuration changed",
                "配置已变化，请重新加载后核对",
                "設定已變更，請重新載入後核對",
                "The configuration has changed. Reload and review your changes.",
            ),
            (
                "关联词已更新，请重新选择",
                "关联词已更新，请重新选择",
                "關聯詞已更新，請重新選擇",
                "The related word has changed. Select it again.",
            ),
            (
                "词条版本已更新，请重新生成候选",
                "词条版本已更新，请重新生成候选",
                "詞條版本已更新，請重新產生候選",
                "The entry version has changed. Generate the candidates again.",
            ),
            (
                "词性目录已更新，请刷新后重试",
                "词性目录已更新，请刷新后重试",
                "詞性目錄已更新，請重新整理後重試",
                "The part-of-speech catalogue has changed. Reload and try again.",
            ),
            (
                "例句或引用已变化，请刷新后重试",
                "例句或引用已变化，请刷新后重试",
                "例句或引用已變更，請重新整理後重試",
                "The sentence or its references have changed. Reload and try again.",
            ),
            (
                "已发布例句须先撤回，只有草稿可以恢复历史版本",
                "已发布例句须先撤回，只有草稿可以恢复历史版本",
                "已發布例句須先撤回，只有草稿可以恢復歷史版本",
                "Withdraw the published sentence first. Only drafts can be restored to a historical version.",
            ),
            (
                "例句版本已变化，请重新检查音频",
                "例句版本已变化，请重新检查音频",
                "例句版本已變更，請重新檢查音訊",
                "The sentence version has changed. Check the audio again.",
            ),
            (
                "内容版本已变化，请重新检查音频",
                "内容版本已变化，请重新检查音频",
                "內容版本已變更，請重新檢查音訊",
                "The content version has changed. Check the audio again.",
            ),
            (
                "permission catalog changed",
                "权限目录已变化，请重新加载",
                "權限目錄已變更，請重新載入",
                "The permission catalogue has changed. Reload it.",
            ),
            (
                "admin permissions changed",
                "管理员权限已变化，请重新加载",
                "管理員權限已變更，請重新載入",
                "Administrator permissions have changed. Reload them.",
            ),
            (
                "tag name already exists",
                "标签名称已存在",
                "標籤名稱已存在",
                "A tag with this name already exists.",
            ),
            (
                "tag changed",
                "标签已变化，请重新加载",
                "標籤已變更，請重新載入",
                "The tag has changed. Reload it.",
            ),
            (
                "entry changed since it was loaded",
                "词条版本已变化，请重新加载后核对",
                "詞條版本已變更，請重新載入後核對",
                "This entry has changed. Reload and review your changes.",
            ),
            (
                "认证状态已变化，请刷新后重试",
                "认证状态已变化，请刷新后重试",
                "認證狀態已變更，請重新整理後重試",
                "The certification status has changed. Reload and try again.",
            ),
        ],
        ErrorCode::WordListConflict => &[
            (
                "语音退款尚未完成，请稍后重试",
                "语音退款尚未完成，请稍后重试",
                "語音退款尚未完成，請稍後重試",
                "The audio refund is not complete. Try again later.",
            ),
            (
                "请先撤回词表再修改内容",
                "请先撤回词表再修改内容",
                "請先撤回詞表再修改內容",
                "Withdraw the word list before editing its content.",
            ),
            (
                "此词表包含自定义词条，请刷新页面后重新编辑",
                "此词表包含自定义词条，请刷新页面后重新编辑",
                "此詞表包含自訂詞條，請重新整理頁面後重新編輯",
                "This word list contains custom entries. Reload the page and edit it again.",
            ),
            (
                "请先撤回词表再生成语音",
                "请先撤回词表再生成语音",
                "請先撤回詞表再產生語音",
                "Withdraw the word list before generating audio.",
            ),
            (
                "此词表包含自定义词条，请刷新页面后重试",
                "此词表包含自定义词条，请刷新页面后重试",
                "此詞表包含自訂詞條，請重新整理頁面後重試",
                "This word list contains custom entries. Reload the page and try again.",
            ),
            (
                "词条曾不可用，作者修复并重新审核后才能投币",
                "词条曾不可用，作者修复并重新审核后才能投币",
                "詞條曾無法使用，作者修復並重新審核後才能投幣",
                "Entries were unavailable. You can tip after the author repairs the list and it passes review again.",
            ),
            (
                "目录已改变，请刷新后重新确认",
                "目录已改变，请刷新后重新确认",
                "目錄已變更，請重新整理後重新確認",
                "The library has changed. Reload and confirm again.",
            ),
            (
                "目标目录已有同名文件夹",
                "目标目录已有同名文件夹",
                "目標目錄已有同名資料夾",
                "The destination already contains a folder with this name.",
            ),
            (
                "请先撤回词表再修改名称",
                "请先撤回词表再修改名称",
                "請先撤回詞表再修改名稱",
                "Withdraw the word list before renaming it.",
            ),
            (
                "词表状态已改变，请刷新",
                "词表状态已改变，请刷新",
                "詞表狀態已變更，請重新整理",
                "The word list state has changed. Reload the page.",
            ),
            (
                "目录已变化，请刷新后重新选择",
                "目录已变化，请刷新后重新选择",
                "目錄已變更，請重新整理後重新選擇",
                "The library has changed. Reload and select again.",
            ),
            (
                "包含不可用来源，请逐份选择可用词表",
                "包含不可用来源，请逐份选择可用词表",
                "包含無法使用的來源，請逐份選擇可用詞表",
                "The selection contains unavailable sources. Select available word lists individually.",
            ),
            (
                "词表已改变，请重新加载",
                "词表已改变，请重新加载",
                "詞表已變更，請重新載入",
                "This word list has changed. Reload it.",
            ),
        ],
        ErrorCode::LearningSettingsRequired => &[(
            "请先完成学习设置",
            "请先完成学习设置",
            "請先完成學習設定",
            "Complete your learning settings first.",
        )],
        ErrorCode::LearningAlreadyAnswered => &[(
            "该题已记录首次作答，请恢复服务器进度",
            "该题已记录首次作答，请恢复服务器进度",
            "此題已記錄首次作答，請恢復伺服器進度",
            "Your first answer has already been recorded. Restore your progress from the server.",
        )],
        ErrorCode::ServiceUnavailable => &[
            (
                "OpenCode 连接暂时不可用，请稍后重试",
                "OpenCode 连接暂时不可用，请稍后重试",
                "OpenCode 連線暫時無法使用，請稍後重試",
                "The OpenCode connection is temporarily unavailable. Try again later.",
            ),
            (
                "模型服务暂时不可用，请稍后重试",
                "模型服务暂时不可用，请稍后重试",
                "模型服務暫時無法使用，請稍後重試",
                "The model service is temporarily unavailable. Try again later.",
            ),
            (
                "ChatGPT 账号服务暂时不可用，请刷新重试",
                "ChatGPT 账号服务暂时不可用，请刷新重试",
                "ChatGPT 帳號服務暫時無法使用，請重新整理後重試",
                "The ChatGPT account service is temporarily unavailable. Reload and try again.",
            ),
            (
                "AI 账本暂时不可用，请稍后重试",
                "AI 账本暂时不可用，请稍后重试",
                "AI 帳本暫時無法使用，請稍後重試",
                "The AI ledger is temporarily unavailable. Try again later.",
            ),
            (
                "request timed out; outcome may be unknown",
                "请求超时，操作结果尚未确认，请先查看结果",
                "請求逾時，操作結果尚未確認，請先查看結果",
                "The request timed out and its outcome may be unknown. Check the result before retrying.",
            ),
            (
                "database temporarily unavailable; outcome may be unknown",
                "服务暂不可用，操作结果尚未确认，请先查看结果",
                "服務暫時無法使用，操作結果尚未確認，請先查看結果",
                "The service is temporarily unavailable and the outcome may be unknown. Check the result before retrying.",
            ),
        ],
        ErrorCode::InternalError => &[(
            "internal error",
            "服务发生错误，请稍后重试",
            "服務發生錯誤，請稍後重試",
            "Something went wrong. Try again later.",
        )],
        ErrorCode::LearningTaskConflict => &[
            (
                "请更新客户端后使用严格单选任务",
                "请更新客户端后使用严格单选任务",
                "請更新用戶端後使用嚴格單選任務",
                "Update your client to use strict multiple-choice tasks.",
            ),
            (
                "请更新客户端后使用组合题型",
                "请更新客户端后使用组合题型",
                "請更新用戶端後使用組合題型",
                "Update your client to use combined question types.",
            ),
            (
                "此客户端不支持拼写回退，请更新后继续",
                "此客户端不支持拼写回退，请更新后继续",
                "此用戶端不支援拼寫回退，請更新後繼續",
                "This client does not support spelling fallback. Update it to continue.",
            ),
            (
                "此客户端不支持单选题，请更新后继续",
                "此客户端不支持单选题，请更新后继续",
                "此用戶端不支援單選題，請更新後繼續",
                "This client does not support multiple-choice questions. Update it to continue.",
            ),
            (
                "请更新客户端后继续组合题型练习",
                "请更新客户端后继续组合题型练习",
                "請更新用戶端後繼續組合題型練習",
                "Update your client to continue practising combined question types.",
            ),
            (
                "请更新客户端后管理严格单选任务",
                "请更新客户端后管理严格单选任务",
                "請更新用戶端後管理嚴格單選任務",
                "Update your client to manage strict multiple-choice tasks.",
            ),
            (
                "此客户端不支持当前练习题型，请更新后继续",
                "此客户端不支持当前练习题型，请更新后继续",
                "此用戶端不支援目前練習題型，請更新後繼續",
                "This client does not support the current practice question type. Update it to continue.",
            ),
            (
                "单选选项已失效，请重新检查来源后继续",
                "单选选项已失效，请重新检查来源后继续",
                "單選選項已失效，請重新檢查來源後繼續",
                "The multiple-choice options are no longer valid. Check the sources again before continuing.",
            ),
            (
                "当前词库没有足够可靠且不含已知歧义的干扰项，请检查来源后重试",
                "当前词库没有足够可靠且不含已知歧义的干扰项，请检查来源后重试",
                "目前詞庫沒有足夠可靠且不含已知歧義的干擾項，請檢查來源後重試",
                "The current lexicon does not have enough reliable distractors without known ambiguity. Check the sources and try again.",
            ),
            (
                "题目快照缺失",
                "题目快照缺失",
                "題目快照缺失",
                "The question snapshot is missing.",
            ),
            (
                "任务版本已变化",
                "任务版本已变化",
                "任務版本已變更",
                "The task version has changed.",
            ),
            (
                "请明确选择从最新轮次重新开始",
                "请明确选择从最新轮次重新开始",
                "請明確選擇從最新輪次重新開始",
                "Explicitly choose to restart from the latest run.",
            ),
            (
                "已有后继轮次，请恢复当前进度",
                "已有后继轮次，请恢复当前进度",
                "已有後續輪次，請恢復目前進度",
                "A subsequent run already exists. Restore your current progress.",
            ),
            (
                "前驱轮次不存在",
                "前驱轮次不存在",
                "找不到前一輪次",
                "The preceding run does not exist.",
            ),
            (
                "请更新客户端后使用按掌握情况练习",
                "请更新客户端后使用按掌握情况练习",
                "請更新用戶端後使用按掌握情況練習",
                "Update your client to practise by mastery.",
            ),
            (
                "练习缺少学习目标",
                "练习缺少学习目标",
                "練習缺少學習目標",
                "The practice has no learning targets.",
            ),
            (
                "请更新客户端后使用词表续练",
                "请更新客户端后使用词表续练",
                "請更新用戶端後使用詞表續練",
                "Update your client to continue word list practice.",
            ),
            (
                "题量、来源或学习设置已变化，请重新核对预览后创建",
                "题量、来源或学习设置已变化，请重新核对预览后创建",
                "題量、來源或學習設定已變更，請重新核對預覽後建立",
                "The question count, sources or learning settings have changed. Review the preview before creating the task.",
            ),
            (
                "学习日已变化，请核对日期后重试",
                "学习日已变化，请核对日期后重试",
                "學習日已變更，請核對日期後重試",
                "The learning day has changed. Check the date and try again.",
            ),
            (
                "任务功能已更新，请刷新页面后继续",
                "任务功能已更新，请刷新页面后继续",
                "任務功能已更新，請重新整理頁面後繼續",
                "Task features have been updated. Reload the page to continue.",
            ),
            (
                "此模式不支持整轮重建；掌握任务请重新确认来源",
                "此模式不支持整轮重建；掌握任务请重新确认来源",
                "此模式不支援整輪重建；掌握任務請重新確認來源",
                "This mode does not support rebuilding the entire run. Reconfirm the sources for mastery tasks.",
            ),
            (
                "任务或当前轮次已变化，请刷新",
                "任务或当前轮次已变化，请刷新",
                "任務或目前輪次已變更，請重新整理",
                "The task or current run has changed. Reload the page.",
            ),
            (
                "当前轮次不允许此操作",
                "当前轮次不允许此操作",
                "目前輪次不允許此操作",
                "This action is not allowed for the current run.",
            ),
            (
                "学习日已变化，请重新核对新轮计划",
                "学习日已变化，请重新核对新轮计划",
                "學習日已變更，請重新核對新輪計畫",
                "The learning day has changed. Review the plan for the new run again.",
            ),
            (
                "新轮内容或开始日期已变化，请重新核对",
                "新轮内容或开始日期已变化，请重新核对",
                "新輪內容或開始日期已變更，請重新核對",
                "The new run content or start date has changed. Review it again.",
            ),
            (
                "学习日已变化，请核对新轮计划",
                "学习日已变化，请核对新轮计划",
                "學習日已變更，請核對新輪計畫",
                "The learning day has changed. Review the plan for the new run.",
            ),
            (
                "已有新的练习批次，请刷新后恢复进度",
                "已有新的练习批次，请刷新后恢复进度",
                "已有新的練習批次，請重新整理後恢復進度",
                "A new practice batch exists. Reload and restore your progress.",
            ),
            (
                "本轮已结束，请主动开启下一轮",
                "本轮已结束，请主动开启下一轮",
                "本輪已結束，請主動開啟下一輪",
                "This run has ended. Start the next run when you are ready.",
            ),
            (
                "今日任务已完成，请下个学习日继续",
                "今日任务已完成，请下个学习日继续",
                "今日任務已完成，請下個學習日繼續",
                "Today's task is complete. Continue on the next learning day.",
            ),
            (
                "来源不可用，已保留本轮进度，请检查来源",
                "来源不可用，已保留本轮进度，请检查来源",
                "來源無法使用，已保留本輪進度，請檢查來源",
                "The sources are unavailable. Your progress for this run has been saved. Check the sources.",
            ),
            (
                "本次已无新词或到期复习，请按下次复习日期继续",
                "本次已无新词或到期复习，请按下次复习日期继续",
                "本次已無新詞或到期複習，請按下次複習日期繼續",
                "There are no new words or reviews due now. Continue on the next review date.",
            ),
            (
                "此任务不支持保留掌握进度的来源更新",
                "此任务不支持保留掌握进度的来源更新",
                "此任務不支援保留掌握進度的來源更新",
                "This task does not support updating sources while retaining mastery progress.",
            ),
            (
                "任务或学习内容已变化，请刷新后重新确认",
                "任务或学习内容已变化，请刷新后重新确认",
                "任務或學習內容已變更，請重新整理後重新確認",
                "The task or learning content has changed. Reload and confirm again.",
            ),
            (
                "请按顺序完成当前题目后继续重练",
                "请按顺序完成当前题目后继续重练",
                "請按順序完成目前題目後繼續重練",
                "Complete the current questions in order before practising them again.",
            ),
        ],
        ErrorCode::LearningRunUnavailable => &[
            (
                "题目来源已改变，请重新生成",
                "题目来源已改变，请重新生成",
                "題目來源已變更，請重新產生",
                "The question sources have changed. Generate the questions again.",
            ),
            (
                "判定版本不支持",
                "判定版本不支持",
                "不支援此判定版本",
                "This grading version is not supported.",
            ),
            (
                "来源已失效、任务已结束或轮次不可继续，请查看进度后明确重新开始",
                "来源已失效、任务已结束或轮次不可继续，请查看进度后明确重新开始",
                "來源已失效、任務已結束或輪次無法繼續，請查看進度後明確重新開始",
                "The sources are no longer valid, the task has ended or the run cannot continue. Check your progress, then explicitly restart.",
            ),
        ],
        ErrorCode::AudioStorageNotConfigured => &[(
            "正式音频存储暂不可用",
            "正式音频存储暂不可用",
            "正式音訊儲存暫時無法使用",
            "Published audio storage is temporarily unavailable.",
        )],
        ErrorCode::SpeechStorageUnavailable => &[
            (
                "语音存储暂不可用",
                "语音存储暂不可用",
                "語音儲存暫時無法使用",
                "Audio storage is temporarily unavailable.",
            ),
            (
                "语音服务暂不可用",
                "语音服务暂不可用",
                "語音服務暫時無法使用",
                "The speech service is temporarily unavailable.",
            ),
            (
                "测试语音存储暂不可用",
                "测试语音存储暂不可用",
                "測試語音儲存暫時無法使用",
                "Test audio storage is temporarily unavailable.",
            ),
            (
                "音频服务暂不可用",
                "音频服务暂不可用",
                "音訊服務暫時無法使用",
                "The audio service is temporarily unavailable.",
            ),
        ],
        ErrorCode::NotFound => &[
            (
                "身份证件不存在",
                "身份证件不存在",
                "找不到身分證件",
                "Identity document not found.",
            ),
            (
                "语音暂不可用",
                "语音暂不可用",
                "語音暫時無法使用",
                "Audio is temporarily unavailable.",
            ),
            (
                "manual operation not found",
                "手动操作记录不存在",
                "找不到手動操作記錄",
                "Manual operation not found.",
            ),
            (
                "该题正式音频暂不可用",
                "该题正式音频暂不可用",
                "此題正式音訊暫時無法使用",
                "Published audio for this question is temporarily unavailable.",
            ),
            ("任务不存在", "任务不存在", "找不到任務", "Task not found."),
            ("轮次不存在", "轮次不存在", "找不到輪次", "Run not found."),
            (
                "作答不存在",
                "作答不存在",
                "找不到作答",
                "Answer not found.",
            ),
            (
                "题目不存在",
                "题目不存在",
                "找不到題目",
                "Question not found.",
            ),
            (
                "wordlist not found",
                "词表不存在",
                "找不到詞表",
                "Word list not found.",
            ),
            (
                "review not found",
                "审核记录不存在",
                "找不到審核記錄",
                "Review not found.",
            ),
            (
                "tip not found",
                "投币记录不存在",
                "找不到投幣記錄",
                "Tip not found.",
            ),
            (
                "目录项目不存在",
                "目录项目不存在",
                "找不到目錄項目",
                "Library item not found.",
            ),
            (
                "deletion request not found",
                "注销申请不存在",
                "找不到註銷申請",
                "Account deletion request not found.",
            ),
        ],
        ErrorCode::IdempotencyConflict => &[
            (
                "请求键已用于不同内容",
                "请求键已用于不同内容",
                "請求鍵已用於不同內容",
                "The request key has already been used for different content.",
            ),
            (
                "提交请求键冲突",
                "提交请求键冲突",
                "提交請求鍵衝突",
                "The submission request key conflicts with an earlier request.",
            ),
            (
                "创建请求键已用于其他内容",
                "创建请求键已用于其他内容",
                "建立請求鍵已用於其他內容",
                "The creation request key has already been used for other content.",
            ),
            (
                "保存请求键已用于其他内容",
                "保存请求键已用于其他内容",
                "儲存請求鍵已用於其他內容",
                "The save request key has already been used for other content.",
            ),
            (
                "deletion intent changed",
                "注销意图已变化",
                "註銷意圖已變更",
                "The account deletion intent has changed.",
            ),
        ],
        ErrorCode::PasswordUnchanged => &[
            (
                "new password must differ from the current one",
                "新密码必须与当前密码不同",
                "新密碼必須與目前密碼不同",
                "The new password must differ from the current password.",
            ),
            (
                "password unchanged",
                "密码未变化",
                "密碼未變更",
                "The password is unchanged.",
            ),
        ],
        ErrorCode::PasswordHashUnavailable => &[(
            "password hash unavailable",
            "密码服务暂不可用，请稍后重试",
            "密碼服務暫時無法使用，請稍後重試",
            "The password service is temporarily unavailable. Try again later.",
        )],
        ErrorCode::InvalidIdentifier => &[
            (
                "exactly one of phone or email is required",
                "请提供手机号或邮箱中的一项",
                "請提供手機號碼或電郵地址其中一項",
                "Provide exactly one phone number or email address.",
            ),
            (
                "contact channel unavailable",
                "该联系渠道不可用",
                "此聯絡管道無法使用",
                "This contact channel is unavailable.",
            ),
            (
                "contact unchanged",
                "联系方式未变更",
                "聯絡方式未變更",
                "The contact is unchanged.",
            ),
            (
                "invalid identifier",
                "账号标识无效",
                "帳號識別資料無效",
                "Invalid account identifier.",
            ),
            (
                "identifier is invalid",
                "账号标识无效",
                "帳號識別資料無效",
                "Invalid account identifier.",
            ),
            (
                "provide exactly one phone or email",
                "请提供手机号或邮箱中的一项",
                "請提供手機號碼或電郵地址其中一項",
                "Provide exactly one phone number or email address.",
            ),
            (
                "phone or email is missing",
                "请输入手机号或邮箱",
                "請輸入手機號碼或電郵地址",
                "Enter a phone number or email address.",
            ),
            (
                "invalid subject",
                "账号信息无效",
                "帳號資料無效",
                "Invalid account details.",
            ),
        ],
        ErrorCode::UserAlreadyExists => &[
            (
                "contact already registered",
                "该联系方式已注册",
                "此聯絡方式已註冊",
                "This contact is already registered.",
            ),
            (
                "user already exists",
                "账号已存在",
                "帳號已存在",
                "This account already exists.",
            ),
        ],
        ErrorCode::UserNotFound => &[(
            "user not found",
            "用户不存在",
            "找不到用戶",
            "User not found.",
        )],
        ErrorCode::AdminNotFound => &[(
            "admin not found",
            "管理员不存在",
            "找不到管理員",
            "Administrator not found.",
        )],
        ErrorCode::AccountDeletionUpgradeRequired => &[(
            "use the account deletion request flow",
            "请使用注销申请流程",
            "請使用註銷申請流程",
            "Use the account deletion request flow.",
        )],
        ErrorCode::AccountDeletionChannelUnavailable => &[(
            "selected channel is unavailable",
            "所选联系渠道不可用",
            "所選聯絡管道無法使用",
            "The selected contact channel is unavailable.",
        )],
        ErrorCode::InvalidAccountDeletionCode => &[(
            "invalid account deletion code",
            "注销验证码无效",
            "註銷驗證碼無效",
            "Invalid account deletion verification code.",
        )],
        ErrorCode::AccountDeletionPending => &[(
            "account deletion is already pending",
            "账号已有待处理的注销申请",
            "帳號已有待處理的註銷申請",
            "An account deletion request is already pending.",
        )],
        ErrorCode::AccountDeletionConsentOutdated => &[(
            "reload the current consent",
            "请重新加载当前注销协议",
            "請重新載入目前的註銷協議",
            "Reload the current account deletion consent.",
        )],
        ErrorCode::AccountDeletionConsentRequired => &[(
            "active consent is required",
            "请同意当前注销协议",
            "請同意目前的註銷協議",
            "Accept the current account deletion consent.",
        )],
        ErrorCode::AccountDeletionBalanceChanged => &[
            (
                "reload balance and sign again",
                "余额已变化，请重新加载余额并再次签署",
                "餘額已變更，請重新載入餘額並再次簽署",
                "Your balance has changed. Reload the balance and sign again.",
            ),
            (
                "coin operation rejected",
                "余额已变化，请重新加载余额并再次签署",
                "餘額已變更，請重新載入餘額並再次簽署",
                "Your balance has changed. Reload the balance and sign again.",
            ),
        ],
        ErrorCode::AccountDeletionExpired => &[(
            "cancellation deadline passed",
            "已超过撤销期限",
            "已超過撤銷期限",
            "The cancellation deadline has passed.",
        )],
        ErrorCode::CoinInvalidAmount => &[(
            "invalid coin request",
            "币金额无效或超出范围",
            "幣金額無效或超出範圍",
            "The coin amount is invalid or out of range.",
        )],
        ErrorCode::CoinAccountUnavailable => &[(
            "coin operation rejected",
            "账号无法进行币操作",
            "帳號無法進行幣操作",
            "This account cannot perform coin operations.",
        )],
        ErrorCode::CoinWalletUnavailable => &[(
            "coin operation rejected",
            "钱包状态不允许此操作",
            "錢包狀態不允許此操作",
            "The wallet state does not allow this operation.",
        )],
        ErrorCode::CoinInsufficientBalance => &[(
            "coin operation rejected",
            "余额不足",
            "餘額不足",
            "Your balance is insufficient.",
        )],
        ErrorCode::CoinIdempotencyConflict => &[(
            "coin operation rejected",
            "币请求键已用于其他操作",
            "幣請求鍵已用於其他操作",
            "The coin request key has already been used for another operation.",
        )],
        ErrorCode::CoinSourceConflict => &[
            (
                "业务日已切换，请重试登录奖励",
                "业务日已切换，请重试登录奖励",
                "業務日已切換，請重試登入獎勵",
                "The business day has changed. Try claiming the sign-in reward again.",
            ),
            (
                "coin operation rejected",
                "币来源记录冲突",
                "幣來源記錄衝突",
                "The coin source record conflicts with an existing record.",
            ),
            (
                "event already recorded",
                "该事件已记录",
                "此事件已記錄",
                "This event has already been recorded.",
            ),
        ],
        ErrorCode::WordListTipLimit => &[(
            "这份词表累计最多投 2 枚",
            "这份词表累计最多投 2 枚",
            "這份詞表累計最多投 2 枚",
            "You can tip this word list a total of at most 2 coins.",
        )],
        ErrorCode::ValidationFailed => &[
            (
                "invalid refresh request id",
                "刷新请求标识无效",
                "更新請求識別碼無效",
                "Invalid refresh request identifier.",
            ),
            (
                "invalid current-only refresh request",
                "仅当前会话的刷新请求无效",
                "僅目前工作階段的更新請求無效",
                "Invalid request to refresh only the current session.",
            ),
            (
                "语音生成参数无效",
                "语音生成参数无效",
                "語音產生參數無效",
                "Invalid audio generation parameters.",
            ),
            (
                "只能为本词表的自定义词条生成语音",
                "只能为本词表的自定义词条生成语音",
                "只能為本詞表的自訂詞條產生語音",
                "Audio can only be generated for custom entries in this word list.",
            ),
            (
                "名称须为 1–100 字，词条须为 1–10000 个且不重复",
                "名称须为 1–100 字，词条须为 1–10000 个且不重复",
                "名稱須為 1–100 字，詞條須為 1–10000 個且不重複",
                "The name must contain 1–100 characters, with 1–10,000 distinct entries.",
            ),
            (
                "包含未发布、归档或不支持的词条",
                "包含未发布、归档或不支持的词条",
                "包含未發布、封存或不支援的詞條",
                "The list contains unpublished, archived or unsupported entries.",
            ),
            (
                "注不能超过 1000 字",
                "注不能超过 1000 字",
                "註不能超過 1000 字",
                "A note must not exceed 1,000 characters.",
            ),
            (
                "请选择平台词条，或填写自定义词条（1–50 字）及释义（1–200 字）",
                "请选择平台词条，或填写自定义词条（1–50 字）及释义（1–200 字）",
                "請選擇平台詞條，或填寫自訂詞條（1–50 字）及釋義（1–200 字）",
                "Select a platform entry, or provide a custom entry (1–50 characters) and definition (1–200 characters).",
            ),
            (
                "词条来源不能直接修改，请删除后重新添加",
                "词条来源不能直接修改，请删除后重新添加",
                "詞條來源不能直接修改，請刪除後重新新增",
                "You cannot change an entry's source directly. Remove the entry and add it again.",
            ),
            (
                "自定义词缺少稳定标识",
                "自定义词缺少稳定标识",
                "自訂詞缺少穩定識別碼",
                "A custom entry is missing its stable identifier.",
            ),
            (
                "备注不能超过 1000 字，平台词条标识须一致",
                "备注不能超过 1000 字，平台词条标识须一致",
                "備註不能超過 1000 字，平台詞條識別碼須一致",
                "Notes must not exceed 1,000 characters, and platform entry identifiers must match.",
            ),
            (
                "生成语音的保存请求缺少请求标识",
                "生成语音的保存请求缺少请求标识",
                "產生語音的儲存請求缺少請求識別碼",
                "The save request for generated audio is missing its request identifier.",
            ),
            (
                "invalid note updates",
                "备注更新无效",
                "備註更新無效",
                "Invalid note updates.",
            ),
            (
                "items 不可与旧版选词字段混用",
                "items 不可与旧版选词字段混用",
                "items 不可與舊版選詞欄位混用",
                "Do not combine items with legacy entry-selection fields.",
            ),
            (
                "作者注释须与词条顺序一致，且每条不超过 1000 字",
                "作者注释须与词条顺序一致，且每条不超过 1000 字",
                "作者註釋須與詞條順序一致，且每條不超過 1000 字",
                "Author notes must match the entry order, with no more than 1,000 characters per note.",
            ),
            (
                "原因须为 1–1000 字",
                "原因须为 1–1000 字",
                "原因須為 1–1000 字",
                "The reason must contain 1–1,000 characters.",
            ),
            (
                "创建时间范围无效",
                "创建时间范围无效",
                "建立時間範圍無效",
                "Invalid creation time range.",
            ),
            (
                "名称须为 1–100 字",
                "名称须为 1–100 字",
                "名稱須為 1–100 字",
                "The name must contain 1–100 characters.",
            ),
            (
                "目标必须是正常文件夹",
                "目标必须是正常文件夹",
                "目標必須是正常資料夾",
                "The destination must be an active folder.",
            ),
            (
                "不能将文件夹移动到自身或子文件夹",
                "不能将文件夹移动到自身或子文件夹",
                "不能將資料夾移動到自身或子資料夾",
                "You cannot move a folder into itself or its descendants.",
            ),
            (
                "文件夹最多 20 层",
                "文件夹最多 20 层",
                "資料夾最多 20 層",
                "Folders must not exceed 20 levels.",
            ),
            (
                "分页或搜索参数无效",
                "分页或搜索参数无效",
                "分頁或搜尋參數無效",
                "Invalid pagination or search parameters.",
            ),
            (
                "仅永久删除可清空回收站",
                "仅永久删除可清空回收站",
                "僅永久刪除可清空回收站",
                "Only permanent deletion can empty the recycle bin.",
            ),
            (
                "请选择 1–100 个不同项目",
                "请选择 1–100 个不同项目",
                "請選擇 1–100 個不同項目",
                "Select 1–100 distinct items.",
            ),
            (
                "不能移入自身或子文件夹",
                "不能移入自身或子文件夹",
                "不能移入自身或子資料夾",
                "You cannot move an item into itself or its descendants.",
            ),
            (
                "请选择目录项目",
                "请选择目录项目",
                "請選擇目錄項目",
                "Select library items.",
            ),
            (
                "回收站内容不能用于学习任务",
                "回收站内容不能用于学习任务",
                "回收站內容不能用於學習任務",
                "Items in the recycle bin cannot be used for learning tasks.",
            ),
            (
                "所选文件夹没有词表，请调整选择",
                "所选文件夹没有词表，请调整选择",
                "所選資料夾沒有詞表，請調整選擇",
                "The selected folders contain no word lists. Adjust your selection.",
            ),
            (
                "展开后超过 5 份词表，请减少选择",
                "展开后超过 5 份词表，请减少选择",
                "展開後超過 5 份詞表，請減少選擇",
                "The selection expands to more than 5 word lists. Select fewer items.",
            ),
        ],
        ErrorCode::SpeechProviderUnavailable => &[(
            "语音生成暂不可用",
            "语音生成暂不可用",
            "語音產生暫時無法使用",
            "Audio generation is temporarily unavailable.",
        )],
        ErrorCode::SpeechVoiceNotFound => &[(
            "默认发音人暂不可用",
            "默认发音人暂不可用",
            "預設發音人暫時無法使用",
            "The default voice is temporarily unavailable.",
        )],
        ErrorCode::WordListAudioCostChanged => &[(
            "语音费用已变化，请刷新后确认费用",
            "语音费用已变化，请刷新后确认费用",
            "語音費用已變更，請重新整理後確認費用",
            "The audio cost has changed. Reload and confirm the cost.",
        )],
        ErrorCode::AccountDeletionUnsettledAudio => &[(
            "请等待语音生成或退款完成后再申请注销",
            "请等待语音生成或退款完成后再申请注销",
            "請等待語音產生或退款完成後再申請註銷",
            "Wait for audio generation or refunds to complete before requesting account deletion.",
        )],
        ErrorCode::CoinReversalInvalid => &[(
            "only an original manual credit can be reversed",
            "仅原始手动入账可冲正",
            "僅原始手動入帳可沖正",
            "Only an original manual credit can be reversed.",
        )],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    #[test]
    fn common_error_details_keep_the_reason_and_recovery_action() {
        for (code, source, hant, english) in [
            (
                ErrorCode::LearningTaskConflict,
                "此客户端不支持拼写回退，请更新后继续",
                "此用戶端不支援拼寫回退，請更新後繼續",
                "This client does not support spelling fallback. Update it to continue.",
            ),
            (
                ErrorCode::LearningTaskConflict,
                "单选选项已失效，请重新检查来源后继续",
                "單選選項已失效，請重新檢查來源後繼續",
                "The multiple-choice options are no longer valid. Check the sources again before continuing.",
            ),
            (
                ErrorCode::InvalidRequestBody,
                "当前没有可重新确认的词义，请先恢复来源或补足可靠干扰项",
                "目前沒有可重新確認的詞義，請先恢復來源或補足可靠干擾項",
                "There are no senses available to reconfirm. Restore the sources or add reliable distractors first.",
            ),
            (
                ErrorCode::LearningTaskConflict,
                "任务版本已变化",
                "任務版本已變更",
                "The task version has changed.",
            ),
            (
                ErrorCode::WordListConflict,
                "请先撤回词表再修改内容",
                "請先撤回詞表再修改內容",
                "Withdraw the word list before editing its content.",
            ),
            (
                ErrorCode::InvalidIdentifier,
                "contact unchanged",
                "聯絡方式未變更",
                "The contact is unchanged.",
            ),
            (
                ErrorCode::CoinInsufficientBalance,
                "coin operation rejected",
                "餘額不足",
                "Your balance is insufficient.",
            ),
        ] {
            assert!(detail(code, source, Locale::ZhHans).is_some());
            assert_eq!(detail(code, source, Locale::ZhHant), Some(hant));
            for locale in [Locale::EnGb, Locale::EnUs] {
                assert_eq!(detail(code, source, locale), Some(english));
            }
        }
    }

    #[test]
    fn insufficient_questions_keeps_counts_and_preview_guidance() {
        let revision = DetailTemplate::PositiveRevision {
            field: "base_revision",
        };
        assert_eq!(
            revision.render(Locale::ZhHant),
            "base_revision 必須至少為 1"
        );
        assert_eq!(
            revision.render(Locale::EnUs),
            "base_revision must be at least 1"
        );
        let voice = DetailTemplate::InvalidVoiceField { field: "locale" };
        assert_eq!(voice.render(Locale::ZhHant), "音色欄位 locale 無效");
        assert_eq!(
            voice.render(Locale::EnUs),
            "voice field 'locale' is invalid"
        );
        let source = "仅有 3 道可出题内容，需要 20 道；请查看预览排除原因";
        assert_eq!(
            detail(
                ErrorCode::LearningInsufficientQuestions,
                source,
                Locale::EnUs
            ),
            None
        );
        let template = DetailTemplate::LearningInsufficientQuestions {
            available: 3,
            required: 20,
        };
        assert_eq!(template.render(Locale::ZhHans), source);
        assert_eq!(
            template.render(Locale::ZhHant),
            "僅有 3 道可出題內容，需要 20 道；請查看預覽排除原因"
        );
        for locale in [Locale::EnGb, Locale::EnUs] {
            assert_eq!(
                template.render(locale),
                "Only 3 questions are available; 20 are required. Check the preview for exclusion reasons."
            );
        }
    }

    #[test]
    fn field_issue_translation_preserves_location_and_unregistered_text() {
        use crate::lexicon::dto::{
            PersistedWordStep, V3DraftNodeLocation, V3DraftValidationIssue, V3ValidationIssueCode,
        };
        let node_id = uuid::Uuid::new_v4();
        let mut issue = V3DraftValidationIssue {
            schema_version: 3,
            step: PersistedWordStep::Meanings,
            node_id,
            field: "sense_groups".into(),
            code: V3ValidationIssueCode::SenseGroupRequired,
            message: "至少需要一个语义区间".into(),
            node_location: V3DraftNodeLocation {
                content_id: None,
                content_slot_id: None,
                node_role: "entry".into(),
                ancestor_node_ids: vec![],
                pos_id: None,
                form_group_id: None,
                membership_id: None,
                form_id: None,
                variant_id: None,
                pronunciation_id: None,
                form_type: None,
                dialect: None,
            },
        };
        localize_issue(&mut issue, Locale::EnUs);
        assert_eq!(issue.message, "At least one sense group is required.");
        assert_eq!(issue.node_id, node_id);
        assert_eq!(issue.field, "sense_groups");
        issue.code = V3ValidationIssueCode::PosRequired;
        issue.message = "a complete entry requires at least one part of speech".into();
        localize_issue(&mut issue, Locale::ZhHant);
        assert_eq!(issue.message, "完整詞條至少需要一個詞性");
        issue.code = V3ValidationIssueCode::AudioAssetInvalid;
        let limit = crate::lexicon::service::v3::MAX_VARIANT_AUDIO_ASSETS;
        issue.message =
            format!("a text or pronunciation variant may reference at most {limit} audio assets");
        localize_issue(&mut issue, Locale::ZhHans);
        assert_eq!(
            issue.message,
            format!("文本或发音变体最多引用 {limit} 条音频")
        );
        issue.code = V3ValidationIssueCode::PhraseComponentTargetUnavailable;
        issue.message = "phrase component target is missing, archived or not a V3 word".into();
        let mut response_issue = issue.clone();
        localize_issue(&mut response_issue, Locale::ZhHant);
        assert_eq!(
            response_issue.message,
            "片語成分目標不存在、已封存或不是 V3 單字"
        );
        let mut problem = ProblemDetails {
            type_uri: ErrorCode::ValidationFailed.type_uri(),
            title: "Validation failed",
            status: 422,
            detail: "V3 draft validation failed".into(),
            code: ErrorCode::ValidationFailed,
            field: None,
            field_issues: Some(vec![issue.clone()]),
            meta: None,
        };
        localize(&mut problem, Locale::ZhHans);
        let failed_issue = &problem.field_issues.unwrap()[0];
        assert_eq!(
            failed_issue.message,
            "短语成分目标不存在、已归档或不是 V3 单词"
        );
        assert_eq!(failed_issue.node_id, response_issue.node_id);
        issue.message = "供应商原文".into();
        localize_issue(&mut issue, Locale::EnUs);
        assert_eq!(issue.message, "供应商原文");
    }

    #[test]
    fn extra_details_require_exact_code_and_source() {
        for source in [
            crate::lexicon::sentence_formatting::UPGRADE_MESSAGE,
            crate::lexicon::voice_versions::UPGRADE_MESSAGE,
        ] {
            assert!(
                detail(ErrorCode::InvalidRequestBody, source, Locale::EnUs)
                    .unwrap()
                    .contains("Preserve your local input")
            );
        }
        assert_eq!(
            detail(ErrorCode::NotFound, "申请不存在", Locale::EnUs),
            Some("Application not found.")
        );
        assert_eq!(
            detail(ErrorCode::NotFound, "user not found", Locale::ZhHans),
            Some("用户不存在")
        );
        let source = "音频存储暂不可用，请使用相同请求重试";
        assert_eq!(
            detail(ErrorCode::SpeechStorageUnavailable, source, Locale::EnUs),
            Some("Audio storage is temporarily unavailable. Retry the same request.")
        );
        assert_eq!(
            detail(ErrorCode::SpeechStorageUnavailable, source, Locale::ZhHant),
            Some("音訊儲存暫時無法使用，請使用相同請求重試")
        );
        assert_eq!(detail(ErrorCode::Forbidden, source, Locale::EnUs), None);
        assert_eq!(
            detail(
                ErrorCode::SpeechStorageUnavailable,
                "供应商原文",
                Locale::EnUs
            ),
            None
        );
    }

    #[test]
    fn composed_plan_reasons_preserve_order_unknown_text_and_integer_precision() {
        let template = DetailTemplate::LearningPlanReasons(vec![
            "词表中没有可出题内容，请检查中文释义和英文词面".into(),
            "日期计划容量不足，请调整日期或新建任务".into(),
            "<img src=x> {{available}}".into(),
        ]);
        assert_eq!(
            template.render(Locale::EnUs),
            "The word lists have no content suitable for questions. Check the Chinese definitions and English headwords. The schedule has insufficient capacity. Adjust the dates or create a new task. <img src=x> {{available}}"
        );
        assert_eq!(
            template.render(Locale::ZhHant),
            "詞表中沒有可出題內容，請檢查中文釋義和英文詞面；日期計畫容量不足，請調整日期或新建任務；<img src=x> {{available}}"
        );
        assert_eq!(
            DetailTemplate::LearningInsufficientQuestions {
                available: 1,
                required: 1
            }
            .render(Locale::EnUs),
            "Only 1 question is available; 1 is required. Check the preview for exclusion reasons."
        );
        let count = DetailTemplate::LearningInsufficientQuestions {
            available: 9_007_199_254_740_993,
            required: i32::MAX,
        };
        assert_eq!(
            count.render(Locale::EnGb),
            "Only 9007199254740993 questions are available; 2147483647 are required. Check the preview for exclusion reasons."
        );
    }

    #[test]
    fn all_registered_code_titles_are_nonempty_and_english_titles_are_human_readable() {
        for code in ErrorCode::ALL {
            let mut sources = std::collections::HashSet::new();
            for (source, zh, hant, en) in details_for(code) {
                assert!(sources.insert(source), "duplicate source for {code:?}");
                for value in [source, zh, hant, en] {
                    assert!(!value.trim().is_empty(), "empty detail for {code:?}");
                }
            }
            for locale in [Locale::ZhHans, Locale::ZhHant, Locale::EnGb, Locale::EnUs] {
                let value = title(code, locale);
                assert!(!value.trim().is_empty(), "{code:?}/{locale:?}");
                assert!(
                    !value.contains('_'),
                    "machine identifier used as title: {code:?}"
                );
            }
        }
    }

    #[test]
    fn only_registered_details_change_and_machine_context_is_preserved() {
        let mut problem = ProblemDetails {
            type_uri: ErrorCode::RevisionConflict.type_uri(),
            title: "Revision conflict",
            status: 409,
            detail: "entry changed since it was loaded".into(),
            code: ErrorCode::RevisionConflict,
            field: Some("revision"),
            field_issues: None,
            meta: Some(crate::error::ProblemMeta {
                current_revision: Some(17),
                ..Default::default()
            }),
        };
        let mut before = serde_json::to_value(&problem).unwrap();
        localize(&mut problem, Locale::ZhHant);
        let mut after = serde_json::to_value(&problem).unwrap();
        assert_ne!(before["detail"], after["detail"]);
        for key in ["title", "detail"] {
            before.as_object_mut().unwrap().remove(key);
            after.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(before, after);
        for unknown in [
            "原始业务正文",
            "vendor: invalid token; id=42",
            "invalid token",
            "{{parameter}} <script>",
        ] {
            problem.detail = unknown.to_owned();
            localize(&mut problem, Locale::EnUs);
            assert_eq!(
                problem.detail, unknown,
                "unregistered code/detail pair must stay verbatim"
            );
        }
    }

    #[tokio::test]
    async fn internal_causes_do_not_become_public_details() {
        let response = AppError::internal(anyhow::anyhow!("password=private; postgres://private"))
            .into_response();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["detail"], "internal error");
        assert!(!String::from_utf8_lossy(&bytes).contains("private"));
    }
}
