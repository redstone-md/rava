//! Reading a Mojang codec/record shape out of bytecode.

use crate::*;

impl Vm {
    /// 21.2 recursive native AST builder for a Mojang codec object (Skeletal Registry Extraction).
    /// Reads the codec's fields through rava's INTERNAL field API (read_value_field — a pure read, no
    /// guest-heap alloc, no GC trigger) and classifies the node: FieldDecoder → Field(name, sub);
    /// ListCodec → List(sub); PrimitiveCodec → Primitive; KeyDispatchCodec → Dispatch (stop, custom
    /// lambdas); anything else / depth-cap → Opaque (stop). Recursion follows the already-built
    /// sub-codecs. Inert (used only by the gated 21.1/21.2 recon).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_codec_ast(&mut self, codec: &Value, depth: u8) -> crate::codec_flatten::CodecNode {
        use crate::codec_flatten::CodecNode;
        if depth >= 12 {
            return CodecNode::Opaque("depth-cap".to_string());
        }
        let class = self.value_class_name(codec);
        if class.contains("PrimitiveCodec") {
            let short = class.rsplit('/').next().unwrap_or(&class).to_string();
            return CodecNode::Primitive(short);
        }
        match class.as_str() {
            "com/mojang/serialization/codecs/FieldDecoder"
            | "com/mojang/serialization/codecs/FieldEncoder" => {
                let name = self.codec_field_string(codec, &class, "name");
                let sub = match self.codec_field_ref(codec, &class, "elementCodec") {
                    Some(ic) => self.build_codec_ast(&ic, depth + 1),
                    None => CodecNode::Opaque("no-inner".to_string()),
                };
                CodecNode::Field(name, Box::new(sub))
            }
            "com/mojang/serialization/codecs/ListCodec" => {
                match self.codec_field_ref(codec, &class, "elementCodec") {
                    Some(ic) => CodecNode::List(Box::new(self.build_codec_ast(&ic, depth + 1))),
                    None => CodecNode::Opaque("list".to_string()),
                }
            }
            "com/mojang/serialization/codecs/KeyDispatchCodec" => {
                let sub = match self.codec_field_ref(codec, &class, "keyCodec") {
                    Some(kc) => self.build_codec_ast(&kc, depth + 1),
                    None => CodecNode::Opaque("no-key".to_string()),
                };
                CodecNode::Dispatch(Box::new(sub))
            }
            other => {
                // 21.3 PRODUCTS → STRUCT: Products$P1..P16 encapsulate the record's field codecs in
                // t1..tN (each an App = a field builder). Assemble them into a flat Struct.
                if let Some(n) = crate::codec_flatten::products_arity(other) {
                    let mut fields: Vec<(String, Box<CodecNode>)> = Vec::with_capacity(n as usize);
                    for i in 1..=n {
                        let fname = format!("t{i}");
                        if let Some(t) = self.codec_field_ref(codec, &class, &fname) {
                            fields.push((fname, Box::new(self.build_codec_ast(&t, depth + 1))));
                        }
                    }
                    return CodecNode::Struct(fields);
                }
                // 21.3 WRAPPER PEELING: an anonymous delegating codec (Codec$N / MapCodec$N) or a
                // named wrapper (RegistryFileCodec, RecordCodecBuilder) holds its real codec in one
                // ref field. Read the KNOWN delegate fields; if it points at another codec, recurse
                // THROUGH the wrapper (peel to the raw PrimitiveCodec). A Supplier/Function-backed
                // field (e.g. MapCodec$11.val$value) is a guest lambda ⇒ NOT a codec ⇒ stays Opaque.
                // `this$0` is an anonymous inner class's wrapped delegate (Decoder$N.this$0 = the
                // mapped-over Decoder). Specific codec fields first, then this$0. A val$function /
                // val$value (guest lambda) is never a codec ⇒ the inner-is-codec guard rejects it.
                const DELEGATES: &[&str] = &[
                    "elementCodec", "val$decoder", "val$codec", "this$0", "codec", "wrapped",
                    "delegate", "innerCodec",
                ];
                let is_wrapper = other.contains("Codec$")
                    || other.contains("Decoder$")
                    || other == "net/minecraft/resources/RegistryFileCodec"
                    || other == "com/mojang/serialization/codecs/RegistryFileCodec"
                    || other == "com/mojang/serialization/codecs/RecordCodecBuilder";
                if is_wrapper {
                    for f in DELEGATES {
                        if let Some(inner) = self.codec_field_ref(codec, &class, f) {
                            let icc = self.value_class_name(&inner);
                            // Only peel to an actual codec/decoder (never through a guest lambda).
                            if icc.starts_with("com/mojang/serialization")
                                || icc.contains("Codec")
                                || icc.contains("Decoder")
                            {
                                return self.build_codec_ast(&inner, depth + 1);
                            }
                        }
                    }
                    // No codec delegate found: a STATELESS anonymous codec (no instance fields — e.g.
                    // Codec$16/$14, the base terminal codecs the primitives are built from) is a leaf
                    // → Primitive. A wrapper with only lambda-backed fields (MapCodec$11.val$value =
                    // a Supplier, a validate/xmap with a captured Function) is a genuine custom
                    // transformation with no structural inner → stays Opaque.
                    if self.class_field_layout(&class).is_empty() {
                        let short = class.rsplit('/').next().unwrap_or(&class).to_string();
                        return CodecNode::Primitive(short);
                    }
                }
                CodecNode::Opaque(class)
            }
        }
    }

    /// Resolve `field` on `class` ONLY IF it is actually in the class's object layout — reading a
    /// non-layout field id on an arena object PANICS (opcodes arena_getfield asserts the id). The
    /// wrapper-peeling probes many candidate field names, most absent on any given class, so this
    /// existence guard is mandatory. Returns the field's Value (None if absent/null).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn codec_read_field(&mut self, recv: &Value, class: &str, field: &str) -> Option<Value> {
        let id = self.resolve_field_id(class, field);
        let layout = self.class_field_layout(class);
        if !layout.iter().any(|(fid, _)| *fid == id) {
            return None; // field not in this class's layout — reading it would panic
        }
        self.read_value_field(recv, id)
    }
    /// The inner sub-codec ref field (None for absent/null).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn codec_field_ref(&mut self, recv: &Value, class: &str, field: &str) -> Option<Value> {
        match self.codec_read_field(recv, class, field) {
            Some(Value::Null) | None => None,
            Some(v) => Some(v),
        }
    }
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn codec_field_string(&mut self, recv: &Value, class: &str, field: &str) -> String {
        match self.codec_read_field(recv, class, field) {
            Some(v) => self.value_to_string(&v).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// 24.2: classify a FieldDecoder's elementCodec into a RecordAst field kind. Uses build_codec_ast
    /// (peels 21.3 wrappers → accurate Primitive detection); an Opaque node that is actually a record
    /// codec is re-labelled Record (build_codec_ast can't structurally build records).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn recordast_field_kind(&mut self, recv: &Value) -> u8 {
        let Some(inner) =
            self.codec_field_ref(recv, "com/mojang/serialization/codecs/FieldDecoder", "elementCodec")
        else {
            return crate::recordast::OPAQUE;
        };
        match self.build_codec_ast(&inner, 1) {
            crate::codec_flatten::CodecNode::Primitive(_) => crate::recordast::PRIMITIVE,
            crate::codec_flatten::CodecNode::List(_) => crate::recordast::LIST,
            crate::codec_flatten::CodecNode::Dispatch(_) => crate::recordast::DISPATCH,
            crate::codec_flatten::CodecNode::Struct(_) | crate::codec_flatten::CodecNode::Field(_, _) => {
                crate::recordast::RECORD
            }
            crate::codec_flatten::CodecNode::Opaque(cls) => {
                if cls.contains("RecordCodecBuilder") || cls.contains("MapCodecCodec") {
                    crate::recordast::RECORD
                } else {
                    crate::recordast::OPAQUE
                }
            }
        }
    }
}
