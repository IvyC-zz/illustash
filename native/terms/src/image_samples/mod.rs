//! Old-Pillow-equivalent sample decoders shared by relation comparison (#152)
//! and, once it lands, pixel-hash provenance (#153).
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
pub(crate) mod pnm;
pub(crate) mod tiff_scalar;
