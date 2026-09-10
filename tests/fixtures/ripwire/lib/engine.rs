pub fn accept_frame(value: u32) -> u32 { value + 1 }
pub fn local_consumer() -> u32 { accept_frame(1) }
pub trait FrameSink { fn put(&self, value: u32); }
pub fn trait_consumer(sink: &dyn FrameSink) { sink.put(accept_frame(2)); }
macro_rules! emit { ($value:expr) => { accept_frame($value) }; }
pub fn macro_consumer() -> u32 { emit!(3) }
pub fn closure_consumer() -> u32 { let next = || accept_frame(4); next() }
