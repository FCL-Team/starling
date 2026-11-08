//! The host launcher's add-ons screen: package checks in the engine's own process.
//!
//! Fold Craft Launcher's first embedded build exposes no add-ons bridge; the module
//! stays declared in `android_main.rs` so the engine's JNI surface keeps its shape.
//! A future build can add `com.mio.horizon` JNI exports here without touching the
//! rest of the engine.
