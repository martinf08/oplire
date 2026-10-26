use clap::Parser;

#[derive(Parser)]
#[command(
    name = "proxy-hunt",
    version,
    about = "Public proxy collection is disabled."
)]
struct Cli {}

fn main() {
    let _ = Cli::parse();
    eprintln!("proxy-hunt is disabled: public proxy collection and quota-driven rotation are not supported.");
    eprintln!("Add only proxies you are authorized to use for a legitimate, non-quota purpose.");
    std::process::exit(2);
}
