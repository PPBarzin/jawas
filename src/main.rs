#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    jawas::app::run().await
}
