fn main() {
    println!("cargo:rerun-if-changed=../../native/terms/src/research_compute/relation_fft.cpp");
    println!("cargo:rerun-if-changed=../../native/terms/vendor/pocketfft/pocketfft_hdronly.h");
    let mut fft = cc::Build::new();
    fft.cpp(true)
        .std("c++11")
        .opt_level(3)
        // NumPy 2.5.3 的顶层 meson.build 要求 C 和 C++ 都使用严格的 Clang 浮点行为；
        // 浮点收缩（contraction）会改变 PocketFFT 的 float32 结果位。
        .flag_if_supported("-ftrapping-math")
        .file("../../native/terms/src/research_compute/relation_fft.cpp")
        .include("../../native/terms/vendor/pocketfft");
    // MSVC 默认按系统代码页读源码，中文系统上是 GBK：UTF-8 的中文注释会吞掉行尾换行，
    // 把下一行代码也变成注释。源码里的非 ASCII 字符只在注释里，按 UTF-8 读不改变编译结果。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        fft.flag("/utf-8");
    }
    fft.compile("illustash_relation_fft");
}
