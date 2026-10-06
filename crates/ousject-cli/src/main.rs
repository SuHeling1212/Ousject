mod cli;

fn main() {
    if let Err(error) = cli::run_cli() {
        eprintln!("ousject: {error}");
        std::process::exit(1);
    }
}
