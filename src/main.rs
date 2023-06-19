use frogbot::{run, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // init logging
    tracing_subscriber::fmt::init();
    let config = Config::load("./config.toml");
    run(config).await
}
