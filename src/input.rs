//! Machine-code input helpers.
use crate::Result;
pub use crate::elf::{
    Code, FunctionSymbol, elf_function, elf_functions, elf_section, elf_sections, elf_text,
};
pub fn hex(s: &str) -> Result<Vec<u8>> {
    let compact: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '_')
        .collect();
    if !compact.len().is_multiple_of(2) || !compact.is_ascii() {
        return Err("hex input must contain pairs of ASCII hex digits".into());
    }
    (0..compact.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&compact[i..i + 2], 16).map_err(|_| "invalid hex digit".into()))
        .collect()
}
