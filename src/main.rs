use clap::{Parser, Subcommand};
use oplire::{config::ProxyConfig, health::check_url, proxy::start_proxy_server};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "oplire",
    version,
    about = "OpenCode health checks and Anthropic-compatible API bridge"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the local Anthropic-compatible API bridge.
    Proxy {
        /// Address to bind; defaults to loopback only.
        #[arg(long, env = "OPLIRE_LISTEN", default_value = "127.0.0.1:8080")]
        listen: String,
        /// OpenAI-compatible upstream base URL.
        #[arg(
            long,
            env = "OPLIRE_UPSTREAM_URL",
            default_value = "http://127.0.0.1:3000"
        )]
        upstream: String,
        /// Optional bearer token for the upstream.
        #[arg(long, env = "OPLIRE_API_KEY")]
        api_key: Option<String>,
    },
    /// Check a URL with bounded retries for transient failures; never retries HTTP 429.
    Check {
        /// URL to check.
        #[arg(long, env = "OPLIRE_CHECK_URL")]
        url: String,
        /// Maximum attempts for network errors and server errors.
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
        attempts: u8,
    },
    /// Print the bridge's bundled model catalogue.
    Models,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    match Cli::parse().command {
        Command::Proxy {
            listen,
            upstream,
            api_key,
        } => {
            start_proxy_server(ProxyConfig {
                listen_addr: listen,
                upstream_base_url: upstream,
                api_key,
            })
            .await?;
        }
        Command::Check { url, attempts } => {
            check_url(&url, attempts).await?;
            println!("Health check succeeded: {url}");
        }
        Command::Models => {
            println!(
                "{}",
                serde_json::to_string_pretty(&ProxyConfig::models_response())?
            );
        }
    }

    Ok(())
}
