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
        ErrorCode::InvalidPathParameter => &[(
            "invalid path parameter",
            "路径参数无效",
            "路徑參數無效",
            "Invalid path parameter.",
        )],
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
        ErrorCode::InvalidPhone => &[(
            "invalid phone",
            "手机号无效",
            "手機號碼無效",
            "Invalid phone number.",
        )],
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
        ErrorCode::WordListConflict => &[(
            "词表已改变，请重新加载",
            "词表已改变，请重新加载",
            "詞表已變更，請重新載入",
            "This word list has changed. Reload it.",
        )],
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
