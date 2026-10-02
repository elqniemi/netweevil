use super::*;

#[test]
fn equal_arrays_share_one_allocation() {
    let interner = ArrayInterner::<u32>::new();
    let first = interner.intern(vec![1, 2, 3]);
    let second = interner.intern(vec![1, 2, 3]);
    let different = interner.intern(vec![1, 2, 4]);
    assert!(Arc::ptr_eq(&first, &second));
    assert!(!Arc::ptr_eq(&first, &different));
}

#[test]
fn floats_are_compared_bit_for_bit() {
    let interner = ArrayInterner::<f64>::new();
    let positive = interner.intern(vec![0.0, f64::INFINITY]);
    let negative = interner.intern(vec![-0.0, f64::INFINITY]);
    assert!(!Arc::ptr_eq(&positive, &negative));
    assert_eq!(negative[0].to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn dropped_arrays_are_not_reused() {
    let interner = ArrayInterner::<u32>::new();
    drop(interner.intern(vec![7; 4]));
    let second = interner.intern(vec![7; 4]);
    assert_eq!(Arc::strong_count(&second), 1);
    assert_eq!(interner.entries.lock().unwrap().len(), 1);
}

#[test]
fn derived_values_follow_their_key() {
    let cache = DerivedCache::<u32, String>::new();
    let key = Arc::new(1);
    let other = Arc::new(1);
    let first = cache.get_or_build(&key, || "a".to_string());
    let again = cache.get_or_build(&key, || unreachable!("cached"));
    let for_other = cache.get_or_build(&other, || "b".to_string());
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(*for_other, "b");
}
