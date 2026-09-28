//! Two input representations sharing one prepared-method local-slot layout.
use crate::{jit, Value, Vm};

pub(super) enum Arguments<'a> { Values(&'a [Value]), Raw(&'a [i64]) }

/// Immutable descriptor facts owned by one linked method, not a call-site history.
pub(crate) struct Layout {
    parameters: Box<[(bool, bool)]>, // (reference, category-2)
    return_start: usize,
}

impl Layout {
    pub(super) fn new(descriptor: &str, is_static: bool) -> Self {
        let mut parameters = Vec::new();
        if !is_static { parameters.push((true, false)); }
        let bytes = descriptor.as_bytes();
        let mut at = descriptor.find('(').map(|p| p + 1).unwrap_or(0);
        while at < bytes.len() && bytes[at] != b')' {
            let array = bytes[at] == b'[';
            while at < bytes.len() && bytes[at] == b'[' { at += 1; }
            let kind = bytes.get(at).copied().unwrap_or(b')');
            let wide = !array && matches!(kind, b'J' | b'D');
            parameters.push((array || kind == b'L', wide));
            if kind == b'L' { while at < bytes.len() && bytes[at] != b';' { at += 1; } }
            at += 1;
        }
        Self { parameters: parameters.into_boxed_slice(),
            return_start: descriptor.rfind(')').map(|p| p + 1).unwrap_or(descriptor.len()) }
    }

    pub(super) fn return_start(&self) -> usize { self.return_start }
}

impl Arguments<'_> {
    fn len(&self) -> usize {
        match self { Self::Values(v) => v.len(), Self::Raw(v) => v.len() }
    }

    fn cell(&self, vm: &Vm, index: usize, reference: bool) -> i64 {
        let cell = match self {
            Self::Values(v) if reference => jit::box_ref(vm, &v[index]),
            Self::Values(v) => jit::pack_local(&v[index]),
            Self::Raw(v) => v[index],
        };
        if reference { crate::nextgen::forward_if_moved(cell) } else { cell }
    }

    #[cfg(test)]
    fn pack(&self, vm: &Vm, descriptor: &str, is_static: bool, total: usize) -> Vec<i64> {
        self.pack_reusing(vm, &Layout::new(descriptor, is_static), total, Vec::with_capacity(total))
    }

    pub(super) fn pack_reusing(&self, vm: &Vm, layout: &Layout,
        total: usize, mut cells: Vec<i64>) -> Vec<i64> {
        cells.clear();
        cells.reserve(total);
        let mut slot = 0;
        for &(reference, wide) in layout.parameters.iter() {
            if slot < self.len() { cells.push(self.cell(vm, slot, reference)); slot += 1; }
            if wide {
                match self {
                    Self::Raw(_) => cells.push(0),
                    Self::Values(_) if slot < self.len() => {
                        cells.push(self.cell(vm, slot, false)); slot += 1;
                    }
                    _ => {},
                }
            }
        }
        while slot < self.len() { cells.push(self.cell(vm, slot, false)); slot += 1; }
        cells.resize(total, 0);
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aot_frame_plan_preserves_many_arguments_arrays_and_wide_bits() {
        let vm = Vm::new(crate::class_loader::ClassLoader::new());
        let descriptor = format!("({}J[D[[JLjava/lang/Object;D)Ljava/lang/Object;", "I".repeat(80));
        let layout = Layout::new(&descriptor, false);
        assert_eq!(&descriptor[layout.return_start()..], "Ljava/lang/Object;");
        drop(descriptor); // The reusable plan owns every parameter fact it needs.
        let nan_bits = 0x7ff8_0000_0000_1234u64 as i64;
        let mut raw = vec![0];
        raw.extend(-80..0);
        raw.extend([i64::MIN, 0, 0, 0, nan_bits]);
        let mut expected = vec![0];
        expected.extend(-80..0);
        expected.extend([i64::MIN, 0, 0, 0, 0, nan_bits, 0]);
        expected.resize(96, 0);
        let cells = Arguments::Raw(&raw).pack_reusing(&vm, &layout, 96, vec![i64::MAX; 96]);
        assert_eq!(cells, expected);
        raw[1] = 123;
        expected[1] = 123;
        assert_eq!(Arguments::Raw(&raw).pack_reusing(&vm, &layout, 96, cells), expected);
    }

    #[test]
    fn aot_reused_cells_clear_old_references_and_wide_padding() {
        let vm = Vm::new(crate::class_loader::ClassLoader::new());
        let scratch = vec![i64::MAX; 20];
        let raw = Arguments::Raw(&[0, 42]);
        let cells = raw.pack_reusing(&vm, &Layout::new("(J)V", false), 8, scratch);
        assert_eq!(cells, vec![0, 42, 0, 0, 0, 0, 0, 0]);
        let values = Arguments::Values(&[Value::Int(7)]);
        assert_eq!(values.pack_reusing(&vm, &Layout::new("(I)V", true), 12, cells),
            vec![7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn raw_wide_arguments_match_interpreter_local_slots() {
        let vm = Vm::new(crate::class_loader::ClassLoader::new());
        let values = [Value::Int(-7), Value::Long(i64::MIN), Value::Int(0),
            Value::Double(-2.5), Value::Int(0), Value::Null];
        let raw = [-7, i64::MIN, (-2.5f64).to_bits() as i64, 0];
        let desc = "(IJDLjava/lang/Object;)V";
        assert_eq!(Arguments::Values(&values).pack(&vm, desc, true, 10),
            Arguments::Raw(&raw).pack(&vm, desc, true, 10));
        assert_eq!(Arguments::Raw(&[0, 12, 0]).pack(&vm, "(J[D)V", false, 5),
            vec![0, 12, 0, 0, 0]);
    }
}
