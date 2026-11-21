use super::{Write, begin, end, paused, record};
use crate::tree::NodeId;

fn nodes(writes: &[Write]) -> Vec<NodeId> {
    writes
        .iter()
        .map(|write| match write {
            Write::Decided { node, .. } => *node,
            Write::Sent { .. } => unreachable!("no hook runs in this test"),
        })
        .collect()
}

#[test]
fn records_only_between_begin_and_end() {
    record(NodeId::for_test(1));
    begin();
    record(NodeId::for_test(2));
    assert_eq!(nodes(&end()), [NodeId::for_test(2)]);
    record(NodeId::for_test(3));
    begin();
    assert!(end().is_empty(), "a write after end is not kept");
}

#[test]
fn paused_writes_are_left_out() {
    begin();
    paused(|| {
        record(NodeId::for_test(1));
        paused(|| record(NodeId::for_test(2)));
        record(NodeId::for_test(3));
    });
    record(NodeId::for_test(4));
    assert_eq!(nodes(&end()), [NodeId::for_test(4)]);
}

#[test]
fn a_panic_inside_paused_resumes_recording() {
    begin();
    let caught = std::panic::catch_unwind(|| paused(|| panic!("inside")));
    assert!(caught.is_err());
    record(NodeId::for_test(1));
    assert_eq!(nodes(&end()), [NodeId::for_test(1)]);
}

#[test]
fn the_site_is_the_caller() {
    begin();
    let line = line!() + 1;
    record(NodeId::for_test(1));
    let [Write::Decided { site, .. }] = end()[..] else {
        panic!("one decided write")
    };
    assert_eq!((site.file(), site.line()), (file!(), line));
}
