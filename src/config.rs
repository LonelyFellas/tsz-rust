use serde::Deserialize;
use std::num::NonZeroU16;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: NonZeroU16,
    pub database_url: String,
    pub jwt_secret: String,
}

fn default_port() -> NonZeroU16 {
    NonZeroU16::new(8383).unwrap()
}

impl Config {
    /// 从任意键值对解析配置——纯函数，不触碰进程环境，因此可单元测试。
    /// 生产的 [`load_config`] 与测试都走这条路径，保证被测逻辑即生产逻辑。
    pub fn from_pairs<I>(pairs: I) -> Result<Self, envy::Error>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        envy::from_iter(pairs)
    }
}

/// 生产入口：加载 `.env.local`（存在则读），再从进程环境解析配置。
pub fn load_config() -> Result<Config, envy::Error> {
    dotenvy::from_filename(".env.local").ok();
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
        ]
    }

    // 走生产接缝 Config::from_pairs —— 与 load_config 相同的解析路径，
    // 但喂入键值对、不触碰进程全局环境变量（testing-guide 铁律 7：禁 set_var）。
    fn parse(pairs: &[(&str, &str)]) -> Result<Config, envy::Error> {
        Config::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
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
    fn env_key_is_case_insensitive() {
        // envy 把键统一小写化后匹配字段名，用小写键验证这一行为。
        let input = [
            ("database_url", "postgres://localhost/tsz"),
            ("jwt_secret", "s3cret"),
        ];
        let cfg = parse(&input).expect("小写键也应能匹配到字段");
        assert_eq!(cfg.database_url, "postgres://localhost/tsz");
    }
}
