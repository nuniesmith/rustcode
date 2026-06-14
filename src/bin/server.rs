// `RustCode` Server
//
// Thin entry point that loads configuration from environment
// and delegates to `server::run_server()`.

use rustcode::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install ring as the process-default rustls CryptoProvider before any TLS
    // client is built. reqwest uses rustls-no-provider (ring, matching sqlx);
    // this avoids the default aws-lc-rs, whose aws-lc-sys C build fails in Docker.
    let _ = rustls::crypto::ring::default_provider().install_default();

    dotenvy::dotenv().ok();

    let config = Config::load().map_err(|e| anyhow::anyhow!("Config load failed: {e}"))?;

    if let Err(e) = config.validate() {
        // Non-fatal for missing LLM key — server can still serve proxied requests
        tracing::warn!("Config validation warning: {}", e);
    }

    rustcode::run_server(config)
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {e}"))
}
