#[tokio::main]
async fn main() {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    if let Err(e) = tsz_rust::run().await {
        tracing::error!("启动服务失败: {e:?}");
        std::process::exit(1);
    }
}
