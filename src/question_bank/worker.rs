use std::{sync::Arc, time::Duration};

pub struct AgentConfig {
    pub base_url: String,
    pub outbound_token: String,
    pub inbound_token: String,
    pub client: reqwest::Client,
}
impl AgentConfig {
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        let base = std::env::var("VOCABULARY_AGENT_URL")
            .ok()
            .filter(|v| !v.is_empty());
        let outbound = std::env::var("VOCABULARY_AGENT_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        let inbound = std::env::var("VOCABULARY_AGENT_CALLBACK_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        if base.is_none() && outbound.is_none() && inbound.is_none() {
            return Ok(None);
        }
        let base_url = base.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_URL missing"))?;
        let url = reqwest::Url::parse(&base_url)?;
        anyhow::ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
            "Agent URL must use HTTPS or loopback HTTP"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid Agent URL"
        );
        let outbound_token =
            outbound.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_TOKEN missing"))?;
        let inbound_token =
            inbound.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_CALLBACK_TOKEN missing"))?;
        anyhow::ensure!(
            outbound_token.len() >= 32
                && inbound_token.len() >= 32
                && outbound_token != inbound_token,
            "two distinct service credentials of at least 32 bytes required"
        );
        Ok(Some(Arc::new(Self {
            base_url: base_url.trim_end_matches('/').into(),
            outbound_token,
            inbound_token,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()?,
        })))
    }
}
