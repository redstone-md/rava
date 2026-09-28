//! Structural proof for method-confined primitive arrays.
//!
//! The proof is deliberately independent of class and method identity. A candidate is keyed only
//! by its `newarray` bytecode index and primitive `atype`, and every unsupported control-flow or
//! stack shape refuses it.

use rava_classfile::Code;

use crate::jit::{proves_confined, ConfinedArrayKind, ConfinedFlow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArenaPrimArraySite {
    pub(crate) pc: usize,
    pub(crate) atype: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArenaPrimArrayPlan {
    sites: Vec<ArenaPrimArraySite>,
}

impl ArenaPrimArrayPlan {
    pub(crate) fn sites(&self) -> &[ArenaPrimArraySite] {
        &self.sites
    }

    pub(crate) fn contains(&self, pc: usize, atype: u8) -> bool {
        self.sites
            .binary_search_by_key(&(pc, atype), |s| (s.pc, s.atype))
            .is_ok()
    }
}

pub(crate) fn analyze_arena_primitive_arrays(code: &Code) -> ArenaPrimArrayPlan {
    if !code.exceptions.is_empty() {
        return ArenaPrimArrayPlan::default();
    }
    let bc = &code.bytecode;
    let Some(flow) = ConfinedFlow::decode(bc) else {
        return ArenaPrimArrayPlan::default();
    };
    let mut candidates = Vec::new();
    for &pc in flow.pcs() {
        if bc[pc] == 0xbc {
            let Some(&atype @ 4..=11) = bc.get(pc + 1) else {
                return ArenaPrimArrayPlan::default();
            };
            candidates.push(ArenaPrimArraySite { pc, atype });
        }
    }
    let sites = candidates
        .into_iter()
        .filter(|site| {
            proves_confined(
                code,
                &flow,
                site.pc,
                ConfinedArrayKind::Primitive(site.atype),
            )
        })
        .collect();
    ArenaPrimArrayPlan { sites }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rava_classfile::ExceptionHandler;

    fn code(bytecode: Vec<u8>, locals: u16) -> Code {
        Code {
            max_stack: 8,
            max_locals: locals,
            bytecode,
            ..Code::default()
        }
    }

    fn refuses(body: &Code) {
        assert!(analyze_arena_primitive_arrays(body).sites().is_empty());
    }

    #[test]
    fn accepts_matching_load_store_for_all_atypes() {
        let cases = [
            (4, 0x33, 0x54, 0x03, 0xac),
            (5, 0x34, 0x55, 0x03, 0xac),
            (6, 0x30, 0x51, 0x0b, 0xae),
            (7, 0x31, 0x52, 0x0e, 0xaf),
            (8, 0x33, 0x54, 0x03, 0xac),
            (9, 0x35, 0x56, 0x03, 0xac),
            (10, 0x2e, 0x4f, 0x03, 0xac),
            (11, 0x2f, 0x50, 0x09, 0xad),
        ];
        for (atype, load, store, value, ret) in cases {
            let body = code(
                vec![
                    0x04, 0xbc, atype, 0x4b, 0x2a, 0x03, value, store, 0x2a, 0x03, load, ret,
                ],
                1,
            );
            let plan = analyze_arena_primitive_arrays(&body);
            assert!(plan.contains(1, atype), "atype {atype}");
        }
    }

    #[test]
    fn accepts_loop_carried_local() {
        let body = code(
            vec![
                0x04, 0xbc, 10, 0x4b, 0x03, 0x3c, 0x2a, 0x1b, 0x2e, 0x57, 0x84, 0x01, 0x01, 0x1b,
                0x04, 0xa1, 0xff, 0xf7, 0xb1,
            ],
            2,
        );
        assert!(analyze_arena_primitive_arrays(&body).contains(1, 10));
    }

    #[test]
    fn accepts_arraylength_on_candidate() {
        let body = code(vec![0x04, 0xbc, 10, 0xbe, 0xac], 0);
        assert!(analyze_arena_primitive_arrays(&body).contains(1, 10));
    }

    #[test]
    fn rejects_mismatched_array_opcode() {
        let mismatch = code(vec![0x04, 0xbc, 10, 0x03, 0x2f, 0xad], 0);
        refuses(&mismatch);
    }

    #[test]
    fn rejects_return_escape() {
        let escape = code(vec![0x04, 0xbc, 10, 0xb0], 0);
        refuses(&escape);
    }

    #[test]
    fn rejects_call_while_live() {
        let call = code(vec![0x04, 0xbc, 10, 0x4b, 0xb8, 0x00, 0x01, 0xb1], 1);
        refuses(&call);
    }

    #[test]
    fn rejects_deopt_capable_ops_while_live() {
        let cases = [
            (
                "unrelated aaload",
                vec![0x04, 0xbc, 10, 0x4b, 0x2b, 0x03, 0x32, 0x57, 0xb1],
                2,
            ),
            ("ldc", vec![0x04, 0xbc, 10, 0x4b, 0x12, 0x01, 0x57, 0xb1], 1),
            (
                "checkcast",
                vec![0x04, 0xbc, 10, 0x4b, 0x01, 0xc0, 0x00, 0x01, 0x57, 0xb1],
                1,
            ),
            (
                "idiv",
                vec![0x04, 0xbc, 10, 0x4b, 0x04, 0x04, 0x6c, 0x57, 0xb1],
                1,
            ),
            ("athrow", vec![0x04, 0xbc, 10, 0x4b, 0x01, 0xbf], 1),
        ];
        for (name, bytecode, locals) in cases {
            let body = code(bytecode, locals);
            assert!(
                analyze_arena_primitive_arrays(&body).sites().is_empty(),
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_exception_table() {
        let mut except = code(vec![0x04, 0xbc, 10, 0x57, 0xb1], 0);
        except.exceptions.push(ExceptionHandler {
            start_pc: 0,
            end_pc: 4,
            handler_pc: 4,
            catch_type: 0,
        });
        refuses(&except);
    }

    #[test]
    fn rejects_ambiguous_merge() {
        let merge = code(
            vec![
                0x04, 0xbc, 10, 0x4b, 0x03, 0x99, 0x00, 0x08, 0x2a, 0x4c, 0xa7, 0x00, 0x05, 0x01,
                0x4c, 0x2b, 0xbe, 0xac,
            ],
            2,
        );
        refuses(&merge);
    }
}
