use super::*;

#[test]
fn insert_and_get() {
    let mut cache = LineCache::new(10);
    let key = CacheKey { cell: 1, width: 80 };
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
    let k1 = CacheKey { cell: 1, width: 80 };
    let k2 = CacheKey { cell: 2, width: 80 };
    let k3 = CacheKey { cell: 3, width: 80 };

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
fn insert_never_evicts_itself() {
    let mut cache = LineCache::new(3);
    let k1 = CacheKey { cell: 1, width: 80 };
    let k2 = CacheKey { cell: 2, width: 80 };

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
    let k1_80 = CacheKey { cell: 1, width: 80 };
    let k1_100 = CacheKey {
        cell: 1,
        width: 100,
    };
    let k2_80 = CacheKey { cell: 2, width: 80 };

    cache.insert(k1_80, vec![Line::from("1")]);
    cache.insert(k1_100, vec![Line::from("2")]);
    cache.insert(k2_80, vec![Line::from("3")]);
    assert_eq!(cache.total_lines(), 3);

    cache.invalidate_cell(1);
    assert!(cache.get(k1_80).is_none());
    assert!(cache.get(k1_100).is_none());
    assert!(cache.get(k2_80).is_some());
    assert_eq!(cache.total_lines(), 1);
}

#[test]
fn clear_resets_everything() {
    let mut cache = LineCache::new(10);
    cache.insert(CacheKey { cell: 1, width: 80 }, vec![Line::from("1")]);
    cache.clear();
    assert_eq!(cache.total_lines(), 0);
    assert!(cache.get(CacheKey { cell: 1, width: 80 }).is_none());
}
