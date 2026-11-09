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
    LearningInsufficientQuestions { available: usize, required: i32 },
    LearningPlanReasons(Vec<String>),
}

impl DetailTemplate {
    pub(crate) fn render(&self, locale: Locale) -> String {
        match self {
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
}

fn detail(code: ErrorCode, source: &str, locale: Locale) -> Option<&'static str> {
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
                "判定版本不支持",
                "判定版本不支持",
                "不支援此判定版本",
                "This grading version is not supported.",
            ),
            (
                "来源已失效、任务已结束或轮次不可继续，请查看进度后明确重新开始",
                "来源已失效、任务已结束或轮次不可继续，请查看进度后明确重新开始",
                "來源已失效、任務已結束或輪次無法繼續，請查看進度後明確重新開始",
                "The sources have expired, the task has ended or the run cannot continue. Check your progress, then explicitly restart.",
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
