//! Batch analysis of real ELF function symbols, including Rosemary itself.
use crate::{
    Result,
    analysis::{self, hex, quoted},
    decode::{RecursiveOptions, decode_linear, decode_recursive},
    elf::{Elf, FunctionSymbol},
};
use std::{collections::BTreeMap, fmt::Write};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Recursive,
    Linear,
}
impl ScanMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Recursive => "recursive",
            Self::Linear => "linear",
        }
    }
}

#[derive(Debug)]
pub struct FunctionScan {
    pub symbol: FunctionSymbol,
    pub instructions: usize,
    pub blocks: usize,
    pub proven_branches: usize,
    pub inferred_targets: usize,
    pub unresolved_transfers: usize,
    pub decoded_bytes: usize,
    pub error: Option<String>,
}
#[derive(Debug)]
pub struct ScanReport {
    pub mode: ScanMode,
    pub max_instructions: usize,
    /// FNV-1a identifier for reproducible comparisons, not a security hash.
    pub fingerprint: u64,
    pub functions: Vec<FunctionScan>,
}
pub fn scan(bytes: &[u8], filter: Option<&str>, max_instructions: usize) -> Result<ScanReport> {
    scan_with_mode(bytes, filter, max_instructions, ScanMode::Recursive)
}
pub fn scan_with_mode(
    bytes: &[u8],
    filter: Option<&str>,
    max_instructions: usize,
    mode: ScanMode,
) -> Result<ScanReport> {
    if max_instructions == 0 {
        return Err("instruction limit must be positive".into());
    }
    let image = Elf::parse(bytes)?;
    let mut functions = Vec::new();
    for (symbol, index) in image.functions()? {
        if filter.is_some_and(|filter| !symbol.name.contains(filter)) {
            continue;
        }
        let result = (|| {
            let code = image.function_code(&symbol, index)?;
            let p = if mode == ScanMode::Linear {
                decode_linear(code.bytes, code.base, Some(code.entry), max_instructions)?
            } else {
                decode_recursive(
                    code.bytes,
                    code.base,
                    code.entry,
                    RecursiveOptions {
                        follow_calls: false,
                        max_instructions,
                    },
                )?
            };
            let a = analysis::analyze(&p)?;
            Ok((
                p.instructions.len(),
                a.blocks.len(),
                (0..p.instructions.len())
                    .filter(|&n| a.values.branch(n).is_some())
                    .count(),
                a.edges.iter().filter(|e| e.inferred).count(),
                a.edges
                    .iter()
                    .filter(|e| e.resolution == analysis::Resolution::Indirect)
                    .count(),
                p.instructions.iter().map(|i| i.len as usize).sum(),
            ))
        })();
        let mut f = FunctionScan {
            symbol,
            instructions: 0,
            blocks: 0,
            proven_branches: 0,
            inferred_targets: 0,
            unresolved_transfers: 0,
            decoded_bytes: 0,
            error: None,
        };
        match result {
            Ok((instructions, blocks, branches, targets, unresolved, decoded_bytes)) => {
                f.instructions = instructions;
                f.blocks = blocks;
                f.proven_branches = branches;
                f.inferred_targets = targets;
                f.unresolved_transfers = unresolved;
                f.decoded_bytes = decoded_bytes;
            }
            Err(error) => f.error = Some(error),
        }
        functions.push(f);
    }
    let fingerprint = bytes.iter().fold(0xcbf29ce484222325u64, |hash, b| {
        (hash ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    Ok(ScanReport {
        mode,
        max_instructions,
        fingerprint,
        functions,
    })
}
impl ScanReport {
    pub fn lifted(&self) -> usize {
        self.functions.iter().filter(|f| f.error.is_none()).count()
    }
    pub fn errors(&self) -> Vec<(String, usize)> {
        let mut counts = BTreeMap::new();
        for f in &self.functions {
            if let Some(error) = &f.error {
                let reason = if error.starts_with("0x") {
                    error.split_once(": ").map_or(error.as_str(), |(_, s)| s)
                } else {
                    error
                };
                *counts.entry(reason.to_string()).or_insert(0) += 1;
            }
        }
        let mut errors: Vec<_> = counts.into_iter().collect();
        errors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        errors
    }
    pub fn text(&self) -> String {
        let mut s = format!(
            "ELF scan (FNV-1a {:016x}, {} mode)\n{} / {} function symbols lifted; {} failed\nLinear mode covers every byte in the symbol; recursive mode follows discovered paths.\n",
            self.fingerprint,
            self.mode.name(),
            self.lifted(),
            self.functions.len(),
            self.functions.len() - self.lifted()
        );
        for f in &self.functions {
            writeln!(
                s,
                "{} 0x{:x} {}{}",
                if f.error.is_none() { "OK" } else { "FAIL" },
                f.symbol.address,
                f.symbol.name.escape_default(),
                f.error.as_ref().map_or_else(
                    || format!(
                        ": {} instructions, {} blocks, {} proven branches, {} unresolved transfers",
                        f.instructions, f.blocks, f.proven_branches, f.unresolved_transfers
                    ),
                    |e| format!(": {e}")
                )
            )
            .unwrap();
        }
        if self.functions.iter().any(|f| f.error.is_some()) {
            s.push_str("\nFailure groups:\n");
            for (reason, count) in self.errors() {
                writeln!(s, "  {count}: {reason}").unwrap();
            }
        }
        s
    }
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\"schema\": \"rosemary-scan-1\", \"fingerprint_fnv1a\": \"{:016x}\", \"mode\": \"{}\", \"instruction_limit\": {}, \"function_count\": {}, \"lifted\": {}, \"failed\": {}, \"functions\": [",
            self.fingerprint,
            self.mode.name(),
            self.max_instructions,
            self.functions.len(),
            self.lifted(),
            self.functions.len() - self.lifted()
        );
        for (n, f) in self.functions.iter().enumerate() {
            if n != 0 {
                s.push(',');
            }
            write!(s,"\n  {{\"name\": {}, \"address\": {}, \"size\": {}, \"status\": {}, \"instructions\": {}, \"decoded_bytes\": {}, \"blocks\": {}, \"proven_branches\": {}, \"inferred_targets\": {}, \"unresolved_transfers\": {}, \"error\": {}}}",
                quoted(&f.symbol.name),hex(f.symbol.address),hex(f.symbol.size),quoted(if f.error.is_none() {"lifted"}else{"failed"}),
                f.instructions,f.decoded_bytes,f.blocks,f.proven_branches,f.inferred_targets,f.unresolved_transfers,f.error.as_ref().map_or("null".into(),|e|quoted(e))).unwrap();
        }
        s.push_str("\n], \"failure_groups\": [");
        for (n, (reason, count)) in self.errors().iter().enumerate() {
            if n != 0 {
                s.push(',');
            }
            write!(s, "{{\"reason\": {}, \"count\": {count}}}", quoted(reason)).unwrap();
        }
        s.push_str("]}\n");
        s
    }
}
