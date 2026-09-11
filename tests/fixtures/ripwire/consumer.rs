mod lib { pub mod engine; }
pub fn cross_consumer() -> u32 { lib::engine::accept_frame(5) }
