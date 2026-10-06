//! 词条核心的独立命令行，见 `illustash_terms::cli`。

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(illustash_terms::cli::run(&args))
}
