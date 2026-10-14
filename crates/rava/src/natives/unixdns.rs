//! Unix sun.net.dns resolver fallback; resolv.conf parsing stays in the class library.

use super::{ret_ref, NativeRegistry};
use crate::runtime;

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("sun/net/dns/ResolverConfigurationImpl", "fallbackDomain0", "()Ljava/lang/String;", |t, _| {
        match crate::os::fallback_domain() {
            Some(bytes) => {
                let units = crate::symbol::decode_modified_utf8(&bytes);
                ret_ref(runtime::new_string_utf16(t, &units)?)
            }
            None => ret_ref(std::ptr::null_mut()),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::NativeRegistry;

    #[test]
    fn builtins_register_the_unix_resolver_fallback() {
        let registry = NativeRegistry::with_builtins();
        assert!(registry.table.contains_key(&("sun/net/dns/ResolverConfigurationImpl", "fallbackDomain0", "()Ljava/lang/String;")));
    }
}
