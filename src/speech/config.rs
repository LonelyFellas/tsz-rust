use std::{collections::BTreeMap, fmt, time::Duration};

use thiserror::Error;

use super::AzureSpeechProvider;

const PREFIX: &str = "AZURE_SPEECH_";
const ALLOWED_FIELDS: [&str; 6] = [
    "ENABLED",
    "REGION",
    "KEY",
    "CONNECT_TIMEOUT_MS",
    "REQUEST_TIMEOUT_MS",
    "MAX_RESPONSE_BYTES",
];
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SpeechConfigError {
    #[error("Azure Speech variable '{name}' is configured more than once")]
    DuplicateVariable { name: String },
    #[error("Azure Speech variable '{name}' is unknown")]
    UnknownVariable { name: String },
    #[error("AZURE_SPEECH_ENABLED must be true or false")]
    InvalidEnabled,
    #[error("Azure Speech is disabled but additional Azure Speech variables are configured")]
    DisabledWithConfiguration,
    #[error("Azure Speech is missing required field '{field}'")]
    MissingField { field: &'static str },
    #[error("Azure Speech field '{field}' is invalid")]
    InvalidField { field: &'static str },
    #[error("Azure Speech HTTP client configuration is invalid")]
    InvalidClient,
}

#[derive(Clone, PartialEq, Eq)]
struct SecretKey(String);

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AzureSpeechConfig {
    region: String,
    key: SecretKey,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl fmt::Debug for AzureSpeechConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AzureSpeechConfig")
            .field("region", &self.region)
            .field("key", &self.key)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl AzureSpeechConfig {
    pub fn from_pairs<I>(pairs: I) -> Result<Option<Self>, SpeechConfigError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut variables = BTreeMap::new();
        for (name, value) in pairs {
            let name = name.to_ascii_uppercase();
            if !name.starts_with(PREFIX) {
                continue;
            }
            if variables.insert(name.clone(), value).is_some() {
                return Err(SpeechConfigError::DuplicateVariable { name });
            }
        }
        if variables.is_empty() {
            return Ok(None);
        }
        if let Some(name) = variables.keys().find(|name| {
            !ALLOWED_FIELDS
                .iter()
                .any(|field| name.as_str() == format!("{PREFIX}{field}"))
        }) {
            return Err(SpeechConfigError::UnknownVariable { name: name.clone() });
        }
        let enabled = match variables.get("AZURE_SPEECH_ENABLED").map(String::as_str) {
            Some("true") => true,
            Some("false") => false,
            _ => return Err(SpeechConfigError::InvalidEnabled),
        };
        if !enabled {
            return if variables.len() == 1 {
                Ok(None)
            } else {
                Err(SpeechConfigError::DisabledWithConfiguration)
            };
        }

        let required = |name: &'static str| {
            variables
                .get(&format!("{PREFIX}{name}"))
                .ok_or(SpeechConfigError::MissingField { field: name })
        };
        let region = required("REGION")?.clone();
        if !valid_region(&region) {
            return Err(SpeechConfigError::InvalidField { field: "REGION" });
        }
        let key = required("KEY")?.clone();
        if key.is_empty() || key.len() > 256 || key.chars().any(char::is_whitespace) {
            return Err(SpeechConfigError::InvalidField { field: "KEY" });
        }
        let connect_timeout = parse_millis(
            variables.get("AZURE_SPEECH_CONNECT_TIMEOUT_MS"),
            DEFAULT_CONNECT_TIMEOUT_MS,
            "CONNECT_TIMEOUT_MS",
            100,
            30_000,
        )?;
        let request_timeout = parse_millis(
            variables.get("AZURE_SPEECH_REQUEST_TIMEOUT_MS"),
            DEFAULT_REQUEST_TIMEOUT_MS,
            "REQUEST_TIMEOUT_MS",
            500,
            120_000,
        )?;
        if request_timeout < connect_timeout {
            return Err(SpeechConfigError::InvalidField {
                field: "REQUEST_TIMEOUT_MS",
            });
        }
        let max_response_bytes = variables.get("AZURE_SPEECH_MAX_RESPONSE_BYTES").map_or(
            Ok(DEFAULT_MAX_RESPONSE_BYTES),
            |value| {
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|value| (1024..=20 * 1024 * 1024).contains(value))
                    .ok_or(SpeechConfigError::InvalidField {
                        field: "MAX_RESPONSE_BYTES",
                    })
            },
        )?;
        Ok(Some(Self {
            region,
            key: SecretKey(key),
            connect_timeout,
            request_timeout,
            max_response_bytes,
        }))
    }

    pub fn build_provider(&self) -> Result<AzureSpeechProvider, SpeechConfigError> {
        AzureSpeechProvider::new(
            &self.region,
            self.key.0.clone(),
            self.connect_timeout,
            self.request_timeout,
            self.max_response_bytes,
        )
    }
}

fn parse_millis(
    value: Option<&String>,
    default: u64,
    field: &'static str,
    minimum: u64,
    maximum: u64,
) -> Result<Duration, SpeechConfigError> {
    let millis = value.map_or(Ok(default), |value| {
        value
            .parse::<u64>()
            .ok()
            .filter(|value| (minimum..=maximum).contains(value))
            .ok_or(SpeechConfigError::InvalidField { field })
    })?;
    Ok(Duration::from_millis(millis))
}

pub(crate) fn valid_region(region: &str) -> bool {
    !region.is_empty()
        && region.len() <= 63
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !region.starts_with('-')
        && !region.ends_with('-')
}
