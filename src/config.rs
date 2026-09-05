use serde::Deserialize;
use std::num::{NonZeroU8, NonZeroU16};

use crate::lexicon::content_completion::LexiconGeneratorConfig;
use crate::platform::storage::ObjectStorageConfig;
use crate::speech::AzureSpeechConfig;

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmartLexiconV3Flags {
    #[serde(
        default,
        rename = "smart_lexicon_v3_read",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub read: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_create",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub create: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_edit",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub edit: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_publish",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub publish: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_projection",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub projection: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_legacy_bridge_read",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub legacy_bridge_read: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_sentence_associations",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub sentence_associations: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_sentence_target_discovery",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub sentence_target_discovery: bool,
    #[serde(
        default,
        rename = "smart_lexicon_v3_draft_relation_prebinding",
        deserialize_with = "deserialize_explicit_bool"
    )]
    pub draft_relation_prebinding: bool,
}

fn deserialize_explicit_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    value
        .parse()
        .map_err(|_| serde::de::Error::custom("expected true or false"))
}

impl SmartLexiconV3Flags {
    #[doc(hidden)]
    pub const fn all_enabled() -> Self {
        Self {
            read: true,
            create: true,
            edit: true,
            publish: true,
            projection: true,
            legacy_bridge_read: true,
            sentence_associations: true,
            sentence_target_discovery: true,
            draft_relation_prebinding: true,
        }
    }

    #[doc(hidden)]
    pub const fn all_disabled() -> Self {
        Self {
            read: false,
            create: false,
            edit: false,
            publish: false,
            projection: false,
            legacy_bridge_read: false,
            sentence_associations: false,
            sentence_target_discovery: false,
            draft_relation_prebinding: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: NonZeroU16,
    pub database_url: String,
    pub jwt_secret: String,
    #[serde(default = "default_refresh_ttl_days")]
    pub refresh_ttl_days: u64,
    #[serde(default = "default_access_ttl_minutes")]
    pub access_ttl_minutes: u64,
    #[serde(default = "default_otp_ttl_minutes")]
    pub otp_ttl_minutes: u64,
    #[serde(default = "default_otp_cooldown_seconds")]
    pub otp_cooldown_seconds: u64,
    #[serde(default = "default_otp_daily_limit")]
    pub otp_daily_limit: u64,
    // NonZeroU8：类型即约束——envy 解析期即拒 0（"错一次即毁"）/负数/越界，
    // 传给 store 的 max_attempts:u8 无需 `as` 截断（对齐 port: NonZeroU16 的做法）。
    #[serde(default = "default_otp_max_attempts")]
    pub otp_max_attempts: NonZeroU8,
    pub redis_url: String,
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,
    pub admin_jwt_secret: String,
    #[serde(default = "default_admin_access_ttl_minutes")]
    pub admin_access_ttl_minutes: u64,
    #[serde(default = "default_admin_refresh_ttl_days")]
    pub admin_refresh_ttl_days: u64,
    #[serde(flatten)]
    pub smart_lexicon_v3_flags: SmartLexiconV3Flags,
    #[serde(skip)]
    pub object_storage: ObjectStorageConfig,
    #[serde(skip)]
    pub azure_speech: Option<AzureSpeechConfig>,
    #[serde(skip)]
    pub lexicon_generator: Option<LexiconGeneratorConfig>,
}

fn default_refresh_ttl_days() -> u64 {
    30
}

fn default_access_ttl_minutes() -> u64 {
    15
}

fn default_otp_ttl_minutes() -> u64 {
    10
}
fn default_otp_cooldown_seconds() -> u64 {
    60
}
fn default_otp_daily_limit() -> u64 {
    10
}
fn default_otp_max_attempts() -> NonZeroU8 {
    NonZeroU8::new(5).unwrap()
}
fn default_port() -> NonZeroU16 {
    NonZeroU16::new(8383).unwrap()
}

fn default_cookie_secure() -> bool {
    true // 生产环境配置成true
}
fn default_admin_access_ttl_minutes() -> u64 {
    15
}
fn default_admin_refresh_ttl_days() -> u64 {
    7
}

impl Config {
    /// 从任意键值对解析配置——纯函数，不触碰进程环境，因此可单元测试。
    /// 生产的 [`load_config`] 与测试都走这条路径，保证被测逻辑即生产逻辑。
    pub fn from_pairs<I>(pairs: I) -> Result<Self, envy::Error>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let pairs = pairs.into_iter().collect::<Vec<_>>();
        let object_storage = ObjectStorageConfig::from_pairs(pairs.iter().cloned())
            .map_err(|error| envy::Error::Custom(error.to_string()))?;
        let azure_speech = AzureSpeechConfig::from_pairs(pairs.iter().cloned())
            .map_err(|error| envy::Error::Custom(error.to_string()))?;
        let lexicon_generator = LexiconGeneratorConfig::from_pairs(pairs.iter().cloned())
            .map_err(envy::Error::Custom)?;
        let mut cfg: Self = envy::from_iter(pairs)?;
        cfg.object_storage = object_storage;
        cfg.azure_speech = azure_speech;
        cfg.lexicon_generator = lexicon_generator;
        // 两把密钥相同 = per-realm 隔离塌一半,启动即失败(admin-design.md §13)
        if cfg.admin_jwt_secret == cfg.jwt_secret {
            return Err(envy::Error::Custom(
                "ADMIN_JWT_SECRET must differ from JWT_SECRET".into(),
            ));
        }
        Ok(cfg)
    }
}

/// 生产入口：加载 `.env`（存在则读），再从进程环境解析配置。
pub fn load_config() -> Result<Config, envy::Error> {
    dotenvy::from_filename(".env").ok();
    Config::from_pairs(std::env::vars())
}

#[cfg(test)]
mod tests {
    use super::*;

    // —— 测试脚手架（对标 testing-guide 维度四：数据集中构造）——

    // 一份「必填项齐全」的合法基线，测试只声明与它的差异（Object Mother）。
    fn valid_baseline() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DATABASE_URL", "postgres://localhost/tsz"),
            ("JWT_SECRET", "s3cret"),
            // Redis 是硬依赖（见 docs/redis-design.md §4），与 DATABASE_URL 同级必填，
            // 故基线里备齐，其余测试才能只声明「与基线的差异」。
            ("REDIS_URL", "redis://localhost:6379/0"),
            // admin realm 第二把签名密钥（admin-design.md §4）：与 JWT_SECRET 同级必填——
            // 双防线 = per-realm secret + aud 校验，密钥不分离则防线塌一半。
            ("ADMIN_JWT_SECRET", "adm1n-s3cret"),
        ]
    }

    // 走生产接缝 Config::from_pairs —— 与 load_config 相同的解析路径，
    // 但喂入键值对、不触碰进程全局环境变量（testing-guide 铁律 7：禁 set_var）。
    fn parse(pairs: &[(&str, &str)]) -> Result<Config, envy::Error> {
        Config::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn smart_lexicon_v3_flag_values(flags: SmartLexiconV3Flags) -> [bool; 6] {
        [
            flags.read,
            flags.create,
            flags.edit,
            flags.publish,
            flags.projection,
            flags.legacy_bridge_read,
        ]
    }

    #[test]
    fn port_falls_back_to_default_when_absent() {
        // Arrange：只给必填项
        let input = valid_baseline();
        // Act
        let cfg = parse(&input).expect("必填项齐全应能解析");
        // Assert
        assert_eq!(cfg.port.get(), 8383, "省略 PORT 时应回落到默认端口 8383");
    }

    #[test]
    fn explicit_port_overrides_default() {
        let mut input = valid_baseline();
        input.push(("PORT", "9000"));

        let cfg = parse(&input).expect("字段齐全应能解析");

        assert_eq!(cfg.port.get(), 9000, "显式 PORT 应覆盖默认值");
    }

    #[test]
    fn all_fields_parsed_when_present() {
        let mut input = valid_baseline();
        input.push(("PORT", "9000"));

        let cfg = parse(&input).expect("字段齐全应能解析");

        assert_eq!(cfg.port.get(), 9000);
        assert_eq!(cfg.database_url, "postgres://localhost/tsz");
        assert_eq!(cfg.jwt_secret, "s3cret");
        assert_eq!(cfg.redis_url, "redis://localhost:6379/0");
        assert_eq!(cfg.admin_jwt_secret, "adm1n-s3cret");
    }

    #[test]
    fn qwen_lexicon_generator_is_attached_only_with_explicit_complete_configuration() {
        let mut input = valid_baseline();
        input.extend([
            ("LEXICON_GENERATOR_PROVIDER", "qwen"),
            ("QWEN_LEXICON_API_KEY", "secret"),
            ("QWEN_LEXICON_MODEL", "qwen3.8-max"),
        ]);

        let cfg = parse(&input).expect("完整千问配置应接入应用配置");

        assert!(matches!(
            cfg.lexicon_generator,
            Some(LexiconGeneratorConfig::Qwen(_))
        ));
    }

    #[test]
    fn qwen_lexicon_generator_rejects_missing_selected_configuration() {
        let mut input = valid_baseline();
        input.push(("LEXICON_GENERATOR_PROVIDER", "qwen"));

        let error = parse(&input).expect_err("选择千问但没有凭据和模型应启动失败");

        assert!(error.to_string().contains("qwen lexicon generator"));
    }

    #[test]
    fn non_numeric_port_errors() {
        // port 是 NonZeroU16：非数字的值应在解析阶段就被 envy 挡下（类型即约束）。
        let mut input = valid_baseline();
        input.push(("PORT", "not-a-number"));

        assert!(parse(&input).is_err(), "非数字端口应解析失败");
    }

    #[test]
    fn out_of_range_port_errors() {
        // 超出 u16 上限（65535）的值同样应被拒绝。
        let mut input = valid_baseline();
        input.push(("PORT", "70000"));

        assert!(parse(&input).is_err(), "越界端口应解析失败");
    }

    #[test]
    fn zero_port_errors() {
        // NonZeroU16 相比 u16 的增量约束：端口 0 非法（bind 语义里表示随机端口），应被拒。
        let mut input = valid_baseline();
        input.push(("PORT", "0"));

        assert!(parse(&input).is_err(), "端口 0 应解析失败");
    }

    #[test]
    fn missing_database_url_errors() {
        let input = [("JWT_SECRET", "s3cret")];
        assert!(parse(&input).is_err(), "缺少必填的 DATABASE_URL 应返回错误");
    }

    #[test]
    fn missing_jwt_secret_errors() {
        let input = [("DATABASE_URL", "postgres://localhost/tsz")];
        assert!(parse(&input).is_err(), "缺少必填的 JWT_SECRET 应返回错误");
    }

    #[test]
    fn missing_redis_url_errors() {
        // REDIS_URL 无 #[serde(default)]，与 DATABASE_URL 同为硬依赖：缺了必须解析失败，
        // 让「Redis 连接串没配」在启动即暴露，而不是运行到发验证码时才炸。
        let input = [
            ("DATABASE_URL", "postgres://localhost/tsz"),
            ("JWT_SECRET", "s3cret"),
        ];
        assert!(parse(&input).is_err(), "缺少必填的 REDIS_URL 应返回错误");
    }

    #[test]
    fn missing_admin_jwt_secret_errors() {
        // ADMIN_JWT_SECRET 无 default，必填（admin-design.md §4）：缺了必须启动即失败，
        // 而不是 admin 登录时才发现签不出 token。⚠️ 部署面：上 admin 版前生产 .env 先加此项。
        let input = [
            ("DATABASE_URL", "postgres://localhost/tsz"),
            ("JWT_SECRET", "s3cret"),
            ("REDIS_URL", "redis://localhost:6379/0"),
        ];
        assert!(
            parse(&input).is_err(),
            "缺少必填的 ADMIN_JWT_SECRET 应返回错误"
        );
    }

    #[test]
    fn admin_jwt_secret_must_differ_from_jwt_secret() {
        // 加固（admin-design.md §4，用户拍板 2026-07-19）：两把密钥相同 = per-realm 隔离
        // 塌掉一半（只剩 aud 校验一道墙），而「复制粘贴同一串」恰是最易犯的部署失误——
        // 解析后校验，相同则启动即失败。
        let input = [
            ("DATABASE_URL", "postgres://localhost/tsz"),
            ("JWT_SECRET", "same-secret"),
            ("REDIS_URL", "redis://localhost:6379/0"),
            ("ADMIN_JWT_SECRET", "same-secret"),
        ];
        assert!(
            parse(&input).is_err(),
            "ADMIN_JWT_SECRET 与 JWT_SECRET 相同应解析失败（密钥必须分离）"
        );
    }

    #[test]
    fn admin_ttls_fall_back_to_defaults() {
        // admin 双 TTL 默认值（admin-design.md §13）：access 15 分钟；
        // refresh 7 天——注意语义是绝对上限非滑动（Q8），轮换不续期。
        let input = valid_baseline();
        let cfg = parse(&input).expect("必填项齐全应能解析");
        assert_eq!(
            cfg.admin_access_ttl_minutes, 15,
            "省略时 admin access TTL 默认 15 分钟"
        );
        assert_eq!(
            cfg.admin_refresh_ttl_days, 7,
            "省略时 admin refresh TTL 默认 7 天（绝对上限）"
        );
    }

    #[test]
    fn explicit_admin_ttls_override_defaults() {
        let mut input = valid_baseline();
        input.push(("ADMIN_ACCESS_TTL_MINUTES", "30"));
        input.push(("ADMIN_REFRESH_TTL_DAYS", "14"));

        let cfg = parse(&input).expect("字段齐全应能解析");

        assert_eq!(cfg.admin_access_ttl_minutes, 30, "显式值应覆盖默认");
        assert_eq!(cfg.admin_refresh_ttl_days, 14, "显式值应覆盖默认");
    }

    #[test]
    fn object_storage_is_disabled_when_no_space_is_declared() {
        let cfg = parse(&valid_baseline()).expect("核心必填项齐全应能解析");

        assert!(cfg.object_storage.is_empty(), "默认不得启用远端对象存储");
    }

    #[test]
    fn explicit_object_storage_space_requires_a_complete_configuration() {
        let mut input = valid_baseline();
        input.push(("OBJECT_STORAGE_SPACES", "avatars"));

        let error = parse(&input).expect_err("声明空间但缺少连接与策略字段应启动失败");

        assert!(
            error.to_string().contains("BACKEND"),
            "错误应指出缺失字段且不回显配置值"
        );
    }

    #[test]
    fn complete_object_storage_space_is_attached_to_application_config() {
        let mut input = valid_baseline();
        input.extend([
            ("OBJECT_STORAGE_SPACES", "avatars"),
            ("OBJECT_STORAGE_AVATARS_BACKEND", "oss"),
            (
                "OBJECT_STORAGE_AVATARS_OSS_ENDPOINT",
                "https://oss-cn-hangzhou.aliyuncs.com",
            ),
            ("OBJECT_STORAGE_AVATARS_OSS_REGION", "cn-hangzhou"),
            ("OBJECT_STORAGE_AVATARS_OSS_BUCKET", "tsz-avatars"),
            ("OBJECT_STORAGE_AVATARS_OSS_ROOT", "/avatars"),
            ("OBJECT_STORAGE_AVATARS_OSS_ACCESS_KEY_ID", "test-key-id"),
            (
                "OBJECT_STORAGE_AVATARS_OSS_ACCESS_KEY_SECRET",
                "test-key-secret",
            ),
            ("OBJECT_STORAGE_AVATARS_PRIVACY", "private"),
            ("OBJECT_STORAGE_AVATARS_MAX_OBJECT_SIZE_BYTES", "1048576"),
            ("OBJECT_STORAGE_AVATARS_PRESIGN_TTL_SECONDS", "300"),
            ("OBJECT_STORAGE_AVATARS_CACHE_CONTROL", "none"),
        ]);

        let cfg = parse(&input).expect("完整对象存储配置应接入应用配置");

        assert_eq!(cfg.object_storage.len(), 1);
    }

    #[test]
    fn env_key_is_case_insensitive() {
        // envy 把键统一小写化后匹配字段名，用小写键验证这一行为。
        // 备齐全部必填项（含 redis_url），否则会因缺项报错而非验证到大小写匹配。
        let input = [
            ("database_url", "postgres://localhost/tsz"),
            ("jwt_secret", "s3cret"),
            ("redis_url", "redis://localhost:6379/0"),
            ("admin_jwt_secret", "adm1n-s3cret"),
        ];
        let cfg = parse(&input).expect("小写键也应能匹配到字段");
        assert_eq!(cfg.database_url, "postgres://localhost/tsz");
    }

    #[test]
    fn smart_lexicon_v3_flags_default_to_disabled() {
        let cfg = parse(&valid_baseline()).expect("核心必填项齐全应能解析");

        assert_eq!(
            smart_lexicon_v3_flag_values(cfg.smart_lexicon_v3_flags),
            [false; 6],
            "未配置时六项 V3 能力必须全部关闭"
        );
        assert!(
            !cfg.smart_lexicon_v3_flags.sentence_associations,
            "Pending 例句关联是新增写能力，未显式配置时必须关闭"
        );
        assert!(
            !cfg.smart_lexicon_v3_flags.sentence_target_discovery,
            "一键发现会改变发布自动关联边界，未显式配置时必须关闭"
        );
        assert!(!cfg.smart_lexicon_v3_flags.draft_relation_prebinding);
    }

    #[test]
    fn smart_lexicon_v3_flags_parse_each_explicit_boolean() {
        let mut enabled = valid_baseline();
        enabled.extend([
            ("SMART_LEXICON_V3_READ", "true"),
            ("SMART_LEXICON_V3_CREATE", "true"),
            ("SMART_LEXICON_V3_EDIT", "true"),
            ("SMART_LEXICON_V3_PUBLISH", "true"),
            ("SMART_LEXICON_V3_PROJECTION", "true"),
            ("SMART_LEXICON_V3_LEGACY_BRIDGE_READ", "true"),
            ("SMART_LEXICON_V3_SENTENCE_ASSOCIATIONS", "true"),
            ("SMART_LEXICON_V3_SENTENCE_TARGET_DISCOVERY", "true"),
            ("SMART_LEXICON_V3_DRAFT_RELATION_PREBINDING", "true"),
            ("SMART_LEXICON_V3_SENSE_COMPONENT_USAGES", "true"),
        ]);
        let enabled = parse(&enabled).expect("显式 true 应能解析");
        assert_eq!(
            smart_lexicon_v3_flag_values(enabled.smart_lexicon_v3_flags),
            [true; 6]
        );
        assert!(enabled.smart_lexicon_v3_flags.sentence_associations);
        assert!(enabled.smart_lexicon_v3_flags.sentence_target_discovery);
        assert!(enabled.smart_lexicon_v3_flags.draft_relation_prebinding);

        let mut disabled = valid_baseline();
        disabled.extend([
            ("SMART_LEXICON_V3_READ", "false"),
            ("SMART_LEXICON_V3_CREATE", "false"),
            ("SMART_LEXICON_V3_EDIT", "false"),
            ("SMART_LEXICON_V3_PUBLISH", "false"),
            ("SMART_LEXICON_V3_PROJECTION", "false"),
            ("SMART_LEXICON_V3_LEGACY_BRIDGE_READ", "false"),
            ("SMART_LEXICON_V3_SENTENCE_ASSOCIATIONS", "false"),
            ("SMART_LEXICON_V3_SENTENCE_TARGET_DISCOVERY", "false"),
            ("SMART_LEXICON_V3_DRAFT_RELATION_PREBINDING", "false"),
            ("SMART_LEXICON_V3_SENSE_COMPONENT_USAGES", "false"),
        ]);
        let disabled = parse(&disabled).expect("显式 false 应能解析");

        assert_eq!(
            smart_lexicon_v3_flag_values(disabled.smart_lexicon_v3_flags),
            [false; 6]
        );
        assert!(!disabled.smart_lexicon_v3_flags.sentence_associations);
        assert!(!disabled.smart_lexicon_v3_flags.sentence_target_discovery);
        assert!(!disabled.smart_lexicon_v3_flags.draft_relation_prebinding);
    }

    #[test]
    fn smart_lexicon_v3_flags_reject_non_boolean_values() {
        let mut input = valid_baseline();
        input.push(("SMART_LEXICON_V3_CREATE", "1"));

        assert!(
            parse(&input).is_err(),
            "功能开关只接受明确的 true/false，不接受 1 等模糊值"
        );
    }
}
