//! 仅供测试的核心读宿主，用于合成读路由语料测试（#9）。
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
