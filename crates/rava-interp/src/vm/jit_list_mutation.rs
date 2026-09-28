//! JIT list writes share collection contracts, view updates and modification counts.
use crate::*;

impl Vm {
    pub(crate) fn jit_list_mutation(
        &mut self, receiver: u32, name: &str, descriptor: &str, cells: &[i64],
    ) -> Result<Option<i64>, VmError> {
        let (args, count, primitive) = match (name, descriptor) {
            ("add", "(Ljava/lang/Object;)Z") => (
                [Value::Ref(receiver), jit::unbox_ref(self, *cells.get(1).unwrap_or(&0), ""), Value::Null],
                2, true,
            ),
            ("remove", "(I)Ljava/lang/Object;") => (
                [Value::Ref(receiver), Value::Int(*cells.get(1).unwrap_or(&0) as i32), Value::Null],
                2, false,
            ),
            ("set", "(ILjava/lang/Object;)Ljava/lang/Object;") => (
                [Value::Ref(receiver), Value::Int(*cells.get(1).unwrap_or(&0) as i32),
                    jit::unbox_ref(self, *cells.get(2).unwrap_or(&0), "")],
                3, false,
            ),
            _ => return Ok(None),
        };
        let args = &args[..count];
        #[cfg(feature = "next-gen-gc")]
        let _roots = crate::opcodes::push_call_roots(self, None, args);
        let handled = crate::stream::coll_op(self, name, descriptor, args)?;
        Ok(handled.map(|value| value.map_or(0, |value| {
            if primitive { jit::pack_local(&value) } else { jit::box_ref(self, &value) }
        })))
    }
}
