#[cfg(feature = "rtlsdr")]
pub mod rtlsdr;
pub mod traits;

pub use traits::*;

/// Returns true if the `rtlsdr` feature was compiled in.
pub const fn has_rtlsdr_feature() -> bool {
    cfg!(feature = "rtlsdr")
}

/// Returns true if the `audio` feature was compiled in.
pub const fn has_audio_feature() -> bool {
    cfg!(feature = "audio")
}
