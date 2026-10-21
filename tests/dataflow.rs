use rosemary::{
    analysis,
    dataflow::{self, Flags},
    ir::{Operand, Program},
};

fn p(lines: &str) -> Program {
    Program::from_text(&format!("rosemary-ir 1\nentry 0\n{lines}")).unwrap()
}
#[test]
fn constants_prove_branches_and_simplify_without_changing_layout() {
    let program = p(
        "0 1 xor r:32:0 r:32:0\n1 1 jne i:64:5\n2 1 mov r:32:0 i:32:6\n3 1 imul r:32:0 r:32:0 i:32:7\n4 1 ret\n5 1 mov r:32:0 i:32:ff\n6 1 ret\n",
    );
    let a = analysis::analyze(&program).unwrap();
    assert_eq!(a.values.branch(1), Some(false));
    assert_eq!(a.values.states[4].unwrap().registers[0], Some(42));
    assert!(a.values.states[5].is_none());
    assert!(
        a.values
            .text()
            .contains("branch always falls through (ZF=1")
    );
    assert!(a.values.text().contains("rax at return = 0x2a"));
    assert!(a.edges.iter().any(|e| a.edge_feasible(e) == Some(false)));
    assert!(a.dot().contains("(infeasible)"));
    let simplified = a.values.simplify().unwrap();
    assert_eq!(simplified.instructions[1].op, "nop");
    assert!(simplified.instructions[1].operands.is_empty());
    assert_eq!(
        program
            .instructions
            .iter()
            .map(|i| (i.address, i.len))
            .collect::<Vec<_>>(),
        simplified
            .instructions
            .iter()
            .map(|i| (i.address, i.len))
            .collect::<Vec<_>>()
    );
    let program = p("0 1 sub r:64:0 r:64:0\n1 1 je i:64:3\n2 1 ret\n3 1 ret\n");
    assert_eq!(
        dataflow::analyze(&program)
            .unwrap()
            .simplify()
            .unwrap()
            .instructions[1]
            .op,
        "jmp"
    );
}
#[test]
fn merge_points_keep_only_constants_agreed_on_by_every_path() {
    for (other, expected) in [("2a", Some(42)), ("2b", None)] {
        let program = p(&format!(
            "0 1 test r:64:7 r:64:7\n1 1 je i:64:5\n2 1 mov r:32:0 i:32:2a\n3 1 jmp i:64:7\n5 1 mov r:32:0 i:32:{other}\n6 1 nop\n7 1 ret\n"
        ));
        let a = dataflow::analyze(&program).unwrap();
        assert_eq!(a.states[6].unwrap().registers[0], expected);
        assert_eq!(a.branch(1), None);
    }
}
#[test]
fn loops_reach_a_fixed_point_and_retract_early_branch_conclusions() {
    let program = p("0 1 mov r:32:0 i:32:0\n1 1 add r:32:0 i:32:1\n2 1 jne i:64:1\n3 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.branch(2), None);
    assert!(a.states[3].is_some());
    assert_eq!(a.states[3].unwrap().registers[0], None);
    assert!(a.iterations < 100);
    assert_eq!(a.simplify().unwrap(), program);
}
#[test]
fn known_indirect_targets_feed_back_into_cfg_and_simplification() {
    let program = p(
        "0 1 mov r:64:0 i:64:4\n1 1 jmp r:64:0\n2 1 mov r:64:3 i:64:5\n3 1 ret\n4 1 xor r:32:3 r:32:3\n5 1 ret\n",
    );
    let a = analysis::analyze(&program).unwrap();
    assert!(!a.values.conservative_indirect);
    assert_eq!(a.values.target(1), Some(4));
    assert!(a.edges.iter().any(|e| e.target == Some(4) && e.inferred));
    assert!(a.blocks.iter().find(|b| b.start == 4).unwrap().reachable);
    assert!(a.values.states[2].is_none());
    assert_eq!(
        a.values.simplify().unwrap().instructions[1].operands,
        [Operand::Imm(4, 64)]
    );
}
#[test]
fn unresolved_indirect_transfers_invalidate_facts_even_inside_blocks() {
    let program = p(
        "0 1 jmp r:64:0\n1 1 mov r:32:0 i:32:2a\n2 1 cmp r:32:0 i:32:2a\n3 1 je i:64:5\n4 1 ret\n5 1 ret\n",
    );
    let a = dataflow::analyze(&program).unwrap();
    assert!(a.conservative_indirect);
    assert!(a.states.iter().all(Option::is_some));
    assert_eq!(a.states[2].unwrap().registers[0], None);
    assert_eq!(a.branch(3), None);
    assert_eq!(a.simplify().unwrap(), program);
}
#[test]
fn calls_memory_aliases_and_undefined_flags_do_not_produce_false_certainty() {
    let program = p(
        "0 1 mov r:32:0 i:32:2a\n1 1 call i:64:5\n2 1 cmp r:32:0 i:32:2a\n3 1 je i:64:6\n4 1 ret\n5 1 ret\n6 1 ret\n",
    );
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[2].unwrap().registers[0], None);
    assert_eq!(a.states[5].unwrap().registers[0], None);
    assert_eq!(a.branch(3), None);
    let program = p(
        "0 1 mov r:64:3 i:64:80\n1 1 mov m:64:3:-:1:0 i:64:2a\n2 1 mov r:64:0 m:64:3:-:1:0\n3 1 ret\n",
    );
    let a = dataflow::analyze(&program).unwrap();
    assert!(a.annotation(2).contains("effective address = 0x80"));
    assert_eq!(a.states[3].unwrap().registers[0], None);
    let program =
        p("0 1 xor r:32:0 r:32:0\n1 1 imul r:32:0 r:32:1\n2 1 je i:64:4\n3 1 ret\n4 1 ret\n");
    let a = dataflow::analyze(&program).unwrap();
    assert_eq!(a.states[2].unwrap().registers[0], Some(0));
    assert_eq!(a.states[2].unwrap().flags.of, Some(false));
    assert_eq!(a.branch(2), None);
}
#[test]
fn zero_extension_sign_extension_addresses_and_carry_preservation() {
    let program = p(
        "0 1 mov r:64:0 i:64:ffffffffffffffff\n1 1 add r:64:0 i:64:1\n2 1 inc r:32:0\n3 1 mov r:32:1 i:32:ffffffff\n4 1 movsxd r:64:1 r:32:1\n5 1 lea r:64:2 m:64:1:0:8:1\n6 1 ret\n",
    );
    let a = dataflow::analyze(&program).unwrap();
    let final_state = a.states[6].unwrap();
    assert_eq!(
        final_state.registers[..3],
        [Some(1), Some(u64::MAX), Some(8)]
    );
    assert_eq!(final_state.flags.cf, Some(true));
}
#[test]
fn all_condition_codes_agree_with_independent_boolean_definitions() {
    for bits in 0..32 {
        let [zf, sf, cf, of, pf] = std::array::from_fn(|n| bits & (1 << n) != 0);
        let f = Flags {
            zf: Some(zf),
            sf: Some(sf),
            cf: Some(cf),
            of: Some(of),
            pf: Some(pf),
        };
        let expected = [
            of,
            !of,
            cf,
            !cf,
            zf,
            !zf,
            cf || zf,
            !cf && !zf,
            sf,
            !sf,
            pf,
            !pf,
            sf != of,
            sf == of,
            zf || sf != of,
            !zf && sf == of,
        ];
        for (op, value) in rosemary::ir::CONDITIONS.iter().zip(expected) {
            assert_eq!(f.condition(op), Some(value), "{op}, {bits}");
        }
    }
    let partial = Flags {
        zf: Some(true),
        ..Default::default()
    };
    assert_eq!(partial.condition("jbe"), Some(true));
    assert_eq!(partial.condition("jg"), Some(false));
    assert_eq!(partial.condition("jl"), None);
}
