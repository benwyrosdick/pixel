//! Document model for the Pixel photo editor.
//!
//! The window code lives in the binary. Everything that changes pixels,
//! layers, or files is in this crate so it can be tested without GTK.

pub mod document;
