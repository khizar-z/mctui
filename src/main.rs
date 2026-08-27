mod hud;
mod live;
mod terminal;

use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let config = live::LiveConfig::parse_env()?;
    live::start(config).await
}
