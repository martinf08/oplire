use std::process::{Command, Output};
use tempfile::TempDir;

fn proxy_rotate(args: &[&str], state: &TempDir) -> Output {
    Command::new(env!("CARGO_BIN_EXE_proxy-rotate"))
        .args(args)
        .env("OPLIRE_STATE_DIR", state.path())
        .output()
        .expect("run proxy-rotate")
}

#[test]
fn rate_limit_notice_never_creates_state_or_switches_proxy() {
    let state = TempDir::new().unwrap();
    let output = proxy_rotate(&["429"], &state);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no proxy switch"));
    assert!(!state.path().join("proxies.db").exists());

    let output = proxy_rotate(&["rotate", "--reason", "HTTP 429 quota"], &state);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains("must never trigger"));
    assert!(!state.path().join("proxies.db").exists());
}

#[test]
fn proxy_list_redacts_credentials() {
    let state = TempDir::new().unwrap();
    let added = proxy_rotate(&["add", "http://alice:secret@example.test:8080"], &state);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(!String::from_utf8_lossy(&added.stdout).contains("secret"));

    let listed = proxy_rotate(&["list", "--json"], &state);
    assert!(listed.status.success());
    let json = String::from_utf8_lossy(&listed.stdout);
    assert!(json.contains("[REDACTED]"));
    assert!(!json.contains("secret"));
}

#[test]
fn retired_tools_are_rust_binaries_and_keep_their_safety_guards() {
    let hunt = Command::new(env!("CARGO_BIN_EXE_proxy-hunt"))
        .output()
        .expect("run proxy-hunt");
    assert_eq!(hunt.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&hunt.stderr).contains("disabled"));

    let reset = Command::new(env!("CARGO_BIN_EXE_reset-opencode"))
        .arg("--default")
        .output()
        .expect("run reset-opencode");
    assert_eq!(reset.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&reset.stderr).contains("disabled"));
}

#[test]
fn reset_network_tool_exposes_dry_run_and_explicit_purge_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_reset-opencode-default"))
        .arg("--help")
        .output()
        .expect("run reset-opencode-default help");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--dry-run"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("--purge"));
}

#[test]
fn cli_help_does_not_display_credentials_from_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_oplire"))
        .args(["proxy", "--help"])
        .env("OPLIRE_API_KEY", "secret-api-key")
        .env(
            "OPLIRE_EGRESS_PROXY",
            "http://alice:secret-password@localhost:8080",
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("secret-api-key"));
    assert!(!help.contains("secret-password"));
}

#[cfg(unix)]
#[test]
fn installed_bundle_can_reinstall_itself_without_truncating_the_running_binary() {
    let root = TempDir::new().unwrap();
    let prefix = root.path().join("prefix");
    let install = |executable: &std::path::Path| {
        let output = Command::new(executable)
            .arg("install")
            .arg("--prefix")
            .arg(&prefix)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    install(std::path::Path::new(env!("CARGO_BIN_EXE_oplire")));
    install(&prefix.join("bin/oplire"));
    assert!(Command::new(prefix.join("bin/oplire"))
        .arg("--version")
        .status()
        .unwrap()
        .success());
}
