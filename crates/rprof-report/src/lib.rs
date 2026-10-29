//! Shared capture analysis, offline HTML, text summaries, and trace export.

mod analyze;
mod html;
mod model;
mod text;
mod trace;

pub use analyze::{Limits, analyze};
pub use html::write_html;
pub use model::*;
pub use text::write_text;
pub use trace::write_trace;
