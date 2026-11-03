//! Shared capture analysis, offline HTML, text summaries, and trace export.

mod analyze;
mod clock;
mod cuda_summary;
mod html;
mod index_metrics;
pub mod indexed;
mod io_summary;
mod io_timeline;
mod model;
mod mpi_summary;
mod openmp_summary;
mod pmu_summary;
mod python_summary;
mod pytorch_summary;
mod system_timeline;
mod text;
mod trace;

pub use analyze::{Limits, analyze};
pub use html::{write_html, write_html_payload};
pub use model::*;
pub use text::write_text;
pub use trace::write_trace;
