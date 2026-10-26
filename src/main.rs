use anyhow::Context;
use clap::{Parser, Subcommand};
use oplire::{config::ProxyConfig, health::check_url, proxy::start_proxy_server};
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::process::{Command as ProcessCommand, Stdio};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tracing_subscriber::EnvFilter;

const PROGRAMS: [&str; 5] = [
    "oplire",
    "proxy-rotate",
    "proxy-hunt",
    "reset-opencode",
    "reset-opencode-default",
];

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
        #[arg(long, env = "OPLIRE_API_KEY", hide_env_values = true)]
        api_key: Option<String>,
        /// Explicit HTTP(S) or SOCKS5 proxy for upstream traffic (for example, a local WARP proxy).
        #[arg(long, env = "OPLIRE_EGRESS_PROXY", hide_env_values = true)]
        egress_proxy: Option<String>,
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
    /// Install all Oplire executables.
    Install {
        /// Installation prefix (defaults to /usr/local, or the per-user Programs directory on Windows).
        #[arg(long)]
        prefix: Option<PathBuf>,
        /// Download and checksum-verify the latest GitHub release.
        #[arg(long)]
        latest: bool,
        /// Install and enable the systemd proxy connectivity watcher.
        #[arg(long)]
        with_service: bool,
        /// Print changes without writing files or enabling services.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
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
            egress_proxy,
        } => {
            start_proxy_server(ProxyConfig {
                listen_addr: listen,
                upstream_base_url: upstream,
                api_key,
                egress_proxy,
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
        Command::Install {
            prefix,
            latest,
            with_service,
            dry_run,
        } => {
            install_release(
                &prefix.unwrap_or_else(default_prefix),
                latest,
                with_service,
                dry_run,
            )
            .await?;
        }
    }
    Ok(())
}

fn default_prefix() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Programs/Oplire")
    } else {
        PathBuf::from("/usr/local")
    }
}

async fn install_release(
    prefix: &Path,
    latest: bool,
    with_service: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    if with_service {
        anyhow::bail!("--with-service is supported only on Linux with systemd");
    }

    #[cfg(target_os = "linux")]
    if with_service && !dry_run {
        let identity = ProcessCommand::new("id").arg("-u").output()?;
        if String::from_utf8_lossy(&identity.stdout).trim() != "0" {
            anyhow::bail!(
                "--with-service writes to /etc/systemd and requires root; rerun with sudo/doas"
            );
        }
    }

    // Reject unsupported service setups before installing any files.
    #[cfg(target_os = "linux")]
    if with_service
        && !ProcessCommand::new("systemctl")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    {
        anyhow::bail!("systemd is required for --with-service");
    }

    let bin_dir = prefix.join("bin");
    if latest {
        if dry_run {
            println!(
                "[dry-run] download and SHA-256-verify the latest release for {}",
                release_target()?
            );
            println!(
                "[dry-run] install five executables into {}",
                bin_dir.display()
            );
        } else {
            // Download and verify all files before making any installation changes.
            let binaries = download_latest_binaries().await?;
            create_install_bin_dir(&bin_dir)?;
            for (name, bytes) in binaries {
                let destination = bin_dir.join(name);
                write_installed_binary(&destination, &bytes).with_context(|| {
                    format!(
                        "cannot install {}; use sudo or choose a writable --prefix",
                        destination.display()
                    )
                })?;
                println!("Installed {}", destination.display());
            }
        }
    } else {
        let executable = std::env::current_exe()?;
        let source_dir = executable.parent().unwrap_or_else(|| Path::new("."));
        let extension = if cfg!(windows) { ".exe" } else { "" };
        let sources = PROGRAMS
            .iter()
            .map(|name| (*name, source_dir.join(format!("{name}{extension}"))))
            .collect::<Vec<_>>();
        for (_, source) in &sources {
            if !source.is_file() {
                anyhow::bail!("release bundle is incomplete; missing {}", source.display());
            }
        }
        if dry_run {
            println!("[dry-run] create {}", bin_dir.display());
            for (name, _) in &sources {
                println!("[dry-run] install {name} into {}", bin_dir.display());
            }
        } else {
            create_install_bin_dir(&bin_dir)?;
            for (name, source) in &sources {
                let destination = bin_dir.join(format!("{name}{extension}"));
                let bytes = fs::read(source)?;
                write_installed_binary(&destination, &bytes).with_context(|| {
                    format!(
                        "cannot install {}; use sudo or choose a writable --prefix",
                        destination.display()
                    )
                })?;
                println!("Installed {}", destination.display());
            }
        }
    }

    if with_service {
        #[cfg(target_os = "linux")]
        {
            let service = include_str!("../systemd/proxy-rotate.service")
                .replace("/usr/local/bin", &bin_dir.to_string_lossy());
            let service_path = Path::new("/etc/systemd/system/proxy-rotate.service");
            if dry_run {
                println!("[dry-run] write {}", service_path.display());
                println!(
                    "[dry-run] systemctl daemon-reload && systemctl enable --now proxy-rotate"
                );
            } else {
                fs::write(service_path, service)?;
                let reload = ProcessCommand::new("systemctl")
                    .arg("daemon-reload")
                    .status()?;
                if !reload.success() {
                    anyhow::bail!("systemctl daemon-reload failed");
                }
                let enable = ProcessCommand::new("systemctl")
                    .args(["enable", "--now", "proxy-rotate"])
                    .status()?;
                if !enable.success() {
                    anyhow::bail!("could not enable proxy-rotate.service");
                }
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn release_target() -> anyhow::Result<&'static str> {
    if cfg!(target_env = "musl") {
        anyhow::bail!("prebuilt releases target GNU/glibc, not musl; build from source on Alpine");
    }
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        _ => anyhow::bail!("automatic installation is not available for this OS/architecture; download a supported archive from GitHub Releases"),
    }
}

async fn download_latest_binaries() -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    const REPOSITORY: &str = "martinf08/oplire";
    let target = release_target()?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let asset_names = PROGRAMS
        .iter()
        .map(|name| format!("{name}-{target}{suffix}"))
        .collect::<Vec<_>>();
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(concat!(
            "Oplire/",
            env!("CARGO_PKG_VERSION"),
            " release installer"
        ))
        .build()?;
    let release: GithubRelease = client
        .get(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases/latest"
        ))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let checksum_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS")
        .context("latest release has no SHA256SUMS manifest")?;
    let manifest = download_release_asset(&client, checksum_asset).await?;
    let manifest = String::from_utf8(manifest).context("release checksum manifest is not UTF-8")?;
    let checksums = manifest
        .lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once("  ")?;
            Some((
                name.trim_start_matches('*').to_owned(),
                hash.to_ascii_lowercase(),
            ))
        })
        .collect::<HashMap<_, _>>();

    let mut binaries = Vec::with_capacity(asset_names.len());
    for (program, asset_name) in PROGRAMS.iter().zip(asset_names) {
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .with_context(|| format!("latest release is missing {asset_name}"))?;
        let expected = checksums
            .get(&asset_name)
            .with_context(|| format!("{asset_name} is missing from SHA256SUMS"))?;
        let bytes = download_release_asset(&client, asset).await?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if &actual != expected {
            anyhow::bail!("SHA-256 verification failed for {asset_name}");
        }
        binaries.push((format!("{program}{suffix}"), bytes));
    }
    Ok(binaries)
}

async fn download_release_asset(
    client: &reqwest::Client,
    asset: &GithubAsset,
) -> anyhow::Result<Vec<u8>> {
    let url = reqwest::Url::parse(&asset.browser_download_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        anyhow::bail!("refusing an unexpected download URL for {}", asset.name);
    }
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

fn write_installed_binary(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    // Stage beside the destination so Unix upgrades can replace a running
    // executable, without truncating it if a write fails midway.
    let parent = path.parent().context("installation path has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    set_executable_permissions(staged.path())?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn create_install_bin_dir(bin_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(bin_dir).with_context(|| {
        format!(
            "cannot create {}; use sudo or choose a writable --prefix",
            bin_dir.display()
        )
    })
}

fn set_executable_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_installation_replaces_existing_files_without_leftovers() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("oplire");
        fs::write(&destination, b"old binary").unwrap();
        write_installed_binary(&destination, b"new binary").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new binary");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}
