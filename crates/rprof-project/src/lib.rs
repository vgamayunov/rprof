//! Evidence-aware, deterministic analytical projection. The same engine is used
//! by the CLI and the offline WebAssembly editor. No filesystem/network access.
mod engine;
mod model;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub use engine::{digest, project};
pub use model::*;
pub const ENGINE_VERSION: &str = "rprof-projection/1";
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

pub fn project_json(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        bytes.len() <= MAX_REQUEST_BYTES,
        "projection request exceeds 32 MiB"
    );
    let request: Request = serde_json::from_slice(bytes)?;
    Ok(serde_json::to_vec(&project(request)?)?)
}
