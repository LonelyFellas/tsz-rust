use std::{sync::Arc, time::Duration};

pub struct AgentConfig {
    pub base_url: String,
    pub outbound_token: String,
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
        if base.is_none() && outbound.is_none() {
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
        anyhow::ensure!(
            outbound_token.len() >= 32,
            "service credential of at least 32 bytes required"
        );
        Ok(Some(Arc::new(Self {
            base_url: base_url.trim_end_matches('/').into(),
            outbound_token,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()?,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::AgentConfig;

    #[test]
    fn agent_config_ignores_retired_callback_credentials() {
        const CASE: &str = "TSZ_TEST_AGENT_CONFIG_RESULT";
        if let Ok(expected) = std::env::var(CASE) {
            match (expected.as_str(), AgentConfig::from_env()) {
                ("disabled", Ok(None)) | ("enabled", Ok(Some(_))) | ("invalid", Err(_)) => {}
                _ => panic!("unexpected Agent configuration result: {expected}"),
            }
            return;
        }
        for (url, token, callback, expected) in [
            (None, None, None, "disabled"),
            (None, None, Some("legacy-unused"), "disabled"),
            (
                Some("https://agent.example.test"),
                Some("outbound-test-token-at-least-32-characters"),
                None,
                "enabled",
            ),
            (
                Some("https://agent.example.test"),
                Some("outbound-test-token-at-least-32-characters"),
                Some("legacy-unused"),
                "enabled",
            ),
            (
                Some("http://agent.example.test"),
                Some("outbound-test-token-at-least-32-characters"),
                None,
                "invalid",
            ),
            (
                Some("https://agent.example.test"),
                Some("short"),
                None,
                "invalid",
            ),
            (Some("https://agent.example.test"), None, None, "invalid"),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "question_bank::worker::tests::agent_config_ignores_retired_callback_credentials",
            ]);
            command.env(CASE, expected);
            for (key, value) in [
                ("VOCABULARY_AGENT_URL", url),
                ("VOCABULARY_AGENT_TOKEN", token),
                ("VOCABULARY_AGENT_CALLBACK_TOKEN", callback),
            ] {
                command.env_remove(key);
                if let Some(value) = value {
                    command.env(key, value);
                }
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}
