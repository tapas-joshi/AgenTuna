//! AgenTuna CLI entry point.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("agentuna {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    println!("agentuna {}", env!("CARGO_PKG_VERSION"));
}
