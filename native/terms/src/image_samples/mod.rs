//! 与旧版 Pillow 等价的采样解码器，由关系比较（#152）共用，
//! 待像素哈希溯源（#153）合入后也由它共用。
pub(crate) mod pnm;
pub(crate) mod tiff_scalar;
