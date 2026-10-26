use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Output, Stdio},
};

#[derive(Parser)]
#[command(
    name = "reset-opencode-default",
    version,
    about = "Restore the host's default network path without WARP."
)]
struct Cli {
    /// Print intended actions without changing networking or files.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Back up and remove WARP files after disconnecting the tunnel.
    #[arg(short = 'p', long)]
    purge: bool,
}

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let cli = Cli::parse();
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if !is_root() {
        if cli.dry_run {
            eprintln!("Error: run this diagnostic as root to inspect the system state.");
            return ExitCode::from(1);
        }
        for program in ["doas", "sudo"] {
            if let Ok(status) = Command::new(program)
                .arg(env::current_exe().unwrap_or_default())
                .args(args.clone())
                .status()
            {
                return ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8);
            }
        }
        eprintln!("Error: this tool must run as root (or through doas/sudo).");
        return ExitCode::from(1);
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let _ = Cli::parse();
    eprintln!("reset-opencode-default is a Linux-only network administration tool.");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
}

#[cfg(target_os = "linux")]
fn command_output(program: &str, args: &[&str]) -> Option<Output> {
    Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
}

#[cfg(target_os = "linux")]
fn command_succeeds(program: &str, args: &[&str]) -> bool {
    command_output(program, args).is_some_and(|output| output.status.success())
}

#[cfg(target_os = "linux")]
fn run_action(dry_run: bool, program: &str, args: &[&str]) {
    if dry_run {
        println!("  [dry-run] {program} {}", args.join(" "));
    } else {
        let _ = Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(target_os = "linux")]
fn run(cli: Cli) -> Result<()> {
    let state_dir = env::var_os("OPLIRE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"));
    println!("==================================================");
    println!("  Returning to the default connection (without WARP)");
    println!("==================================================");

    println!("[1/5] Stopping Oplire's optional proxy services...");
    if cli.dry_run {
        println!("  [dry-run] proxy-rotate stop");
    } else if let Err(error) = Command::new("proxy-rotate")
        .arg("stop")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        eprintln!("  [warning] could not stop proxy-rotate: {error}");
    }

    println!("[2/5] Disabling the wg0 tunnel...");
    if command_succeeds("ip", &["link", "show", "wg0"]) {
        if cli.dry_run {
            println!("  [dry-run] wg-quick down wg0");
            println!("  [dry-run] if needed, ip link del wg0 and warp-cli disconnect");
        } else if !command_succeeds("wg-quick", &["down", "wg0"]) {
            if !command_succeeds("ip", &["link", "del", "wg0"]) {
                eprintln!("  [warning] could not remove wg0 using wg-quick or ip");
            }
            run_action(false, "warp-cli", &["disconnect"]);
        } else {
            run_action(false, "warp-cli", &["disconnect"]);
        }
    } else {
        println!("  -> wg0 is already absent");
    }

    println!("[3/5] Cleaning up known WireGuard routing rules...");
    let routes = command_output("ip", &["-4", "route", "show", "scope", "link"])
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let lan = routes
        .lines()
        .filter(|line| line.contains(" proto kernel "))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<Vec<_>>()
        .join(",");
    let rules = command_output("ip", &["rule", "show"])
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let mut priorities = Vec::new();
    if !lan.is_empty()
        && rules
            .lines()
            .any(|line| line.contains(&format!("to {lan}")) && line.contains("lookup main"))
    {
        priorities.push("100".to_owned());
    }
    for line in rules
        .lines()
        .filter(|line| line.contains("lookup 51820") || line.contains("suppress_prefixlength 0"))
    {
        if let Some(priority) = line
            .split_whitespace()
            .next()
            .map(|value| value.trim_end_matches(':'))
        {
            if !priorities.iter().any(|known| known == priority) {
                priorities.push(priority.to_owned());
            }
        }
    }
    for priority in priorities {
        if cli.dry_run {
            println!("  [dry-run] ip rule del priority {priority}");
        } else {
            run_action(false, "ip", &["rule", "del", "priority", &priority]);
        }
    }

    println!("[4/5] Verifying the default route and DNS...");
    let routes = command_output("ip", &["route", "show", "default"])
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    if let Some(route) = routes.lines().find(|line| {
        !line
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["dev", "wg0"])
    }) {
        println!("  -> Default route: {route}");
    } else {
        eprintln!("  -> [warning] no default route outside wg0");
    }
    let dns = fs::read_to_string("/etc/resolv.conf")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("nameserver ").map(str::to_owned))
        });
    println!("  -> DNS: {}", dns.as_deref().unwrap_or("absent"));

    let trace = if cli.dry_run {
        None
    } else {
        command_output(
            "curl",
            &[
                "-fsS",
                "--noproxy",
                "*",
                "--max-time",
                "10",
                "https://www.cloudflare.com/cdn-cgi/trace",
            ],
        )
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let ip = trace
        .as_deref()
        .and_then(|text| text.lines().find_map(|line| line.strip_prefix("ip=")));
    let warp = trace
        .as_deref()
        .and_then(|text| text.lines().find_map(|line| line.strip_prefix("warp=")));
    if cli.dry_run {
        println!("  [dry-run] external network check skipped");
    } else if warp == Some("off") {
        println!("  -> WARP: OFF (default connection)");
        println!("  -> Public IP: {}", ip.unwrap_or("unknown"));
    } else if warp.is_none() {
        eprintln!("  -> [warning] network check unavailable; no routing change was triggered by that result");
    } else {
        bail!(
            "WARP still reports active (warp={}); inspect the tunnel and routing manually",
            warp.unwrap_or("unknown")
        );
    }

    println!(
        "[5/5] {}",
        if cli.purge {
            "Removing WARP files after backup..."
        } else {
            "WARP files retained (use --purge to remove them)."
        }
    );
    if cli.purge {
        purge_files(&state_dir, cli.dry_run)?;
    }

    println!("==================================================");
    if cli.dry_run {
        println!("  Dry run complete: no changes were made.");
    } else {
        println!("  Default connection procedure completed. Verify routing with your network administrator.");
    }
    println!("==================================================");
    Ok(())
}

#[cfg(target_os = "linux")]
fn purge_files(state_dir: &Path, dry_run: bool) -> Result<()> {
    let candidates = [
        PathBuf::from("/etc/wireguard/wg0.conf"),
        state_dir.join("wgcf-account.toml"),
        state_dir.join("wgcf-profile.conf"),
        PathBuf::from("/var/lib/cloudflare-warp"),
    ];
    let existing = candidates
        .iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        println!("  -> no WARP files found");
        return Ok(());
    }
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let backup = state_dir.join(format!("warp-backup-{stamp}.tar.gz"));
    if dry_run {
        println!("  [dry-run] create backup {}", backup.display());
        for path in &existing {
            println!(
                "  [dry-run] remove {} after verified backup",
                path.display()
            );
        }
        return Ok(());
    }
    fs::create_dir_all(state_dir)?;
    let status = Command::new("tar")
        .arg("czf")
        .arg(&backup)
        .args(&existing)
        .status()
        .context("run tar to back up WARP files")?;
    if !status.success() {
        bail!("backup failed; WARP files were not removed");
    }
    for path in existing {
        if path.is_dir() {
            fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
        } else {
            fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    println!("  -> Backup: {}", backup.display());
    Ok(())
}
