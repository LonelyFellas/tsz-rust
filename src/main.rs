use tsz_rust::config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    let config = config::load_config()?;

    tsz_rust::run(&config).await
}
