//! 计算器的显式测试传输通道；已安装的 worker 使用同一个 run_files 函数。
#[path = "research_compute/math.rs"]
mod math;
use std::path::PathBuf;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(args.next().ok_or("missing request.json")?);
    if input == std::path::Path::new("--math") {
        let input = PathBuf::from(args.next().ok_or("missing fixed matrix input")?);
        if args.next().is_some() {
            return Err("extra math argument".into());
        }
        return math::run(input);
    }
    let output = PathBuf::from(args.next().ok_or("missing response.json")?);
    if args.next().is_some() {
        return Err("extra calculator argument".into());
    }
    illustash_terms::research_compute::run_files(&input, &output)?;
    Ok(())
}
