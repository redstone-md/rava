use super::*;
use crate::class_loader::ClassLoader;
use rava_classfile::{ClassFile, Code, Constant, ConstantPool, ExceptionHandler, MemberInfo};

pub(crate) fn class() -> Arc<LoadedClass> {
    let method = |name: &str, desc: &str, flags: u16, load: u8, ret: u8, nlocals| MemberInfo {
        name: name.into(), descriptor: desc.into(), access_flags: flags,
        code: Some(Code {
            bytecode: vec![0, 0x57, load, ret], max_locals: nlocals, max_stack: 2,
            exceptions: vec![ExceptionHandler { start_pc: 0, end_pc: 1, handler_pc: 1, catch_type: 0 }],
            ..Default::default()
        }),
    };
    let file = ClassFile::synthesize(
        ConstantPool::from_entries(vec![Constant::Unusable]), 0x21, "OwnedExit".into(),
        None, vec![], vec![
            method("integer", "(I)I", 8, 0x1a, 0xac, 1),
            method("wide", "(J)J", 8, 0x1e, 0xad, 2),
            method("instance", "(I)I", 0, 0x1b, 0xac, 2),
            MemberInfo { name: "native".into(), descriptor: "()V".into(), access_flags: 0x108, code: None },
        ],
    );
    // Return without the loader: only the returned Arc owns this class.
    ClassLoader::new().define_synthetic(file)
}

fn exit_site(class: Arc<LoadedClass>, method: usize, pc: usize) -> Option<OwnedExceptionSite> {
    OwnedExceptionSites::new(class, method)?.site(pc)
}

#[test]
fn owned_metadata_keeps_class_and_code_alive_then_releases_its_pin() {
    let class = class();
    let weak = Arc::downgrade(&class);
    let site = exit_site(class, 0, 0).unwrap();
    assert!(weak.upgrade().is_some());
    let frame = site.frame();
    let expected_code = frame.code;
    drop(site);
    assert!(weak.upgrade().is_some(), "the activation has its own class pin");
    assert_eq!(frame.code, expected_code);
    drop(frame);
    assert!(weak.upgrade().is_none(), "metadata must not leak a permanent class pin");
}

#[test]
fn owned_site_uses_declared_static_and_instance_slot_layouts() {
    let class = class();
    let mut vm = Vm::new(ClassLoader::new());
    for (method, cells, expected) in [
        (0, vec![37], 37), (1, vec![i64::MIN + 9, -1], i64::MIN + 9),
        (2, vec![0, 53], 53),
    ] {
        let site = exit_site(Arc::clone(&class), method, 0).unwrap();
        let mut frame = site.frame();
        let exception = vm.make_exception("java/lang/ArithmeticException", "owned");
        vm.jit_pending_set(exception);
        let outcome = site.resume_pending(&mut vm, &mut frame, &cells).unwrap().unwrap();
        match outcome {
            RunExit::Normal(Some(Value::Int(got))) => assert_eq!(got as i64, expected),
            RunExit::Normal(Some(Value::Long(got))) => assert_eq!(got, expected),
            _ => panic!("wrong handler outcome"),
        }
    }
}

#[test]
fn wrong_class_or_method_cannot_consume_pending_exception() {
    let owner = class();
    let site = exit_site(Arc::clone(&owner), 0, 0).unwrap();
    let other_method = exit_site(owner, 1, 0).unwrap();
    let other_class = exit_site(class(), 0, 0).unwrap();
    let mut vm = Vm::new(ClassLoader::new());
    let exception = vm.make_exception("java/lang/ArithmeticException", "pending");
    vm.jit_pending_set(exception);
    for mut frame in [other_method.frame(), other_class.frame()] {
        assert!(site.resume_pending(&mut vm, &mut frame, &[0]).is_none());
        assert!(vm.jit_pending_is_set());
    }
    assert!(vm.jit_pending_take().is_some());
}

#[test]
fn invalid_member_or_unprotected_pc_cannot_be_published() {
    let class = class();
    for (method, pc) in [(4, 0), (3, 0), (0, 1), (0, 99)] {
        assert!(exit_site(Arc::clone(&class), method, pc).is_none());
    }
    fn send_sync<T: Send + Sync>() {}
    send_sync::<OwnedExceptionSite>();
}

#[test]
fn sites_survive_their_shared_compile_analysis() {
    let sites = OwnedExceptionSites::new(class(), 0).unwrap();
    let first = sites.site(0).unwrap();
    let second = sites.site(0).unwrap();
    drop(sites);
    let first_frame = first.frame();
    let second_frame = second.frame();
    assert_eq!(first_frame.code, second_frame.code);
    assert!(Arc::ptr_eq(&first_frame.class, &second_frame.class));
}
