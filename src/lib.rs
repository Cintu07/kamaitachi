pub mod text;
pub mod render;
pub mod sys;
pub mod arena;
pub mod json;
pub mod net;
pub mod records;
pub mod url;
pub mod html;
pub mod store;
pub mod config;
pub mod msg;
pub mod adapters;
pub mod auth;
pub mod ui;
pub mod mpv;
pub mod bench;
pub mod web;
pub mod gui;

/// Counts allocations so tests and `hayate bench` can prove the frame loop makes none.
#[global_allocator]
static ALLOC: arena::Counting = arena::Counting;
