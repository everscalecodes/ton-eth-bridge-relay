#![deny(
    clippy::unwrap_used,
    clippy::suspicious_operation_groupings,
    clippy::similar_names,
    clippy::same_functions_in_if_condition,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::option_if_let_else,
    clippy::needless_continue,
    clippy::needless_for_each,
    clippy::naive_bytecount,
    // clippy::multiple_crate_versions
)]
#![warn(
    clippy::indexing_slicing,
    clippy::if_then_some_else_none,
    clippy::explicit_into_iter_loop,
    clippy::dbg_macro,
    clippy::verbose_bit_mask,
    clippy::verbose_file_reads,
    clippy::unnested_or_patterns,
    clippy::unnecessary_wraps
)]
pub mod config;
pub mod crypto;
pub mod db;
pub mod engine;
pub mod models;
pub mod prelude;
