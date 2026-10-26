#[tokio::main]
async fn main() {
    match oplire::proxy_pool::run_cli().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::exit(1);
        }
    }
}
