//! Basic blocks and conservative control flow over validated machine-level IR.
use crate::{Result, emit::asm_operand, ir::Program};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Repeat,
    Trap,
    Fallthrough,
    Taken,
    Jump,
    Call,
    Return,
}
impl EdgeKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::Trap => "trap",
            Self::Fallthrough => "fallthrough",
            Self::Taken => "taken",
            Self::Jump => "jump",
            Self::Call => "call",
            Self::Return => "return",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Trap,
    Internal,
    External,
    Interior,
    Indirect,
    Return,
}
impl Resolution {
    pub fn name(self) -> &'static str {
        match self {
            Self::Trap => "trap",
            Self::Internal => "internal",
            Self::External => "external",
            Self::Interior => "instruction-interior",
            Self::Indirect => "indirect",
            Self::Return => "return",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: u64,
    pub site: u64,
    pub target: Option<u64>,
    pub kind: EdgeKind,
    pub resolution: Resolution,
    /// Destination recovered from value analysis rather than a direct operand.
    pub inferred: bool,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub start: u64,
    /// Exclusive end, in original virtual addresses.
    pub end: u64,
    pub first_instruction: usize,
    pub instruction_count: usize,
    pub reachable: bool,
}

#[derive(Debug)]
pub struct Analysis<'a> {
    pub program: &'a Program,
    pub blocks: Vec<Block>,
    pub edges: Vec<Edge>,
    /// Candidates: the entry and internal direct-call destinations.
    pub function_entries: Vec<u64>,
    /// Cyclic strongly connected components, excluding call edges.
    pub loops: Vec<Vec<u64>>,
    pub values: crate::dataflow::ValueAnalysis<'a>,
}

pub fn analyze(program: &Program) -> Result<Analysis<'_>> {
    program.validate()?;
    let values = crate::dataflow::analyze(program)?;
    let instructions = &program.instructions;
    let addresses: BTreeMap<_, _> = instructions
        .iter()
        .enumerate()
        .map(|(n, i)| (i.address, n))
        .collect();
    let mut leaders = BTreeSet::from([0, addresses[&program.entry]]);
    let mut functions = BTreeSet::from([program.entry]);
    for (n, i) in instructions.iter().enumerate() {
        if i.op.starts_with("rep.") {
            leaders.insert(n);
        }
        if let Some(target) = i.direct_target().or_else(|| values.target(n))
            && let Some(&index) = addresses.get(&target)
        {
            leaders.insert(index);
            if i.mnemonic() == "call" {
                functions.insert(target);
            }
        }
        if n + 1 < instructions.len()
            && (i.is_control_flow() || i.address + i.len as u64 != instructions[n + 1].address)
        {
            leaders.insert(n + 1);
        }
    }
    let leaders: Vec<_> = leaders.into_iter().chain([instructions.len()]).collect();
    let mut blocks: Vec<_> = leaders
        .windows(2)
        .map(|w| {
            let last = &instructions[w[1] - 1];
            Block {
                start: instructions[w[0]].address,
                end: last.address + last.len as u64,
                first_instruction: w[0],
                instruction_count: w[1] - w[0],
                reachable: false,
            }
        })
        .collect();
    let block_indices: BTreeMap<_, _> = blocks
        .iter()
        .enumerate()
        .map(|(n, b)| (b.start, n))
        .collect();
    let mut edges = Vec::new();
    for b in &blocks {
        let last = &instructions[b.first_instruction + b.instruction_count - 1];
        let target = last
            .direct_target()
            .or_else(|| values.target(b.first_instruction + b.instruction_count - 1));
        let mut edge = |kind, target| {
            let resolution = if kind == EdgeKind::Trap {
                Resolution::Trap
            } else if kind == EdgeKind::Return {
                Resolution::Return
            } else if let Some(target) = target {
                if addresses.contains_key(&target) {
                    Resolution::Internal
                } else if addresses
                    .range(..target)
                    .next_back()
                    .is_some_and(|(_, &n)| {
                        instructions[n].address + instructions[n].len as u64 > target
                    })
                {
                    Resolution::Interior
                } else {
                    Resolution::External
                }
            } else {
                Resolution::Indirect
            };
            edges.push(Edge {
                from: b.start,
                site: last.address,
                target,
                kind,
                resolution,
                inferred: target.is_some()
                    && last.direct_target().is_none()
                    && matches!(kind, EdgeKind::Call | EdgeKind::Jump),
            });
        };
        match last.mnemonic() {
            "rep.movs" | "rep.stos" => edge(EdgeKind::Repeat, Some(last.address)),
            "ud2" | "int3" | "hlt" => edge(EdgeKind::Trap, None),
            "ret" => edge(EdgeKind::Return, None),
            "jmp" => edge(EdgeKind::Jump, target),
            "call" => edge(EdgeKind::Call, target),
            _ if last.is_conditional() => edge(EdgeKind::Taken, last.direct_target()),
            _ => {}
        }
        if last.falls_through() {
            edge(EdgeKind::Fallthrough, Some(b.end));
        }
    }
    let mut adjacency = vec![Vec::new(); blocks.len()];
    let mut local = adjacency.clone();
    for e in &edges {
        if e.resolution == Resolution::Internal {
            let from = block_indices[&e.from];
            let to = block_indices[&e.target.unwrap()];
            adjacency[from].push(to);
            if e.kind != EdgeKind::Call {
                local[from].push(to);
            }
        }
    }
    let mut pending = vec![block_indices[&program.entry]];
    while let Some(n) = pending.pop() {
        if blocks[n].reachable {
            continue;
        }
        blocks[n].reachable = true;
        pending.extend(&adjacency[n]);
    }
    let loops = cyclic_components(&local)
        .into_iter()
        .map(|c| c.into_iter().map(|n| blocks[n].start).collect())
        .collect();
    Ok(Analysis {
        program,
        blocks,
        edges,
        function_entries: functions.into_iter().collect(),
        loops,
        values,
    })
}

// Iterative Kosaraju: avoid overflowing the host stack on large or hostile IR.
fn cyclic_components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut seen = vec![false; graph.len()];
    let mut order = Vec::new();
    for root in 0..graph.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0)];
        while let Some((node, index)) = stack.last_mut() {
            if let Some(&next) = graph[*node].get(*index) {
                *index += 1;
                if !seen[next] {
                    seen[next] = true;
                    stack.push((next, 0));
                }
            } else {
                order.push(*node);
                stack.pop();
            }
        }
    }
    let mut reverse = vec![Vec::new(); graph.len()];
    for (n, edges) in graph.iter().enumerate() {
        for &next in edges {
            reverse[next].push(n);
        }
    }
    seen.fill(false);
    let mut result = Vec::new();
    for root in order.into_iter().rev() {
        if seen[root] {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![root];
        seen[root] = true;
        while let Some(n) = stack.pop() {
            component.push(n);
            for &next in &reverse[n] {
                if !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        if component.len() > 1 || graph[root].contains(&root) {
            component.sort_unstable();
            result.push(component);
        }
    }
    result.sort();
    result
}

pub(crate) fn quoted(s: &str) -> String {
    let mut result = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => write!(result, "\\u{:04x}", c as u32).unwrap(),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}
pub(crate) fn hex(address: u64) -> String {
    format!("\"0x{address:x}\"")
}
pub(crate) fn listing(i: &crate::ir::Instruction) -> String {
    if crate::strings::valid(i).is_some() {
        return crate::strings::assembly(i);
    }
    let operands = i
        .operands
        .iter()
        .map(asm_operand)
        .collect::<Vec<_>>()
        .join(", ");
    if operands.is_empty() {
        i.op.clone()
    } else {
        format!("{} {operands}", i.op)
    }
}

impl Analysis<'_> {
    pub fn summary(&self) -> String {
        format!(
            "entry: 0x{:x}\ninstructions: {}\ndecoded bytes: {}\nblocks: {}\nloop components: {}\nproven conditional branches: {}\nunresolved transfers: {}\nexplicit traps: {}\n",
            self.program.entry,
            self.program.instructions.len(),
            self.program
                .instructions
                .iter()
                .map(|i| i.len as usize)
                .sum::<usize>(),
            self.blocks.len(),
            self.loops.len(),
            (0..self.program.instructions.len())
                .filter(|&n| self.values.branch(n).is_some())
                .count(),
            self.edges
                .iter()
                .filter(|e| e.resolution == Resolution::Indirect)
                .count(),
            self.edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Trap)
                .count()
        )
    }
    /// Feasibility under the value model; None means no branch conclusion.
    pub fn edge_feasible(&self, edge: &Edge) -> Option<bool> {
        let index = self
            .program
            .instructions
            .binary_search_by_key(&edge.site, |i| i.address)
            .ok()?;
        let taken = self.values.branch(index)?;
        match edge.kind {
            EdgeKind::Taken => Some(taken),
            EdgeKind::Fallthrough => Some(!taken),
            _ => None,
        }
    }
    pub fn text(&self) -> String {
        let mut s = format!(
            "entry: 0x{:x}\ninstructions: {}\nblocks: {} ({} reachable)\nfunction candidates: {}\nloop components: {}\n",
            self.program.entry,
            self.program.instructions.len(),
            self.blocks.len(),
            self.blocks.iter().filter(|b| b.reachable).count(),
            self.function_entries.len(),
            self.loops.len()
        );
        let mut edges = self.edges.iter().peekable();
        for b in &self.blocks {
            writeln!(
                s,
                "\nblock 0x{:x}..0x{:x}{}",
                b.start,
                b.end,
                if b.reachable { "" } else { " [unreachable]" }
            )
            .unwrap();
            for (n, i) in self.program.instructions
                [b.first_instruction..b.first_instruction + b.instruction_count]
                .iter()
                .enumerate()
            {
                let note = self.values.annotation(b.first_instruction + n);
                writeln!(
                    s,
                    "  0x{:x}: {}{}",
                    i.address,
                    listing(i),
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!(" ; {note}")
                    }
                )
                .unwrap();
            }
            while let Some(e) = edges.next_if(|e| e.from == b.start) {
                writeln!(
                    s,
                    "  -> {} {} [{}]{}",
                    e.kind.name(),
                    e.target.map_or("?".into(), |t| format!("0x{t:x}")),
                    e.resolution.name(),
                    if self.edge_feasible(e) == Some(false) {
                        " [infeasible under value model]"
                    } else {
                        ""
                    }
                )
                .unwrap();
            }
        }
        s
    }

    /// Addresses are strings, preserving all 64 bits in JavaScript consumers.
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\n  \"schema\": \"rosemary-analysis-1\",\n  \"entry\": {},\n  \"instruction_count\": {},\n  \"function_entries\": [{}],\n  \"loops\": [{}],\n  \"blocks\": [",
            hex(self.program.entry),
            self.program.instructions.len(),
            self.function_entries
                .iter()
                .map(|&a| hex(a))
                .collect::<Vec<_>>()
                .join(", "),
            self.loops
                .iter()
                .map(|c| format!(
                    "[{}]",
                    c.iter().map(|&a| hex(a)).collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (n, b) in self.blocks.iter().enumerate() {
            if n != 0 {
                s.push(',');
            }
            write!(
                s,
                "\n    {{\"start\": {}, \"end\": {}, \"reachable\": {}, \"instructions\": [",
                hex(b.start),
                hex(b.end),
                b.reachable
            )
            .unwrap();
            for (n, i) in self.program.instructions
                [b.first_instruction..b.first_instruction + b.instruction_count]
                .iter()
                .enumerate()
            {
                if n != 0 {
                    s.push_str(", ");
                }
                write!(
                    s,
                    "{{\"address\": {}, \"size\": {}, \"op\": {}, \"assembly\": {}}}",
                    hex(i.address),
                    i.len,
                    quoted(&i.op),
                    quoted(&listing(i))
                )
                .unwrap();
            }
            s.push_str("]}");
        }
        s.push_str("\n  ],\n  \"edges\": [");
        for (n, e) in self.edges.iter().enumerate() {
            if n != 0 {
                s.push(',');
            }
            write!(s, "\n    {{\"from\": {}, \"site\": {}, \"target\": {}, \"kind\": {}, \"resolution\": {}, \"inferred\": {}, \"feasible\": {}}}", hex(e.from), hex(e.site), e.target.map_or("null".into(), hex), quoted(e.kind.name()), quoted(e.resolution.name()), e.inferred, self.edge_feasible(e).map_or("null", |v| if v { "true" } else { "false" })).unwrap();
        }
        write!(
            s,
            "\n  ],\n  \"value_analysis\": {}\n}}\n",
            self.values.json()
        )
        .unwrap();
        s
    }

    pub fn dot(&self) -> String {
        let mut s = String::from(
            "digraph rosemary {\n  graph [label=\"Rosemary control flow\", labelloc=t];\n  node [shape=box, fontname=monospace];\n",
        );
        for b in &self.blocks {
            let mut label = format!(
                "0x{:x}{}\n",
                b.start,
                if b.start == self.program.entry {
                    " (entry)"
                } else {
                    ""
                }
            );
            for i in &self.program.instructions
                [b.first_instruction..b.first_instruction + b.instruction_count]
            {
                writeln!(label, "0x{:x}: {}", i.address, listing(i)).unwrap();
            }
            writeln!(
                s,
                "  b_{:x} [label={}, style={}];",
                b.start,
                quoted(&label),
                if b.reachable { "solid" } else { "dashed" }
            )
            .unwrap();
        }
        for (n, e) in self.edges.iter().enumerate() {
            let target = if e.resolution == Resolution::Internal {
                format!("b_{:x}", e.target.unwrap())
            } else {
                let label = format!(
                    "{}{}",
                    e.target.map_or(String::new(), |t| format!("0x{t:x} ")),
                    e.resolution.name()
                );
                writeln!(s, "  exit_{n} [label={}, shape=ellipse];", quoted(&label)).unwrap();
                format!("exit_{n}")
            };
            let infeasible = self.edge_feasible(e) == Some(false);
            writeln!(
                s,
                "  b_{:x} -> {target} [label={}, style={}, color={}];",
                e.from,
                quoted(&format!(
                    "{}{}",
                    e.kind.name(),
                    if infeasible { " (infeasible)" } else { "" }
                )),
                if infeasible {
                    "dotted"
                } else if e.kind == EdgeKind::Call {
                    "dashed"
                } else {
                    "solid"
                },
                if infeasible { "gray" } else { "black" }
            )
            .unwrap();
        }
        s.push_str("}\n");
        s
    }
}
