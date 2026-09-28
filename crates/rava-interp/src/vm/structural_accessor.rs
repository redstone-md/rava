//! Structural resolution for the arena accessor linkage table.

use crate::*;

impl Vm {
    /// Resolve and publish one complete `(selector, concrete arena class)` key.
    /// The result is immutable JVM linkage plus exact bytecode and layout facts.
    pub(crate) fn link_structural_accessor(&mut self, class_id: u32, name: &str, desc: &str) {
        use crate::jit::structural_accessor as sa;

        let Some(selector) = sa::selector_id(name, desc) else {
            return;
        };
        if sa::contains(class_id, selector) {
            return;
        }
        let Some(class_name) = crate::nextgen::class_name(class_id) else {
            return;
        };
        let Ok(Some(owner)) = self.resolve_method(&class_name, name, desc) else {
            return;
        };
        let Some(method) = owner.file.method(name, desc) else {
            return;
        };
        let Some(code) = method.code.as_ref() else {
            return;
        };
        // A `static` or `synchronized` body is not a receiver-shape this table answers;
        // `native`/`abstract` have no code and stopped at the line above.
        const ACC_STATIC: u16 = 0x0008;
        const ACC_SYNCHRONIZED: u16 = 0x0020;
        if method.access_flags & (ACC_STATIC | ACC_SYNCHRONIZED) != 0 {
            sa::publish(class_id, selector, sa::OP_REFUSED, None);
            return;
        }

        let recognized = if code.exceptions.is_empty() {
            recognize(self, class_id, &owner.file.pool, &code.bytecode, desc, &owner.file.this_class)
        } else {
            None
        };
        match recognized {
            Some((op, slot)) => sa::publish(class_id, selector, op, Some(slot)),
            None => sa::publish(class_id, selector, sa::OP_REFUSED, None),
        }
    }

    /// The decline-edge writer for the `size()` chained-shape round: link the receiver's
    /// `size()I` structural facts when the round is on, an ARENA receiver reached the
    /// terminal arena refusal, and nothing is published for the pair yet. Everything the
    /// table stores is a structural fact of `(class, selector)` linkage — no receiver
    /// history, no names — so `fu_kind`'s per-class memoisation does not gate it: this is
    /// called per callback and the `contains` probe is the steady-state cost (one acquire
    /// load), after which the pair is permanently answered.
    ///
    /// Called ONLY from the terminal `REFUSE_OBJ` decline — never `REFUSE_UNCACHED`,
    /// which is a transient "not yet" that must never be stored as a "never".
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn link_mg_size_structural(&mut self, recv: &crate::value::Value) {
        if !crate::jit::emit::collections::mg_fast_enabled() {
            return;
        }
        let crate::value::Value::Obj(p) = recv else { return };
        // The selector id is interned ONCE for the process (the emitter bakes the same id),
        // and the `contains` probe — one acquire load — is the whole steady-state cost after
        // the pair's first callback published it. Both keep this per-decline writer off the
        // hot path it shares with 7.9 M refusals a boot.
        static SIZE_I: std::sync::LazyLock<Option<u16>> = std::sync::LazyLock::new(|| {
            crate::jit::structural_accessor::selector_id("size", "()I")
        });
        let Some(selector) = *SIZE_I else { return };
        let class_id = crate::nextgen::arena_class_id(*p);
        if crate::jit::structural_accessor::contains(class_id, selector) {
            return;
        }
        self.link_structural_accessor(class_id, "size", "()I");
    }
}

fn field_slot(
    vm: &Vm,
    class_id: u32,
    pool: &rava_classfile::ConstantPool,
    hi: u8,
    lo: u8,
) -> Option<u32> {
    let member = pool.member_ref(u16::from_be_bytes([hi, lo]))?;
    if member.descriptor != "I" {
        return None;
    }
    let fid = vm.resolve_field_id(member.owner, member.name);
    crate::nextgen::slot_of(class_id, fid).map(|s| s as u32)
}

/// Exact straight-line patterns. The selector name is absent by design; descriptor,
/// return opcode, resolved field and frozen class layout are all rechecked.
fn recognize(
    vm: &Vm,
    class_id: u32,
    pool: &rava_classfile::ConstantPool,
    bc: &[u8],
    desc: &str,
    this_class: &str,
) -> Option<(u8, u32)> {
    use crate::jit::structural_accessor as sa;
    match (desc, bc) {
        ("()I", [0x2a, 0xb4, hi, lo, 0x04, 0x60, 0xac]) => Some((
            sa::OP_INT_FIELD_ADD_ONE,
            field_slot(vm, class_id, pool, *hi, *lo)?,
        )),
        ("(I)D", [0x1b, 0x87, 0x2a, 0xb4, hi, lo, 0x87, 0x6f, 0xaf]) => Some((
            sa::OP_INT_ARG_DIV_FIELD,
            field_slot(vm, class_id, pool, *hi, *lo)?,
        )),
        // The plain `()I` getter — `aload_0; getfield f; ireturn` — answered by ONE arena
        // cell load. The Fieldref's owner must be the DEFINING class itself and the field
        // int-family (`I`/`Z`/`B`/`C`/`S`): every one of those is layout kind 0, whose cell
        // is the sign-extended i32 both the interpreter (`decode_arena_cell`) and the
        // compiled `getfield` (`cell_to_sv`) read identically, so one `ireduce` is exact.
        ("()I", [0x2a, 0xb4, hi, lo, 0xac]) => {
            let member = pool.member_ref(u16::from_be_bytes([*hi, *lo]))?;
            if member.owner != this_class
                || !matches!(
                    member.descriptor.as_bytes().first(),
                    Some(b'I' | b'Z' | b'B' | b'C' | b'S')
                )
            {
                return None;
            }
            let fid = vm.resolve_field_id(member.owner, member.name);
            Some((sa::OP_INT_FIELD, crate::nextgen::slot_of(class_id, fid)? as u32))
        }
        // `()I` literals — `iconst_n`/`bipush`/`sipush; ireturn`. The value rides the
        // 12-bit slot word as `v + 1` (the entry format's own bias, added by `publish`),
        // so it must be non-negative and leave room for that +1. Out-of-range and negative
        // literals answer `None` here, which LINKS as `OP_REFUSED` — a permanent refusal,
        // never a silent no-publish that would re-resolve per callback, and never a wrong
        // answer wrapped into 12 bits.
        ("()I", [op @ 0x03..=0x08, 0xac]) => {
            const_of(i32::from(*op) - 0x03, sa::OP_INT_CONST)
        }
        ("()I", [0x10, b, 0xac]) => const_of(i32::from(*b as i8), sa::OP_INT_CONST),
        ("()I", [0x11, hi, lo, 0xac]) => {
            const_of(i32::from(i16::from_be_bytes([*hi, *lo])), sa::OP_INT_CONST)
        }
        _ => None,
    }
}

fn const_of(v: i32, op: u8) -> Option<(u8, u32)> {
    if (0..crate::jit::structural_accessor::SLOT_MASK as i32).contains(&v) {
        Some((op, v as u32))
    } else {
        None
    }
}

