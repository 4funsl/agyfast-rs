// Compile the shared API as a client. Both the callbacks and the renderers see
// dense output positions; the plan carries the corresponding original slots.
#![allow(dead_code)]

#[path = "../../src/core/mod.rs"]
mod core;

use core::batch::{BatchPlan, Slot};
use core::budget::{join_fair, join_fair_by};

#[test]
fn typed_plan_contract() {
    let make: fn(usize, Option<&str>, Option<usize>, usize) -> Result<BatchPlan, String> = BatchPlan::new;
    let plan = make(5, Some("4-5,2,!5,5"), Some(250), 500).unwrap();
    let positions: Vec<(usize, usize)> = plan.slots.iter().map(|s: &Slot| (s.index(), s.number())).collect();
    assert_eq!(positions, [(3, 4), (1, 2), (4, 5)]);
    assert_eq!(plan.bytes, 500);

    let text = vec!["4".into(), "2".into(), "5".into()];
    assert_eq!(join_fair(&text, &plan), "4\n\n2\n\n5");
    assert_eq!(join_fair_by(&[1; 3], &plan, |i, _| text[i].clone()), "4\n\n2\n\n5");
    assert!(make(5, Some("5,6"), None, 500).is_err());
    assert_eq!(make(1, None, Some(usize::MAX), 500).unwrap().bytes, 8800);
}
