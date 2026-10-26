use std::{
    env,
    ffi::OsString,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(first) = args.first().and_then(|arg| arg.to_str()) {
        if matches!(
            first,
            "--fast" | "-f" | "--no-hunt" | "--default" | "-d" | "--proxy" | "-p"
        ) {
            eprintln!("Legacy reset/rotation options are disabled; respect provider limits.");
            return ExitCode::from(2);
        }
    }

    if args.first().is_some_and(|arg| arg == "--check") {
        if args.len() < 2 {
            eprintln!("Usage: reset-opencode [--check URL] [arguments for OpenCode...]");
            return ExitCode::from(2);
        }
        let url = args.remove(1);
        args.remove(0);
        let status = match Command::new("oplire")
            .args([OsString::from("check"), OsString::from("--url"), url])
            .status()
        {
            Ok(status) => status,
            Err(error) => {
                eprintln!("oplire was not found or could not be started: {error}");
                return ExitCode::from(127);
            }
        };
        if !status.success() {
            return ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
        if args.is_empty() {
            return ExitCode::SUCCESS;
        }
    }

    match Command::new("opencode").args(args).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("opencode was not found in PATH: {error}");
            ExitCode::from(127)
        }
    }
}
