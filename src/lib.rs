pub mod text;
pub mod render;
#[cfg(windows)]
pub mod sys;
pub mod arena;
pub mod json;
pub mod net;
#[cfg(windows)]
pub mod records;
pub mod url;
pub mod html;
#[cfg(windows)]
pub mod store;
#[cfg(windows)]
pub mod config;
#[cfg(windows)]
pub mod msg;
#[cfg(windows)]
pub mod adapters;
#[cfg(windows)]
pub mod auth;
#[cfg(windows)]
pub mod ui;
#[cfg(windows)]
pub mod mpv;
#[cfg(windows)]
pub mod bench;
pub mod web;
#[cfg(windows)]
pub mod gui;

/// Counts allocations so tests and `hayate bench` can prove the frame loop makes none.
#[global_allocator]
static ALLOC: arena::Counting = arena::Counting;
