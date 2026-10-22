pub mod analysis;
pub mod dataflow;
pub mod decode;
pub mod elf;
pub mod emit;
pub mod floating;
pub mod input;
pub mod integer;
pub mod ir;
pub mod scan;
pub mod strings;
pub mod vector;

pub type Result<T> = std::result::Result<T, String>;
