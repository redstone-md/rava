//! Test-only transitive resolver for offline effect analysis.
//!
//! It follows statically exact calls and exact LambdaMetafactory implementations only.
//! Non-constructor invokespecial, virtual/interface dispatch, non-exact lambda handles, concat
//! reference conversions, missing classes/method bodies, and other bootstraps remain Unknown.
//! This graph is evidence only and never an admission proof.

use std::collections::{HashMap, HashSet, VecDeque};

use rava_classfile::ClassFile;
use rava_jar::Jar;

use super::effect_summary::{collect, Dispatch};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct MethodKey {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

#[derive(Debug, Default)]
pub(super) struct GraphReport {
    pub visited: HashSet<MethodKey>,
    pub exact_edges: usize,
    pub lambda_impl_edges: usize,
    pub unresolved_virtual_edges: usize,
    pub unresolved_special_edges: usize,
    pub unresolved_lambda_dispatch: usize,
    pub concat_sites: usize,
    pub unresolved_concat_conversions: usize,
    pub unresolved_bootstraps: usize,
    pub missing_classes: usize,
    pub missing_methods_or_code: usize,
}

/// Walk exact calls without executing guest bytecode. Missing or open-world edges stay unresolved.
pub(super) fn collect_exact_closure(jar: &mut Jar, root: MethodKey) -> GraphReport {
    let mut report = GraphReport::default();
    let mut classes = HashMap::<String, ClassFile>::new();
    let mut pending = VecDeque::from([root]);

    while let Some(key) = pending.pop_front() {
        if !report.visited.insert(key.clone()) {
            continue;
        }
        if !classes.contains_key(&key.owner) {
            match jar
                .read_class(&key.owner)
                .ok()
                .flatten()
                .and_then(|bytes| ClassFile::parse(&bytes).ok())
            {
                Some(class) => {
                    classes.insert(key.owner.clone(), class);
                }
                None => {
                    report.missing_classes += 1;
                    continue;
                }
            }
        }

        let Some(class) = classes.get(&key.owner) else {
            report.missing_classes += 1;
            continue;
        };
        let Some(method) = class.method(&key.name, &key.descriptor) else {
            report.missing_methods_or_code += 1;
            continue;
        };
        let Some(code) = method.code.as_ref() else {
            report.missing_methods_or_code += 1;
            continue;
        };

        let effects = collect(code, &class.pool);
        report.unresolved_bootstraps += effects
            .dynamic_calls
            .saturating_sub(effects.dynamic_sites.len());
        let mut next = Vec::new();
        for edge in effects.calls {
            match edge.dispatch {
                Dispatch::Static => {
                    report.exact_edges += 1;
                    next.push(MethodKey {
                        owner: edge.owner,
                        name: edge.name,
                        descriptor: edge.descriptor,
                    });
                }
                Dispatch::Special if edge.name == "<init>" => {
                    report.exact_edges += 1;
                    next.push(MethodKey {
                        owner: edge.owner,
                        name: edge.name,
                        descriptor: edge.descriptor,
                    });
                }
                Dispatch::Special => report.unresolved_special_edges += 1,
                Dispatch::Virtual | Dispatch::Interface => report.unresolved_virtual_edges += 1,
            }
        }
        for site in effects.dynamic_sites {
            match crate::offload::lmf_indy_impl(
                &class.pool,
                Some(&class.bootstrap_methods),
                site.cp_index,
            ) {
                Some((kind, owner, name, descriptor))
                    if kind == 6 || (kind == 8 && name == "<init>") =>
                {
                    report.lambda_impl_edges += 1;
                    next.push(MethodKey {
                        owner: owner.to_owned(),
                        name: name.to_owned(),
                        descriptor: descriptor.to_owned(),
                    });
                }
                Some(_) => report.unresolved_lambda_dispatch += 1,
                None => {
                    if let Some(descriptor) = crate::offload::concat_indy_desc(
                        &class.pool,
                        Some(&class.bootstrap_methods),
                        site.cp_index,
                    ) {
                        report.concat_sites += 1;
                        if !crate::offload::ref_param_types(descriptor).is_empty() {
                            report.unresolved_concat_conversions += 1;
                        }
                    } else {
                        report.unresolved_bootstraps += 1;
                    }
                }
            }
        }
        pending.extend(next);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual offline graph walk; set RAVA_EFFECT_SUMMARY_JAR to the server jar"]
    fn reports_blocks_exact_call_and_bootstrap_closure() {
        let jar_path = std::env::var("RAVA_EFFECT_SUMMARY_JAR").expect("set jar path");
        let mut jar = Jar::open(jar_path).expect("open jar");
        let graph = collect_exact_closure(
            &mut jar,
            MethodKey {
                owner: "net/minecraft/world/level/block/Blocks".into(),
                name: "<clinit>".into(),
                descriptor: "()V".into(),
            },
        );
        eprintln!(
            "Blocks exact graph: {} method keys, {} exact calls, {} lambda impls, {} unresolved special calls, {} open virtual/interface edges, {} non-exact lambda dispatches, {} concat sites ({} with unresolved reference conversions), {} unknown bootstraps, {} missing classes, {} missing methods/code",
            graph.visited.len(), graph.exact_edges, graph.lambda_impl_edges,
            graph.unresolved_special_edges, graph.unresolved_virtual_edges,
            graph.unresolved_lambda_dispatch, graph.concat_sites,
            graph.unresolved_concat_conversions, graph.unresolved_bootstraps,
            graph.missing_classes, graph.missing_methods_or_code,
        );
        assert!(graph.visited.contains(&MethodKey {
            owner: "net/minecraft/world/level/block/Blocks".into(),
            name: "<clinit>".into(),
            descriptor: "()V".into(),
        }));
        assert!(
            graph.unresolved_virtual_edges > 0
                || graph.unresolved_special_edges > 0
                || graph.missing_classes > 0
        );
    }
}
