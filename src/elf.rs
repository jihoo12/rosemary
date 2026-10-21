//! Bounds-checked ELF64 executable sections and ordinary function symbols.
use crate::Result;
use std::{collections::BTreeMap, fmt::Write};

pub struct Code<'a> {
    pub bytes: &'a [u8],
    pub base: u64,
    pub entry: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub offset: u64,
    pub executable: bool,
    pub kind: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSymbol {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub section: String,
    /// Present in a dynamic symbol table, possibly also in a static table.
    pub dynamic: bool,
}

fn slice(b: &[u8], off: u64, size: u64) -> Result<&[u8]> {
    let end = off.checked_add(size).ok_or("file range overflow")?;
    let start = usize::try_from(off).map_err(|_| "file offset too large")?;
    let end = usize::try_from(end).map_err(|_| "file range too large")?;
    b.get(start..end)
        .ok_or_else(|| "file range out of bounds".into())
}
fn num(b: &[u8], off: u64, n: u64) -> Result<u64> {
    let mut v = 0;
    for (i, x) in slice(b, off, n)?.iter().enumerate() {
        v |= (*x as u64) << (8 * i);
    }
    Ok(v)
}
fn string(bytes: &[u8], offset: u64) -> Result<&str> {
    let tail = bytes
        .get(usize::try_from(offset).map_err(|_| "invalid string offset")?..)
        .ok_or("string offset out of bounds")?;
    let end = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or("unterminated ELF string")?;
    std::str::from_utf8(&tail[..end]).map_err(|_| "ELF name is not UTF-8".into())
}
struct Section {
    name: String,
    kind: u64,
    flags: u64,
    address: u64,
    offset: u64,
    size: u64,
    link: u64,
    entry_size: u64,
}
pub(crate) struct Elf<'a> {
    bytes: &'a [u8],
    entry: u64,
    sections: Vec<Section>,
}
impl<'a> Elf<'a> {
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.get(..4) != Some(b"\x7fELF") {
            return Err("not an ELF file".into());
        }
        slice(bytes, 0, 64)?;
        if bytes[4] != 2
            || bytes[5] != 1
            || bytes[6] != 1
            || num(bytes, 18, 2)? != 62
            || num(bytes, 20, 4)? != 1
        {
            return Err("expected ELF64 little-endian x86-64 version 1".into());
        }
        if !matches!(num(bytes, 16, 2)?, 2 | 3) {
            return Err(
                "ELF must be an executable or shared object; relocatable objects are not supported"
                    .into(),
            );
        }
        let table = num(bytes, 40, 8)?;
        let stride = num(bytes, 58, 2)?;
        let count = num(bytes, 60, 2)?;
        let names = num(bytes, 62, 2)?;
        if stride < 64 || count == 0 || names >= count {
            return Err(
                "missing/unsupported ELF section table (extended indexes unsupported)".into(),
            );
        }
        slice(
            bytes,
            table,
            stride.checked_mul(count).ok_or("section table overflow")?,
        )?;
        let names_header = table + stride * names;
        let strings = slice(
            bytes,
            num(bytes, names_header + 24, 8)?,
            num(bytes, names_header + 32, 8)?,
        )?;
        let mut sections = Vec::new();
        for index in 0..count {
            let h = table + stride * index;
            sections.push(Section {
                name: string(strings, num(bytes, h, 4)?)?.into(),
                kind: num(bytes, h + 4, 4)?,
                flags: num(bytes, h + 8, 8)?,
                address: num(bytes, h + 16, 8)?,
                offset: num(bytes, h + 24, 8)?,
                size: num(bytes, h + 32, 8)?,
                link: num(bytes, h + 40, 4)?,
                entry_size: num(bytes, h + 56, 8)?,
            });
        }
        Ok(Self {
            bytes,
            entry: num(bytes, 24, 8)?,
            sections,
        })
    }
    fn code(&self, section: &Section) -> Result<Code<'a>> {
        if section.kind != 1 || section.flags & 4 == 0 {
            return Err("section is not executable PROGBITS".into());
        }
        Ok(Code {
            bytes: slice(self.bytes, section.offset, section.size)?,
            base: section.address,
            entry: if self.entry == 0 {
                section.address
            } else {
                self.entry
            },
        })
    }
    pub(crate) fn functions(&self) -> Result<Vec<(FunctionSymbol, usize)>> {
        let mut found: BTreeMap<(u64, String, u64, usize), FunctionSymbol> = BTreeMap::new();
        for table in &self.sections {
            if !matches!(table.kind, 2 | 11) {
                continue;
            }
            if table.entry_size < 24 || !table.size.is_multiple_of(table.entry_size) {
                return Err("invalid ELF symbol table entry size".into());
            }
            let symbols = slice(self.bytes, table.offset, table.size)?;
            let names = self
                .sections
                .get(usize::try_from(table.link).map_err(|_| "invalid symbol string table")?)
                .filter(|s| s.kind == 3)
                .ok_or("invalid symbol string table link")?;
            let strings = slice(self.bytes, names.offset, names.size)?;
            for index in 0..table.size / table.entry_size {
                let offset = index * table.entry_size;
                let info = num(symbols, offset + 4, 1)?;
                if info & 15 != 2 {
                    continue;
                } // STT_FUNC
                let section_index = num(symbols, offset + 6, 2)?;
                if section_index == 0 {
                    continue;
                } // undefined import
                if section_index == 0xffff {
                    return Err("extended symbol section indexes unsupported".into());
                }
                if section_index >= 0xff00 {
                    continue;
                } // absolute/common symbols have no section bytes
                let section_index = section_index as usize;
                let section = self
                    .sections
                    .get(section_index)
                    .ok_or("invalid function section index")?;
                if section.kind != 1 || section.flags & 4 == 0 {
                    continue;
                }
                let name = string(strings, num(symbols, offset, 4)?)?.to_string();
                if name.is_empty() {
                    continue;
                }
                let address = num(symbols, offset + 8, 8)?;
                let size = num(symbols, offset + 16, 8)?;
                let relative = address
                    .checked_sub(section.address)
                    .ok_or("function before its section")?;
                if relative >= section.size || size > section.size - relative {
                    return Err(format!("function {name:?} extends outside its section"));
                }
                let key = (address, name.clone(), size, section_index);
                let symbol = found.entry(key).or_insert(FunctionSymbol {
                    name,
                    address,
                    size,
                    section: section.name.clone(),
                    dynamic: false,
                });
                symbol.dynamic |= table.kind == 11;
            }
        }
        Ok(found
            .into_iter()
            .map(|((_, _, _, index), symbol)| (symbol, index))
            .collect())
    }
    pub(crate) fn function_code(&self, symbol: &FunctionSymbol, index: usize) -> Result<Code<'a>> {
        if symbol.size == 0 {
            return Err(format!(
                "function {:?} has no size; use --entry with --recursive and an explicit code range",
                symbol.name
            ));
        }
        let section = &self.sections[index];
        let code = self.code(section)?;
        Ok(Code {
            bytes: slice(code.bytes, symbol.address - section.address, symbol.size)?,
            base: symbol.address,
            entry: symbol.address,
        })
    }
}

pub fn elf_text(bytes: &[u8]) -> Result<Code<'_>> {
    let elf = Elf::parse(bytes)?;
    let section = elf
        .sections
        .iter()
        .find(|s| s.name == ".text")
        .ok_or("ELF has no .text section; use raw mode with an explicit file range")?;
    elf.code(section)
}
pub fn elf_section<'a>(bytes: &'a [u8], name: &str) -> Result<Code<'a>> {
    let elf = Elf::parse(bytes)?;
    let mut matches = elf.sections.iter().filter(|s| s.name == name);
    let section = matches
        .next()
        .ok_or_else(|| format!("section {name:?} not found"))?;
    if matches.next().is_some() {
        return Err(format!("ambiguous section name {name:?}"));
    }
    let mut code = elf.code(section)?;
    if !code
        .entry
        .checked_sub(code.base)
        .is_some_and(|offset| offset < code.bytes.len() as u64)
    {
        code.entry = code.base;
    }
    Ok(code)
}
pub fn elf_entry(bytes: &[u8]) -> Result<u64> {
    Ok(Elf::parse(bytes)?.entry)
}
pub fn elf_executable_code(bytes: &[u8]) -> Result<Vec<Code<'_>>> {
    let elf = Elf::parse(bytes)?;
    elf.sections
        .iter()
        .filter(|s| s.kind == 1 && s.flags & 4 != 0 && s.size != 0)
        .map(|s| elf.code(s))
        .collect()
}
pub fn elf_sections(bytes: &[u8]) -> Result<Vec<SectionInfo>> {
    Ok(Elf::parse(bytes)?
        .sections
        .iter()
        .map(|s| SectionInfo {
            name: s.name.clone(),
            address: s.address,
            size: s.size,
            offset: s.offset,
            executable: s.kind == 1 && s.flags & 4 != 0,
            kind: s.kind,
        })
        .collect())
}
pub fn sections_text(sections: &[SectionInfo]) -> String {
    let mut s = String::from("ADDRESS            SIZE       FILE OFFSET        EXEC  SECTION\n");
    for section in sections {
        writeln!(
            s,
            "0x{:016x} {:10} 0x{:016x} {:5} {}",
            section.address,
            section.size,
            section.offset,
            if section.executable { "yes" } else { "no" },
            section.name.escape_default()
        )
        .unwrap();
    }
    s
}
pub fn sections_json(entry: u64, sections: &[SectionInfo]) -> String {
    use crate::analysis::{hex, quoted};
    let mut s = format!(
        "{{\"schema\":\"rosemary-sections-1\",\"entry\":{},\"sections\":[",
        hex(entry)
    );
    for (n, section) in sections.iter().enumerate() {
        if n != 0 {
            s.push(',');
        }
        write!(s,"\n{{\"name\":{},\"address\":{},\"size\":{},\"file_offset\":{},\"executable\":{},\"kind\":{}}}",quoted(&section.name),hex(section.address),hex(section.size),hex(section.offset),section.executable,section.kind).unwrap();
    }
    s.push_str("\n]}\n");
    s
}
pub fn elf_functions(bytes: &[u8]) -> Result<Vec<FunctionSymbol>> {
    Ok(Elf::parse(bytes)?
        .functions()?
        .into_iter()
        .map(|(s, _)| s)
        .collect())
}
pub fn symbols_text(symbols: &[FunctionSymbol]) -> String {
    let mut s = format!(
        "{} function symbols\nADDRESS            SIZE       TABLE     SECTION / NAME\n",
        symbols.len()
    );
    for symbol in symbols {
        writeln!(
            s,
            "0x{:016x} {:10} {:9} {} / {}",
            symbol.address,
            symbol.size,
            if symbol.dynamic { "dynamic" } else { "static" },
            symbol.section.escape_default(),
            symbol.name.escape_default()
        )
        .unwrap();
    }
    s
}
pub fn symbols_json(symbols: &[FunctionSymbol]) -> String {
    use crate::analysis::{hex, quoted};
    let mut s = String::from("{\"schema\": \"rosemary-symbols-1\", \"functions\": [");
    for (n, symbol) in symbols.iter().enumerate() {
        if n != 0 {
            s.push(',');
        }
        write!(
            s,
            "\n  {{\"name\": {}, \"address\": {}, \"size\": {}, \"section\": {}, \"dynamic\": {}}}",
            quoted(&symbol.name),
            hex(symbol.address),
            hex(symbol.size),
            quoted(&symbol.section),
            symbol.dynamic
        )
        .unwrap();
    }
    s.push_str("\n]}\n");
    s
}
pub fn elf_function<'a>(bytes: &'a [u8], name: &str) -> Result<Code<'a>> {
    let elf = Elf::parse(bytes)?;
    let matches: Vec<_> = elf
        .functions()?
        .into_iter()
        .filter(|(s, _)| s.name == name)
        .collect();
    if matches.len() > 1 {
        return Err(format!(
            "ambiguous function {name:?}; select by address with --entry"
        ));
    }
    let (symbol, index) = matches
        .into_iter()
        .next()
        .ok_or_else(|| format!("function {name:?} not found (binary may be stripped)"))?;
    elf.function_code(&symbol, index)
}
