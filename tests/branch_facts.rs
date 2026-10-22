use rosemary::{dataflow, ir::Program};

fn p(lines: &str) -> Program {
    Program::from_text(&format!("rosemary-ir 1\nentry 0\n{lines}")).unwrap()
}

#[test]
fn taken_and_fallthrough_edges_refine_simple_flag_conditions() {
    for op in [
        "jo", "jno", "jb", "jae", "je", "jne", "js", "jns", "jp", "jnp",
    ] {
        let program = p(&format!(
            "0 1 {op} i:64:4\n1 1 {op} i:64:3\n2 1 ret\n3 1 ret\n4 1 {op} i:64:6\n5 1 ret\n6 1 ret\n"
        ));
        let a = dataflow::analyze(&program).unwrap();
        assert_eq!(a.branch(0), None);
        assert_eq!(a.branch(1), Some(false), "{op}");
        assert_eq!(a.branch(4), Some(true), "{op}");
        assert!(a.states[3].is_none() && a.states[5].is_none());
        let simplified = a.simplify().unwrap();
        assert_eq!(simplified.instructions[1].op, "nop");
        assert_eq!(simplified.instructions[4].op, "jmp");
    }
    let program = Program::from_text(include_str!("../examples/path_facts.ir")).unwrap();
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.branch(1), Some(true));
    assert_eq!(a.branch(6), Some(false));
    assert!(a.states[2].is_none() && a.states[8].is_none());
}

#[test]
fn compound_predicates_keep_common_facts_without_inventing_correlations() {
    let program = p("0 1 ja i:64:3\n1 1 ret\n3 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    let taken = a.states[2].unwrap().flags;
    assert_eq!(taken.cf, Some(false));
    assert_eq!(taken.zf, Some(false));
    assert_eq!(a.states[1].unwrap().flags.cf, None);
    assert_eq!(a.states[1].unwrap().flags.zf, None);
    let program = p("0 1 clc\n1 1 jbe i:64:4\n2 1 ret\n4 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[2].unwrap().flags.zf, Some(false));
    assert_eq!(a.states[3].unwrap().flags.zf, Some(true));
    let program = p("0 1 js i:64:3\n1 1 ret\n3 1 jle i:64:6\n4 1 ret\n6 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    let fallthrough = a.states[3].unwrap().flags;
    assert_eq!(fallthrough.sf, Some(true));
    assert_eq!(fallthrough.of, Some(true));
    assert_eq!(fallthrough.zf, Some(false));
    let program = p("0 1 jl i:64:3\n1 1 ret\n3 1 jge i:64:5\n4 1 ret\n5 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[2].unwrap().flags.sf, None);
    assert_eq!(a.states[2].unwrap().flags.of, None);
    assert_eq!(a.branch(2), None);
}

#[test]
fn counter_edges_refine_zero_and_the_required_zf_value() {
    let program = p("0 1 jrcxz i:64:4\n1 1 ret\n4 1 mov r:64:0 r:64:1\n5 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[1].unwrap().registers[1], None);
    assert_eq!(a.states[2].unwrap().registers[1], Some(0));
    assert_eq!(a.states[3].unwrap().registers[0], Some(0));
    let program = p("0 1 loop i:64:3\n1 1 ret\n3 1 ret\n");
    assert_eq!(
        dataflow::analyze(&program).unwrap().states[1]
            .unwrap()
            .registers[1],
        Some(0)
    );
    for (op, want_zf, guard) in [("loope", true, "je"), ("loopne", false, "jne")] {
        let program = p(&format!(
            "0 1 mov r:64:1 i:64:2\n1 1 {op} i:64:4\n2 1 ret\n4 1 ret\n"
        ));
        let a = dataflow::analyze(&program).unwrap();
        assert_eq!(a.states[2].unwrap().flags.zf, Some(!want_zf));
        assert_eq!(a.states[3].unwrap().flags.zf, Some(want_zf));
        assert_eq!(a.states[2].unwrap().registers[1], Some(1));
        let program = p(&format!(
            "0 1 {guard} i:64:3\n1 1 ret\n3 1 {op} i:64:6\n4 1 ret\n6 1 ret\n"
        ));
        let a = dataflow::analyze(&program).unwrap();
        assert_eq!(a.states[3].unwrap().registers[1], Some(0));
        assert_eq!(a.simplify().unwrap().instructions[2].op, op);
    }
}

#[test]
fn merged_paths_calls_and_unknown_transfers_invalidate_edge_facts() {
    let program = p("0 1 je i:64:3\n1 1 jmp i:64:4\n3 1 nop\n4 1 jne i:64:6\n5 1 ret\n6 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[3].unwrap().flags.zf, None);
    assert_eq!(a.branch(3), None);
    let program = p("0 1 je i:64:4\n1 1 jne i:64:3\n2 1 ret\n3 1 ret\n4 1 jmp r:64:0\n");
    let a = dataflow::analyze(&program).unwrap();
    assert!(a.conservative_indirect);
    assert_eq!(a.branch(1), None);
    assert_eq!(a.simplify().unwrap(), program);
    let program =
        p("0 1 je i:64:3\n1 1 ret\n3 1 call i:64:8\n4 1 jne i:64:6\n5 1 ret\n6 1 ret\n8 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.branch(3), None);
    assert_eq!(a.states[6].unwrap().flags.zf, None);
}
