use super::ThreadBudget;

#[test]
fn reservations_share_one_capacity_and_return_on_drop() {
    let budget = ThreadBudget::new(4);
    let a = budget.reserve(3);
    assert_eq!(a.count(), 3);
    let b = budget.reserve(3);
    assert_eq!(b.count(), 1, "only what is free");
    let c = budget.reserve(2);
    assert_eq!(c.count(), 0, "none free: the call runs on its own thread");
    assert_eq!(budget.in_use(), 4);
    drop(a);
    assert_eq!(budget.in_use(), 1);
    let d = budget.reserve(8);
    assert_eq!(d.count(), 3);
    drop((b, c, d));
    assert_eq!(budget.in_use(), 0);
}

/// Working threads count even past capacity, and leave no helpers while
/// they do: 16 concurrent calls on 4 cores add no threads on top.
#[test]
fn occupied_threads_leave_no_helpers() {
    let budget = ThreadBudget::new(4);
    let one = budget.occupy();
    assert_eq!(budget.reserve(8).count(), 3, "a lone call gets the rest");
    let busy: Vec<_> = (0..15).map(|_| budget.occupy()).collect();
    assert_eq!(budget.in_use(), 16);
    assert_eq!(budget.reserve(8).count(), 0);
    drop(busy);
    drop(one);
    assert_eq!(budget.in_use(), 0);
}

#[test]
fn concurrent_reservations_never_exceed_capacity() {
    let budget = ThreadBudget::new(5);
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                for _ in 0..1_000 {
                    let r = budget.reserve(3);
                    assert!(budget.in_use() <= 5);
                    drop(r);
                }
            });
        }
    });
    assert_eq!(budget.in_use(), 0);
}
