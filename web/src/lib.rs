//! Browser platform for Horizon. The wire codecs and simulation data are the
//! same crates as the native client; no native runtime is linked into WASM.
#![forbid(unsafe_code)]
pub mod session;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod renderer;

// Reuse LocalPlayer.sendPosition and vanilla's relative teleport calculation.
#[path = "../../crates/horizon-client/src/player_sync.rs"]
pub mod player_sync;
