use super::IntMap;

#[test]
fn test_int_map_consecutive_inserts() {
    let m: IntMap<u64> = (0..100u64).map(|x| (x, x + 100)).collect();

    for i in 0..100u64 {
        assert_eq!(
            m.get(i).cloned(),
            Some(i + 100),
            "failed to find inserted values, map: {:?}",
            m
        );
    }
}

#[test]
fn test_int_map_sparse_inserts() {
    let m: IntMap<u64> = (0..100u64)
        .filter(|x| x % 2 == 0)
        .map(|x| (x, x + 100))
        .collect();

    for i in 0..100u64 {
        if i % 2 == 0 {
            assert_eq!(m.get(i).cloned(), Some(i + 100));
        } else {
            assert_eq!(m.get(i).cloned(), None);
        }
    }
}

#[test]
fn test_int_map_union() {
    let lmap: IntMap<u64> = (1..101u64).map(|x| (x, x)).collect();
    let rmap: IntMap<u64> = (50..150u64).map(|x| (x, x + 100)).collect();
    let m = rmap.union(lmap);

    assert!(m.get(0).is_none());
    for i in 1..50u64 {
        assert_eq!(m.get(i).cloned(), Some(i));
    }
    for i in 50..150u64 {
        assert_eq!(m.get(i).cloned(), Some(i + 100), "Map: {:?}", m);
    }
    assert!(m.get(150).is_none());
}

#[test]
fn test_iter() {
    use std::collections::BTreeMap;

    let int_map: IntMap<_> = (1..100u64).map(|x| (x, x)).collect();
    let btree_map: BTreeMap<_, _> = (1..100u64).map(|x| (x, x)).collect();

    assert!(int_map.iter().eq(btree_map.iter().map(|(k, v)| (*k, v))));
}

#[test]
fn test_int_map_bounds() {
    let m: IntMap<u64> = (10..=100u64).map(|x| (7 * x, 0)).collect();
    for i in 0..800 {
        let (start, end) = m.bounds(i);
        if (70..=700).contains(&i) {
            assert_eq!(start, Some(((i / 7) * 7, &0)));
            assert_eq!(end, Some((((i + 6) / 7) * 7, &0)));
        } else if i < 70 {
            assert_eq!(start, None);
            assert_eq!(end, Some((70, &0)));
        } else {
            assert_eq!(start, Some((700, &0)));
            assert_eq!(end, None)
        }
    }
}

#[test]
fn test_max_key() {
    let m = IntMap::<u64>::new();
    assert_eq!(m.max_key(), None);
    let m = m.insert(100, 101);
    assert_eq!(m.max_key(), Some(100));
    let m = m.insert(10, 101);
    assert_eq!(m.max_key(), Some(100));
    let m = m.insert(1000, 101);
    assert_eq!(m.max_key(), Some(1000));
    let m = m.insert(1000000, 101);
    assert_eq!(m.max_key(), Some(1000000));
}

#[test]
fn test_max_key_range() {
    let mut m = IntMap::<u64>::new();
    for i in 0..1000u64 {
        m = m.insert(i, i + 100);
        assert_eq!(m.max_key(), Some(i));
    }
}

#[test]
fn test_million_inserts() {
    let now = std::time::Instant::now();
    let hm: im::HashMap<u64, u64> = (0..1_000_000u64).map(|x| (x, x + 100)).collect();

    println!("Time taken im::HashMap: {:?}", now.elapsed());

    let now = std::time::Instant::now();
    let rpds_hm: rpds::HashTrieMap<u64, u64> = (0..1_000_000u64).map(|x| (x, x + 100)).collect();

    println!("Time taken rpds::HashTrieMap: {:?}", now.elapsed());

    let now = std::time::Instant::now();
    let m: IntMap<u64> = (0..1_000_000u64).map(|x| (x, x + 100)).collect();

    println!("Time taken IntMap: {:?}", now.elapsed());
    println!("Size: {}", m.len());

    let now = std::time::Instant::now();
    let _rb: rpds::RedBlackTreeMap<u64, u64> = (0..1_000_000u64).map(|x| (x, x + 100)).collect();

    println!("Time taken rpds::RedBlackTreeMap: {:?}", now.elapsed());

    let now = std::time::Instant::now();
    let mut arr: im::Vector<u64> = im::Vector::new();
    (0..1_000_000u64).for_each(|x| {
        arr.push_back(x + 100);
    });

    let mut new_arr = arr.clone();
    (0..1_000_000u64).for_each(|x| {
        new_arr.push_back(x + 1000);
    });
    println!("Time taken im::Vector: {:?}", now.elapsed());

    assert_eq!(m.len(), hm.len());
    assert_eq!(m.len(), rpds_hm.size());
    assert_eq!(arr.len(), new_arr.len() / 2);
}
