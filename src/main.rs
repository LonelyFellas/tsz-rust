use axum::{Router, routing::get};

const ADDR: &str = "0.0.0.0:8383";

#[tokio::main]
async fn main() {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    // 构建我们应用的一个路由
    let app = Router::new().route("/", get(root));

    // 运行我们应用with hyper, 并且监听和创建全局的8383端口
    let listener = tokio::net::TcpListener::bind(ADDR).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn root() -> &'static str {
    "Hello, World!"
}
