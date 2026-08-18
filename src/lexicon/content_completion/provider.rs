use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct OpenAiLexiconConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct QwenLexiconConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub enum LexiconGeneratorConfig {
    OpenAi(OpenAiLexiconConfig),
    Qwen(QwenLexiconConfig),
}

impl OpenAiLexiconConfig {
    pub fn from_pairs<I>(pairs: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let values = pairs
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let key = values.get("OPENAI_LEXICON_API_KEY").cloned();
        let model = values.get("OPENAI_LEXICON_MODEL").cloned();
        match (key, model) {
            (None, None) => Ok(None),
            (Some(api_key), Some(model))
                if !api_key.trim().is_empty() && !model.trim().is_empty() =>
            {
                let timeout_seconds = values
                    .get("OPENAI_LEXICON_TIMEOUT_SECONDS")
                    .map(|value| {
                        value.parse::<u64>().map_err(|_| {
                            "OPENAI_LEXICON_TIMEOUT_SECONDS must be a positive integer".to_owned()
                        })
                    })
                    .transpose()?
                    .unwrap_or(90);
                if timeout_seconds == 0 {
                    return Err("OPENAI_LEXICON_TIMEOUT_SECONDS must be positive".to_owned());
                }
                Ok(Some(Self {
                    api_key,
                    model,
                    base_url: values
                        .get("OPENAI_LEXICON_BASE_URL")
                        .cloned()
                        .unwrap_or_else(|| "https://api.openai.com/v1".to_owned())
                        .trim_end_matches('/')
                        .to_owned(),
                    timeout: Duration::from_secs(timeout_seconds),
                }))
            }
            _ => Err(
                "OPENAI_LEXICON_API_KEY and OPENAI_LEXICON_MODEL must be configured together"
                    .to_owned(),
            ),
        }
    }
}

impl QwenLexiconConfig {
    pub fn from_pairs<I>(pairs: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let values = pairs
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let key = values.get("QWEN_LEXICON_API_KEY").cloned();
        let model = values.get("QWEN_LEXICON_MODEL").cloned();
        match (key, model) {
            (None, None) => Ok(None),
            (Some(api_key), Some(model))
                if !api_key.trim().is_empty() && !model.trim().is_empty() =>
            {
                let timeout_seconds = values
                    .get("QWEN_LEXICON_TIMEOUT_SECONDS")
                    .map(|value| {
                        value.parse::<u64>().map_err(|_| {
                            "QWEN_LEXICON_TIMEOUT_SECONDS must be a positive integer".to_owned()
                        })
                    })
                    .transpose()?
                    .unwrap_or(90);
                if timeout_seconds == 0 {
                    return Err("QWEN_LEXICON_TIMEOUT_SECONDS must be positive".to_owned());
                }
                Ok(Some(Self {
                    api_key,
                    model,
                    base_url: values
                        .get("QWEN_LEXICON_BASE_URL")
                        .cloned()
                        .unwrap_or_else(|| {
                            "https://dashscope.aliyuncs.com/compatible-mode/v1".to_owned()
                        })
                        .trim_end_matches('/')
                        .to_owned(),
                    timeout: Duration::from_secs(timeout_seconds),
                }))
            }
            _ => Err(
                "QWEN_LEXICON_API_KEY and QWEN_LEXICON_MODEL must be configured together"
                    .to_owned(),
            ),
        }
    }
}

impl LexiconGeneratorConfig {
    pub fn from_pairs<I>(pairs: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let pairs = pairs.into_iter().collect::<Vec<_>>();
        let provider = pairs
            .iter()
            .find(|(key, _)| key == "LEXICON_GENERATOR_PROVIDER")
            .map(|(_, value)| value.trim().to_ascii_lowercase());

        match provider.as_deref() {
            None
                if !pairs.iter().any(|(key, _)| {
                    key.starts_with("OPENAI_LEXICON_") || key.starts_with("QWEN_LEXICON_")
                }) =>
            {
                Ok(None)
            }
            None => Err("LEXICON_GENERATOR_PROVIDER must be configured when a lexicon generator is configured".to_owned()),
            Some("openai") => OpenAiLexiconConfig::from_pairs(pairs.iter().cloned())?
                .map(Self::OpenAi)
                .ok_or_else(|| "openai lexicon generator configuration is incomplete".to_owned())
                .map(Some),
            Some("qwen") => QwenLexiconConfig::from_pairs(pairs.iter().cloned())?
                .map(Self::Qwen)
                .ok_or_else(|| "qwen lexicon generator configuration is incomplete".to_owned())
                .map(Some),
            Some(_) => Err("LEXICON_GENERATOR_PROVIDER must be openai or qwen".to_owned()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentGenerationSource {
    pub entry_id: Uuid,
    pub pos_id: Uuid,
    pub headword: String,
    pub pos: String,
    pub dialect_mode: String,
    pub dictionary_provider: String,
    pub dictionary_version: String,
    pub source_record_keys: Vec<String>,
    pub dictionary_evidence: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedGrammar {
    pub common: Option<String>,
    pub uk: Option<String>,
    pub us: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedDefinition {
    pub zh: String,
    pub en: String,
    pub level: String,
    pub grammar_index: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedExample {
    pub en: String,
    pub zh: String,
    pub level: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedSense {
    pub group_name_zh: String,
    pub group_name_en: String,
    pub sub_pos: String,
    pub level: String,
    pub depends_on_context: bool,
    pub definitions: Vec<GeneratedDefinition>,
    pub examples: Vec<GeneratedExample>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedPosContent {
    pub grammar_structures: Vec<GeneratedGrammar>,
    pub senses: Vec<GeneratedSense>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContentGeneratorError {
    #[error("provider is not configured")]
    NotConfigured,
    #[error("provider rate limited the request")]
    RateLimited,
    #[error("provider request timed out")]
    Timeout,
    #[error("provider rejected the content")]
    SafetyRejected,
    #[error("provider returned invalid structured output")]
    InvalidOutput,
    #[error("provider is unavailable")]
    Unavailable,
}

impl ContentGeneratorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "provider_not_configured",
            Self::RateLimited => "provider_rate_limited",
            Self::Timeout => "provider_timeout",
            Self::SafetyRejected => "provider_safety_rejected",
            Self::InvalidOutput => "invalid_structured_output",
            Self::Unavailable => "provider_unavailable",
        }
    }
}

#[async_trait]
pub trait LexiconContentGenerator: Send + Sync {
    fn provider_name(&self) -> &'static str;
    fn model(&self) -> &str;
    async fn generate(
        &self,
        source: &ContentGenerationSource,
    ) -> Result<GeneratedPosContent, ContentGeneratorError>;
}

pub struct OpenAiContentGenerator {
    client: Client,
    config: OpenAiLexiconConfig,
}

pub struct QwenContentGenerator {
    client: Client,
    config: QwenLexiconConfig,
}

const GENERATION_SYSTEM_PROMPT: &str = "You create evidence-grounded English dictionary teaching content. Return only the requested schema. Include only meanings supported by dictionary_evidence glosses. Do not add senses from general knowledge. Examples, grammar patterns, Chinese translations, and CEFR are model-generated teaching aids, not source facts. Never invent dialect differences, use CEFR A1-C2 only, create 1-5 distinct senses and 2-3 bilingual examples per sense. Chinese must be natural Simplified Chinese. Grammar structures are concise usage patterns, not inflection tables.";
const MAX_PROVIDER_RESPONSE_BYTES: usize = 1_048_576;

fn generation_messages(source: &ContentGenerationSource) -> Value {
    json!([
        {
            "role": "system",
            "content": GENERATION_SYSTEM_PROMPT
        },
        {
            "role": "user",
            "content": serde_json::to_string(source).expect("generation source must serialize")
        }
    ])
}

impl OpenAiContentGenerator {
    pub fn new(config: OpenAiLexiconConfig) -> Result<Self, reqwest::Error> {
        let client = Client::builder().timeout(config.timeout).build()?;
        Ok(Self { client, config })
    }

    fn request_body(&self, source: &ContentGenerationSource) -> Value {
        json!({
            "model": self.config.model,
            "store": false,
            "input": generation_messages(source),
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "lexicon_pos_content",
                    "strict": true,
                    "schema": output_schema()
                }
            }
        })
    }
}

impl QwenContentGenerator {
    pub fn new(config: QwenLexiconConfig) -> Result<Self, reqwest::Error> {
        let client = Client::builder().timeout(config.timeout).build()?;
        Ok(Self { client, config })
    }

    fn request_body(&self, source: &ContentGenerationSource) -> Value {
        json!({
            "model": self.config.model,
            "messages": generation_messages(source),
            "stream": false,
            "reasoning_effort": "low",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "lexicon_pos_content",
                    "strict": true,
                    "schema": output_schema()
                }
            }
        })
    }
}

#[async_trait]
impl LexiconContentGenerator for OpenAiContentGenerator {
    fn provider_name(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    async fn generate(
        &self,
        source: &ContentGenerationSource,
    ) -> Result<GeneratedPosContent, ContentGeneratorError> {
        let response = self
            .client
            .post(format!("{}/responses", self.config.base_url))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.config.api_key),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(self.request_body(source).to_string())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ContentGeneratorError::Timeout
                } else {
                    ContentGeneratorError::Unavailable
                }
            })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(ContentGeneratorError::RateLimited);
        }
        if !response.status().is_success() {
            return Err(ContentGeneratorError::Unavailable);
        }
        let payload = read_bounded_json(response).await?;
        if payload["output"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .any(|content| content["type"] == "refusal")
        {
            return Err(ContentGeneratorError::SafetyRejected);
        }
        let text = payload["output"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .find(|content| content["type"] == "output_text")
            .and_then(|content| content["text"].as_str())
            .ok_or(ContentGeneratorError::InvalidOutput)?;
        serde_json::from_str(text).map_err(|_| ContentGeneratorError::InvalidOutput)
    }
}

#[async_trait]
impl LexiconContentGenerator for QwenContentGenerator {
    fn provider_name(&self) -> &'static str {
        "qwen"
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    async fn generate(
        &self,
        source: &ContentGenerationSource,
    ) -> Result<GeneratedPosContent, ContentGeneratorError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.config.api_key),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(self.request_body(source).to_string())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ContentGeneratorError::Timeout
                } else {
                    ContentGeneratorError::Unavailable
                }
            })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(ContentGeneratorError::RateLimited);
        }
        if !response.status().is_success() {
            return Err(ContentGeneratorError::Unavailable);
        }
        let payload = read_bounded_json(response).await?;
        if payload["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["finish_reason"].as_str())
            == Some("content_filter")
        {
            return Err(ContentGeneratorError::SafetyRejected);
        }
        let text = payload["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .filter(|content| !content.trim().is_empty())
            .ok_or(ContentGeneratorError::InvalidOutput)?;
        serde_json::from_str(text).map_err(|_| ContentGeneratorError::InvalidOutput)
    }
}

async fn read_bounded_json(mut response: Response) -> Result<Value, ContentGeneratorError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(ContentGeneratorError::InvalidOutput);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ContentGeneratorError::Unavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(ContentGeneratorError::InvalidOutput);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ContentGeneratorError::InvalidOutput)
}

fn output_schema() -> Value {
    let nullable_string = json!({"type": ["string", "null"], "maxLength": 500});
    json!({
        "type": "object",
        "properties": {
            "grammar_structures": {
                "type": "array", "minItems": 1, "maxItems": 6,
                "items": {
                    "type": "object",
                    "properties": {"common": nullable_string, "uk": nullable_string, "us": nullable_string},
                    "required": ["common", "uk", "us"],
                    "additionalProperties": false
                }
            },
            "senses": {
                "type": "array", "minItems": 1, "maxItems": 5,
                "items": {
                    "type": "object",
                    "properties": {
                        "group_name_zh": {"type": "string", "maxLength": 120},
                        "group_name_en": {"type": "string", "maxLength": 120},
                        "sub_pos": {"type": "string", "maxLength": 64},
                        "level": {"type": "string", "enum": ["A1", "A2", "B1", "B2", "C1", "C2"]},
                        "depends_on_context": {"type": "boolean"},
                        "definitions": {
                            "type": "array", "minItems": 1, "maxItems": 3,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "zh": {"type": "string", "maxLength": 2000}, "en": {"type": "string", "maxLength": 2000},
                                    "level": {"type": "string", "enum": ["A1", "A2", "B1", "B2", "C1", "C2"]},
                                    "grammar_index": {"type": ["integer", "null"], "minimum": 0}
                                },
                                "required": ["zh", "en", "level", "grammar_index"],
                                "additionalProperties": false
                            }
                        },
                        "examples": {
                            "type": "array", "minItems": 2, "maxItems": 3,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "en": {"type": "string", "maxLength": 2000}, "zh": {"type": "string", "maxLength": 2000},
                                    "level": {"type": "string", "enum": ["A1", "A2", "B1", "B2", "C1", "C2"]}
                                },
                                "required": ["en", "zh", "level"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["group_name_zh", "group_name_en", "sub_pos", "level", "depends_on_context", "definitions", "examples"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["grammar_structures", "senses"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> ContentGenerationSource {
        ContentGenerationSource {
            entry_id: Uuid::now_v7(),
            pos_id: Uuid::now_v7(),
            headword: "bank".to_owned(),
            pos: "noun".to_owned(),
            dialect_mode: "unified".to_owned(),
            dictionary_provider: "Kaikki English Wiktionary".to_owned(),
            dictionary_version: "test-v1".to_owned(),
            source_record_keys: vec!["kaikki:bank:noun:test".to_owned()],
            dictionary_evidence: json!({
                "content": [{"senses": [{"glosses": ["A financial institution"]}]}]
            }),
        }
    }

    async fn test_server(
        status: StatusCode,
        response: Value,
    ) -> (String, std::sync::Arc<tokio::sync::Mutex<Option<Value>>>) {
        use axum::{Json, Router, routing::post};
        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let handler_capture = captured.clone();
        let app = Router::new().route(
            "/responses",
            post(move |Json(request): Json<Value>| {
                let captured = handler_capture.clone();
                let response = response.clone();
                async move {
                    *captured.lock().await = Some(request);
                    (status, Json(response))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), captured)
    }

    async fn qwen_test_server(
        status: StatusCode,
        response: Value,
    ) -> (String, std::sync::Arc<tokio::sync::Mutex<Option<Value>>>) {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let handler_capture = captured.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let captured = handler_capture.clone();
                let response = response.clone();
                async move {
                    *captured.lock().await = Some(json!({
                        "authorization": headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        "body": request
                    }));
                    (status, Json(response))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), captured)
    }

    async fn delayed_qwen_test_server(delay: Duration) -> String {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/chat/completions",
            post(move || async move {
                tokio::time::sleep(delay).await;
                (StatusCode::OK, Json(json!({"choices": []})))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[test]
    fn provider_configuration_is_all_or_nothing() {
        assert!(
            OpenAiLexiconConfig::from_pairs(Vec::new())
                .unwrap()
                .is_none()
        );
        assert!(
            OpenAiLexiconConfig::from_pairs([("OPENAI_LEXICON_API_KEY".into(), "secret".into())])
                .is_err()
        );
        let config = OpenAiLexiconConfig::from_pairs([
            ("OPENAI_LEXICON_API_KEY".into(), "secret".into()),
            ("OPENAI_LEXICON_MODEL".into(), "test-model".into()),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.model, "test-model");
        assert_eq!(config.timeout, Duration::from_secs(90));
    }

    #[test]
    fn qwen_configuration_is_all_or_nothing_and_uses_official_base_url() {
        assert!(QwenLexiconConfig::from_pairs(Vec::new()).unwrap().is_none());
        assert!(
            QwenLexiconConfig::from_pairs([("QWEN_LEXICON_MODEL".into(), "qwen3.8-max".into())])
                .is_err()
        );
        let config = QwenLexiconConfig::from_pairs([
            ("QWEN_LEXICON_API_KEY".into(), "secret".into()),
            ("QWEN_LEXICON_MODEL".into(), "qwen3.8-max".into()),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.model, "qwen3.8-max");
        assert_eq!(
            config.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(config.timeout, Duration::from_secs(90));
    }

    #[test]
    fn generator_configuration_requires_an_explicit_supported_provider() {
        let qwen = [
            ("QWEN_LEXICON_API_KEY".to_owned(), "secret".to_owned()),
            ("QWEN_LEXICON_MODEL".to_owned(), "qwen3.8-max".to_owned()),
        ];
        assert!(LexiconGeneratorConfig::from_pairs(qwen.clone()).is_err());
        assert!(
            LexiconGeneratorConfig::from_pairs(
                qwen.clone()
                    .into_iter()
                    .chain([("LEXICON_GENERATOR_PROVIDER".into(), "unknown".into())])
            )
            .is_err()
        );
        let selected = LexiconGeneratorConfig::from_pairs(
            qwen.into_iter()
                .chain([("LEXICON_GENERATOR_PROVIDER".into(), "qwen".into())]),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(selected, LexiconGeneratorConfig::Qwen(_)));

        let selected_with_unused_partial_openai = LexiconGeneratorConfig::from_pairs([
            ("LEXICON_GENERATOR_PROVIDER".into(), "qwen".into()),
            ("QWEN_LEXICON_API_KEY".into(), "secret".into()),
            ("QWEN_LEXICON_MODEL".into(), "qwen3.8-max".into()),
            ("OPENAI_LEXICON_MODEL".into(), "unused".into()),
        ])
        .unwrap();
        assert!(matches!(
            selected_with_unused_partial_openai,
            Some(LexiconGeneratorConfig::Qwen(_))
        ));
    }

    #[test]
    fn structured_schema_closes_every_object_and_requires_nullable_fields() {
        let schema = output_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["grammar_structures"]["items"]["additionalProperties"],
            false
        );
        assert!(
            schema["properties"]["grammar_structures"]["items"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "uk")
        );
    }

    #[tokio::test]
    async fn provider_sends_non_stored_structured_request_and_parses_output() {
        let output = json!({
            "grammar_structures": [{"common": "a bank for something", "uk": null, "us": null}],
            "senses": [{
                "group_name_zh": "金融机构",
                "group_name_en": "financial institution",
                "sub_pos": "",
                "level": "A2",
                "depends_on_context": false,
                "definitions": [{
                    "zh": "银行",
                    "en": "a financial institution",
                    "level": "A2",
                    "grammar_index": 0
                }],
                "examples": [
                    {"en": "I went to the bank.", "zh": "我去了银行。", "level": "A1"},
                    {"en": "The bank approved the loan.", "zh": "银行批准了贷款。", "level": "B1"}
                ]
            }]
        });
        let (base_url, captured) = test_server(
            StatusCode::OK,
            json!({"output": [{"content": [{"type": "output_text", "text": output.to_string()}]}]}),
        )
        .await;
        let provider = OpenAiContentGenerator::new(OpenAiLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        let generated = provider.generate(&source()).await.unwrap();
        assert_eq!(generated.senses.len(), 1);
        let request = captured.lock().await.clone().unwrap();
        assert_eq!(request["store"], false);
        assert_eq!(request["text"]["format"]["type"], "json_schema");
        assert_eq!(request["text"]["format"]["strict"], true);
    }

    #[tokio::test]
    async fn provider_maps_rate_limit_and_refusal_without_fallback_content() {
        let (base_url, _) = test_server(StatusCode::TOO_MANY_REQUESTS, json!({})).await;
        let provider = OpenAiContentGenerator::new(OpenAiLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::RateLimited)
        ));

        let (base_url, _) = test_server(
            StatusCode::OK,
            json!({"output": [{"content": [{"type": "refusal", "refusal": "unsafe"}]}]}),
        )
        .await;
        let provider = OpenAiContentGenerator::new(OpenAiLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::SafetyRejected)
        ));
    }

    #[tokio::test]
    async fn qwen_sends_strict_chat_completion_and_parses_output() {
        let output = json!({
            "grammar_structures": [{"common": "a bank for something", "uk": null, "us": null}],
            "senses": [{
                "group_name_zh": "金融机构",
                "group_name_en": "financial institution",
                "sub_pos": "",
                "level": "A2",
                "depends_on_context": false,
                "definitions": [{
                    "zh": "银行", "en": "a financial institution", "level": "A2",
                    "grammar_index": 0
                }],
                "examples": [
                    {"en": "I went to the bank.", "zh": "我去了银行。", "level": "A1"},
                    {"en": "The bank approved the loan.", "zh": "银行批准了贷款。", "level": "B1"}
                ]
            }]
        });
        let (base_url, captured) = qwen_test_server(
            StatusCode::OK,
            json!({"choices": [{"message": {"content": output.to_string()}, "finish_reason": "stop"}]}),
        )
        .await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();

        let generated = provider.generate(&source()).await.unwrap();

        assert_eq!(generated.senses.len(), 1);
        assert_eq!(provider.provider_name(), "qwen");
        let request = captured.lock().await.clone().unwrap();
        assert_eq!(request["authorization"], "Bearer test-key");
        assert_eq!(request["body"]["stream"], false);
        assert_eq!(request["body"]["reasoning_effort"], "low");
        assert_eq!(request["body"]["response_format"]["type"], "json_schema");
        assert_eq!(
            request["body"]["response_format"]["json_schema"]["strict"],
            true
        );
        assert_eq!(
            request["body"]["response_format"]["json_schema"]["schema"]["additionalProperties"],
            false
        );
    }

    #[tokio::test]
    async fn qwen_maps_rate_limit_safety_and_invalid_output_without_fallback() {
        let (base_url, _) = qwen_test_server(StatusCode::TOO_MANY_REQUESTS, json!({})).await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::RateLimited)
        ));

        let (base_url, _) = qwen_test_server(
            StatusCode::OK,
            json!({"choices": [{"message": {"content": ""}, "finish_reason": "content_filter"}]}),
        )
        .await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::SafetyRejected)
        ));

        let (base_url, _) = qwen_test_server(
            StatusCode::OK,
            json!({"choices": [{"message": {"content": "not-json"}, "finish_reason": "stop"}]}),
        )
        .await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::InvalidOutput)
        ));

        let oversized = "x".repeat(MAX_PROVIDER_RESPONSE_BYTES + 1);
        let (base_url, _) = qwen_test_server(
            StatusCode::OK,
            json!({"choices": [{"message": {"content": oversized}, "finish_reason": "stop"}]}),
        )
        .await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::InvalidOutput)
        ));
    }

    #[tokio::test]
    async fn qwen_maps_non_success_and_timeout_without_fallback() {
        let (base_url, _) = qwen_test_server(StatusCode::BAD_GATEWAY, json!({})).await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_secs(2),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::Unavailable)
        ));

        let base_url = delayed_qwen_test_server(Duration::from_millis(50)).await;
        let provider = QwenContentGenerator::new(QwenLexiconConfig {
            api_key: "test-key".to_owned(),
            model: "qwen3.8-max".to_owned(),
            base_url,
            timeout: Duration::from_millis(1),
        })
        .unwrap();
        assert!(matches!(
            provider.generate(&source()).await,
            Err(ContentGeneratorError::Timeout)
        ));
    }

    #[tokio::test]
    #[ignore = "requires explicit QWEN_LEXICON_API_KEY and QWEN_LEXICON_MODEL and incurs provider cost"]
    async fn qwen_real_provider_smoke() {
        let config = QwenLexiconConfig::from_pairs(std::env::vars())
            .expect("千问配置必须合法")
            .expect("运行真实 smoke 前必须显式配置千问凭据与模型");
        let provider = QwenContentGenerator::new(config).unwrap();

        let generated = provider.generate(&source()).await.unwrap();

        assert!(!generated.grammar_structures.is_empty());
        assert!(!generated.senses.is_empty());
        assert!(generated.senses.iter().all(|sense| {
            !sense.definitions.is_empty() && (2..=3).contains(&sense.examples.len())
        }));
    }
}
