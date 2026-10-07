//! Test-only core read host for the synthetic read-route corpus tests (#9).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match illustash_terms::read_harness::run(&args) {
        Ok(code) => std::process::exit(code.into()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
