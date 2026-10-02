use super::*;

#[test]
fn reuses_the_most_recently_returned_scratch() {
    let pool = ScratchPool::<Vec<u32>>::new();
    pool.with(|scratch| scratch.push(1));
    pool.with(|scratch| {
        assert_eq!(scratch, &[1]);
        scratch.push(2);
    });
    pool.with(|scratch| assert_eq!(scratch, &[1, 2]));
}

#[test]
fn nested_searches_get_distinct_scratch() {
    let pool = ScratchPool::<Vec<u32>>::new();
    pool.with(|outer| {
        outer.push(1);
        pool.with(|inner| {
            assert!(inner.is_empty());
            inner.push(2);
        });
        assert_eq!(outer, &[1]);
    });
    // Both values are back: the outer one was returned last.
    pool.with(|first| {
        assert_eq!(first, &[1]);
        pool.with(|second| assert_eq!(second, &[2]));
    });
}
