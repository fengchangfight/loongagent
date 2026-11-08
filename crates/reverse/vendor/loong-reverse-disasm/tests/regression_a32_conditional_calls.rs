use loong_reverse_disasm::dataflow::{DefUse, def_use};
use loong_reverse_disasm::{Cond, Flow, Image, Limits, Mode, NoControl, Program, decode};
use loong_reverse_formats::{Arch, Binary, Symbol, SymbolKind};
use std::collections::BTreeSet;
use std::sync::Arc;

#[test]
fn regression_a32_calls_keep_conditions_without_changing_call_flow() {
    let cases = [
        (
            0x1b00_0004u32,
            Some(Cond::Ne),
            Flow::Call { target: 0x1020 },
        ),
        (0x0b00_0004, Some(Cond::Eq), Flow::Call { target: 0x1020 }),
        (0xeb00_0004, None, Flow::Call { target: 0x1020 }),
        (
            0x112f_ff33,
            Some(Cond::Ne),
            Flow::IndirectCall { slot: None },
        ),
        (
            0x012f_ff33,
            Some(Cond::Eq),
            Flow::IndirectCall { slot: None },
        ),
        (0xe12f_ff33, None, Flow::IndirectCall { slot: None }),
        // The unconditional BLX immediate encoding uses the otherwise reserved condition bits.
        (0xfaff_fffe, None, Flow::Call { target: 0x1008 }),
    ];
    for (word, condition, flow) in cases {
        for formatted in [false, true] {
            let insn = decode(Mode::A32, &word.to_le_bytes(), 0x1008, formatted).unwrap();
            assert_eq!(insn.flow, flow, "{word:#010x}, formatted={formatted}");
            assert_eq!(insn.cond, condition, "{word:#010x}, formatted={formatted}");
        }
    }
}

fn program_after_call(call: u32, split_block: bool, noreturn: bool) -> (Image, Program) {
    let words = [
        0xe92d_4010, // push {r4, lr}: preserve the caller's return address
        0xe3a0_0007, // mov r0, #7
        call,
        // A branch to the next instruction makes reaching definitions cross a block boundary.
        if split_block {
            0xeaff_ffff
        } else {
            0xe1a0_4004
        }, // b 0x1010 / mov r4, r4
        0xe8bd_8010, // pop {r4, pc}
        0,
        0,
        0,
        if noreturn { 0xe120_0070 } else { 0xe12f_ff1e }, // bkpt / bx lr at the direct-call target
    ];
    let bytes: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let mut bin = Binary::raw(Arch::Arm, 0x1000, bytes.len() as u64, 0);
    if noreturn {
        bin.symbols.push(Symbol {
            name: "abort".into(),
            demangled: None,
            addr: 0x1020,
            size: 4,
            kind: SymbolKind::Function,
            global: true,
            weak: false,
            defined: true,
            dynamic: false,
        });
    }
    let img = Image::from_binary(Arc::from(bytes), bin);
    let prog = Program::analyze(&img, &Limits::default(), &NoControl).unwrap();
    (img, prog)
}

fn flow_after_call(call: u32, split_block: bool) -> DefUse {
    let (img, prog) = program_after_call(call, split_block, false);
    let function = prog.functions.get(&0x1000).unwrap();
    assert!(function.returns && !function.truncated);
    assert_eq!(function.calls.len(), 1);
    assert_eq!(function.blocks.len(), if split_block { 2 } else { 1 });
    let flow = def_use(&img, &prog, function, 5000, 10000);
    assert!(!flow.truncated);
    flow
}

fn definitions_reaching_return(flow: &DefUse) -> BTreeSet<u64> {
    let ret = flow
        .nodes
        .iter()
        .find(|node| node.addr == Some(0x1010))
        .unwrap();
    flow.edges
        .iter()
        .filter(|edge| edge.to == ret.id && edge.register == "r0")
        .map(|edge| flow.nodes.get(edge.from).unwrap().addr.unwrap())
        .collect()
}

#[test]
fn regression_a32_conditional_calls_keep_the_skipped_result() {
    // Both BLNE and BLXNE r3 can leave the preceding r0 definition intact.
    for call in [0x1b00_0004, 0x112f_ff33] {
        for split_block in [false, true] {
            let flow = flow_after_call(call, split_block);
            assert_eq!(
                definitions_reaching_return(&flow),
                BTreeSet::from([0x1004, 0x1008]),
                "call={call:#010x}, split_block={split_block}: {flow:?}"
            );
        }
    }
}

#[test]
fn regression_a32_unconditional_calls_replace_the_previous_result() {
    for call in [0xeb00_0004, 0xe12f_ff33] {
        for split_block in [false, true] {
            let flow = flow_after_call(call, split_block);
            assert_eq!(
                definitions_reaching_return(&flow),
                BTreeSet::from([0x1008]),
                "call={call:#010x}, split_block={split_block}: {flow:?}"
            );
        }
    }
}

#[test]
fn regression_a32_conditional_noreturn_calls_keep_only_the_skipped_result() {
    for call in [0x1b00_0004, 0x0b00_0004] {
        for split_block in [false, true] {
            let (img, prog) = program_after_call(call, split_block, true);
            assert!(prog.function_by_name("abort").is_some());
            let function = prog.functions.get(&0x1000).unwrap();
            assert_eq!(function.calls.len(), 1);
            assert!(
                function.returns && function.contains(0x1010),
                "a skipped abort must reach the return: {function:?}"
            );
            let flow = def_use(&img, &prog, function, 5000, 10000);
            assert!(!flow.truncated);
            assert_eq!(
                definitions_reaching_return(&flow),
                BTreeSet::from([0x1004]),
                "abort cannot supply a returned value: {flow:?}"
            );
        }
    }
}

#[test]
fn regression_a32_unconditional_noreturn_calls_still_stop() {
    for split_block in [false, true] {
        let (img, prog) = program_after_call(0xeb00_0004, split_block, true);
        let function = prog.functions.get(&0x1000).unwrap();
        assert_eq!(function.calls.len(), 1);
        assert!(
            !function.returns && !function.contains(0x1010),
            "an unconditional abort must stop: {function:?}"
        );
        let flow = def_use(&img, &prog, function, 5000, 10000);
        assert!(!flow.truncated);
        assert!(flow.nodes.iter().all(|node| node.addr != Some(0x1010)));
    }
}
