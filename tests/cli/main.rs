//! Black-box tests of the `maki` binary, one module per command area.
//! One test binary (`cargo test --test cli`); shared helpers in `common`.

mod common;

#[cfg(feature = "ai")]
mod ai;
mod audio;
mod basics;
mod collection;
mod dedup;
mod delete;
mod edit;
mod export;
mod group;
mod import;
mod previews;
mod relocate;
mod search;
mod sidecars;
mod stack;
mod sync;
mod tag;
mod verify;
mod volume;
#[cfg(feature = "pro")]
mod writeback;
