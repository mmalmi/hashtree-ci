//! GitHub Actions workflow parser.
//!
//! Parses `.github/workflows/*.yml` files into executable jobs.

pub mod parser;

pub use parser::*;
