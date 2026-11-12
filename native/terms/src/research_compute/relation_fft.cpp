// NumPy 2.5.3 输出 complex64 的 rfft2 和 float32 的 irfft2，只限 NCC 用到的尺寸。
// 官方头文件和完整 BSD 许可的来源说明：docs/ARCHITECTURE_DIRECTION.md。
#define POCKETFFT_NO_MULTITHREADING
#include "pocketfft_hdronly.h"
#include <algorithm>
#include <complex>
#include <limits>
#include <vector>

using C = std::complex<float>;
using D = std::complex<double>;
using PC = pocketfft::detail::cmplx<double>;
static_assert(sizeof(D) == sizeof(PC), "pocketfft complex layout");

// NCC 总是在两个轴上都补零。NumPy 的 nin < nout 路径即使其他维度允许向量化，
// 也有意使用标量 plan。
extern "C" int illustash_relation_rfft2(const float *input, size_t ih, size_t iw,
                                       size_t h, size_t w, float *output) noexcept {
    const size_t pw = w / 2 + 1;
    if (!input || !output || !ih || !iw || ih >= h || iw >= w ||
        h > size_t(std::numeric_limits<ptrdiff_t>::max()) / sizeof(C) / pw)
        return 1;
    try {
        // _raw_fft 在 norm=None 时传入 Python 整数 1。NumPy 2.5.3 的 gufunc
        // 即使输出是 complex64，也把它解析为 double 循环。
        // 输出在「每个」轴之后转换回去，而不是在整个变换之后。
        auto real = pocketfft::detail::get_plan<pocketfft::detail::pocketfft_r<double>>(w);
        auto complex = pocketfft::detail::get_plan<pocketfft::detail::pocketfft_c<double>>(h);
        std::vector<C> rows(ih * pw);
        std::vector<D> row(pw), column(h);
        for (size_t y = 0; y < ih; ++y) {
            double *packed = reinterpret_cast<double *>(row.data()) + 1;
            std::copy(input + y * iw, input + (y + 1) * iw, packed);
            std::fill(packed + iw, packed + 2 * pw - 1, 0.);
            real->exec(packed, 1., pocketfft::FORWARD);
            row[0] = row[0].imag();
            for (size_t x = 0; x < pw; ++x)
                rows[y * pw + x] = C(float(row[x].real()), float(row[x].imag()));
        }
        C *out = reinterpret_cast<C *>(output);
        for (size_t x = 0; x < pw; ++x) {
            for (size_t y = 0; y < ih; ++y) column[y] = rows[y * pw + x];
            std::fill(column.begin() + ih, column.end(), D(0.));
            complex->exec(reinterpret_cast<PC *>(column.data()), 1., pocketfft::FORWARD);
            for (size_t y = 0; y < h; ++y)
                out[y * pw + x] = C(float(column[y].real()), float(column[y].imag()));
        }
        return 0;
    } catch (...) { return 1; }
}

extern "C" int illustash_relation_irfft2(const float *input, size_t h, size_t w,
                                        float *output) noexcept {
    const size_t pw = w / 2 + 1;
    if (!input || !output || !h || !w ||
        h > size_t(std::numeric_limits<ptrdiff_t>::max()) / sizeof(C) / pw ||
        h > size_t(std::numeric_limits<ptrdiff_t>::max()) / sizeof(float) / w)
        return 1;
    try {
        const ptrdiff_t cs = sizeof(C), rs = sizeof(float);
        std::vector<C> columns(h * pw);
        // NumPy irfft2：先在轴 0 上做复数逆变换，再在轴 1 上做实数逆变换。
        pocketfft::c2c({pw, h}, {cs, cs * ptrdiff_t(pw)}, {cs, cs * ptrdiff_t(pw)},
                      {1}, pocketfft::BACKWARD, reinterpret_cast<const C *>(input),
                      columns.data(), 1.f / float(h));
        pocketfft::c2r({h, w}, {cs * ptrdiff_t(pw), cs}, {rs * ptrdiff_t(w), rs},
                      size_t(1), pocketfft::BACKWARD, columns.data(), output, 1.f / float(w));
        return 0;
    } catch (...) { return 1; }
}
