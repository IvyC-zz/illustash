//! Historical forwarding host for synthetic differential tests only.
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "http") {
        args.remove(0);
    }
    match illustash_terms::http_reference::run(&args) {
        Ok(code) => std::process::exit(code.into()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
