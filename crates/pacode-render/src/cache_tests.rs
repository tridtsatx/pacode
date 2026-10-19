use super::*;

#[test]
fn insert_and_get() {
    let mut cache = LineCache::new(10);
    let key = CacheKey::cell(1, 0, 80, false, 0);
    cache.insert(key, vec![Line::from("line 1"), Line::from("line 2")]);
    assert_eq!(cache.total_lines(), 2);

    let lines = cache.get(key);
    assert!(lines.is_some());
    assert_eq!(lines.unwrap().len(), 2);
    assert_eq!(lines.unwrap()[0].to_string(), "line 1");
}

#[test]
fn lru_eviction_order() {
    let mut cache = LineCache::new(5);
    let k1 = CacheKey::cell(1, 0, 80, false, 0);
    let k2 = CacheKey::cell(2, 0, 80, false, 0);
    let k3 = CacheKey::cell(3, 0, 80, false, 0);

    cache.insert(k1, vec![Line::from("c1_1"), Line::from("c1_2")]); // 2 lines
    cache.insert(k2, vec![Line::from("c2_1"), Line::from("c2_2")]); // 2 lines (total 4)

    // Access k1 so k2 becomes least recently used
    assert!(cache.get(k1).is_some());

    // Insert k3 with 2 lines -> total would be 6 > 5 -> evicts k2
    cache.insert(k3, vec![Line::from("c3_1"), Line::from("c3_2")]);

    assert!(cache.get(k2).is_none(), "k2 should be evicted");
    assert!(cache.get(k1).is_some(), "k1 was recently used");
    assert!(cache.get(k3).is_some(), "k3 was just inserted");
    assert_eq!(cache.total_lines(), 4);
}

#[test]
fn a_get_makes_its_entry_the_most_recent() {
    // Insert k1, k2, k3 (k3 most recent). A get on k1 moves it to the back, so
    // the next eviction takes k2, not k1. `peek` checks without touching
    // recency, so the assertions themselves cannot reshuffle the order.
    let mut cache = LineCache::new(3);
    let k1 = CacheKey::cell(1, 0, 80, false, 0);
    let k2 = CacheKey::cell(2, 0, 80, false, 0);
    let k3 = CacheKey::cell(3, 0, 80, false, 0);
    let k4 = CacheKey::cell(4, 0, 80, false, 0);
    let k5 = CacheKey::cell(5, 0, 80, false, 0);

    cache.insert(k1, vec![Line::from("1")]);
    cache.insert(k2, vec![Line::from("2")]);
    cache.insert(k3, vec![Line::from("3")]);
    assert!(cache.get(k1).is_some()); // order is now k2, k3, k1

    // 4 lines > 3: evicts k2, the least recent since the k1 touch.
    cache.insert(k4, vec![Line::from("4")]);
    assert!(
        cache.peek(k2).is_none(),
        "k2 was the least recent once k1 was touched"
    );
    assert!(cache.peek(k1).is_some(), "the get on k1 kept it warm");
    assert!(cache.peek(k3).is_some());
    assert!(cache.peek(k4).is_some());

    // Evicts k3 — k1, k4, k5 remain.
    cache.insert(k5, vec![Line::from("5")]);
    assert!(cache.peek(k3).is_none());
    assert!(cache.peek(k1).is_some());
    assert_eq!(cache.total_lines(), 3);
}

#[test]
fn insert_never_evicts_itself() {
    let mut cache = LineCache::new(3);
    let k1 = CacheKey::cell(1, 0, 80, false, 0);
    let k2 = CacheKey::cell(2, 0, 80, false, 0);

    cache.insert(k1, vec![Line::from("c1_1"), Line::from("c1_2")]);
    // Insert k2 with 5 lines, which exceeds max_lines (3)
    cache.insert(
        k2,
        vec![
            Line::from("1"),
            Line::from("2"),
            Line::from("3"),
            Line::from("4"),
            Line::from("5"),
        ],
    );

    assert!(cache.get(k1).is_none(), "k1 should be evicted");
    assert!(
        cache.get(k2).is_some(),
        "k2 must not be evicted by its own insert"
    );
    assert_eq!(cache.total_lines(), 5);
}

#[test]
fn invalidate_cell_removes_all_widths() {
    let mut cache = LineCache::new(10);
    let k1_80 = CacheKey::cell(1, 0, 80, false, 0);
    let k1_100 = CacheKey::cell(1, 0, 100, false, 0);
    let k2_80 = CacheKey::cell(2, 0, 80, false, 0);
    let k1_stable = CacheKey::stable_prefix(1, 1, 4, 80, 0);

    cache.insert(k1_80, vec![Line::from("1")]);
    cache.insert(k1_100, vec![Line::from("2")]);
    cache.insert(k2_80, vec![Line::from("3")]);
    cache.insert(k1_stable, vec![Line::from("stable")]);
    assert_eq!(cache.total_lines(), 4);

    cache.invalidate_cell(1);
    assert!(cache.get(k1_80).is_none());
    assert!(cache.get(k1_100).is_none());
    assert!(cache.get(k1_stable).is_none());
    assert!(cache.get(k2_80).is_some());
    assert_eq!(cache.total_lines(), 1);
}

#[test]
fn explicit_identity_fields_do_not_alias() {
    let base = CacheKey::cell(7, 3, 80, false, 11);
    assert_ne!(base, CacheKey::cell(7, 4, 80, false, 11));
    assert_ne!(base, CacheKey::cell(7, 3, 81, false, 11));
    assert_ne!(base, CacheKey::cell(7, 3, 80, true, 11));
    assert_ne!(base, CacheKey::cell(7, 3, 80, false, 12));
    assert_ne!(base, CacheKey::stable_prefix(7, 3, 80, 80, 11));
    assert_ne!(
        CacheKey::stable_prefix(7, 3, 10, 80, 11),
        CacheKey::stable_prefix(7, 4, 10, 80, 11)
    );
}

#[test]
fn clear_resets_everything() {
    let mut cache = LineCache::new(10);
    cache.insert(CacheKey::cell(1, 0, 80, false, 0), vec![Line::from("1")]);
    cache.clear();
    assert_eq!(cache.total_lines(), 0);
    assert!(cache.get(CacheKey::cell(1, 0, 80, false, 0)).is_none());
}

#[test]
fn clear_bumps_generation_and_eviction_keeps_order_in_sync() {
    let mut cache = LineCache::new(100);
    let g = cache.generation();
    cache.insert(CacheKey::cell(1, 0, 80, false, 0), vec![Line::from("1")]);
    assert_eq!(cache.generation(), g);
    cache.clear();
    assert_ne!(cache.generation(), g);
    assert!(cache.order.is_empty());
}

/// The stamp-based LRU evicts exactly what a naive list-based LRU would.
#[test]
fn matches_a_naive_lru_model_over_a_random_workload() {
    use std::collections::VecDeque;
    let max = 12usize;
    let mut cache = LineCache::new(max);
    let mut model: VecDeque<(CacheKey, usize)> = VecDeque::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..2000 {
        let key = CacheKey::cell(next() % 9, 0, 80, false, 0);
        match next() % 3 {
            0 => {
                let hit = cache.get(key).is_some();
                let pos = model.iter().position(|(k, _)| *k == key);
                assert_eq!(hit, pos.is_some());
                if let Some(p) = pos
                    && let Some(e) = model.remove(p)
                {
                    model.push_back(e);
                }
            }
            1 => {
                let n = (next() % 5) as usize + 1;
                cache.insert(key, vec![Line::from("x"); n]);
                if let Some(p) = model.iter().position(|(k, _)| *k == key) {
                    model.remove(p);
                }
                model.push_back((key, n));
                while model.iter().map(|(_, n)| n).sum::<usize>() > max && model.len() > 1 {
                    model.pop_front();
                }
            }
            _ => {
                let cell = key.cell;
                cache.invalidate_cell(cell);
                model.retain(|(k, _)| k.cell != cell);
            }
        }
        assert_eq!(
            cache.total_lines(),
            model.iter().map(|(_, n)| n).sum::<usize>()
        );
        assert_eq!(cache.order.len(), cache.entries.len());
        for (k, _) in &model {
            assert!(cache.contains(*k));
        }
    }
}
