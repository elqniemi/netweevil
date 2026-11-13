use super::*;

#[test]
fn fixed_point_rounding_and_path_sum() {
    assert_eq!(encode_cch_weight(0.5 / CCH_WEIGHT_SCALE), 1);
    assert_eq!(encode_cch_weight(f64::MIN_POSITIVE), 1);
    assert_eq!(encode_cch_weight(0.0), 0);
    let values = [0.0001, 0.123456, 123.456789, 500.123456];
    let mut sum = 0;
    for value in values {
        let encoded = encode_cch_weight(value);
        let tolerance = if value < 0.5 / CCH_WEIGHT_SCALE {
            1.0 / CCH_WEIGHT_SCALE
        } else {
            0.5 / CCH_WEIGHT_SCALE
        };
        assert!((decode_cch_weight(encoded) - value).abs() <= tolerance);
        sum = add_cch_weights(sum, encoded);
    }
    assert!(
        (decode_cch_weight(sum) - values.iter().sum::<f64>()).abs()
            <= values.len() as f64 / CCH_WEIGHT_SCALE
    );
}

#[test]
fn overflow_stays_distinct_from_inaccessible_and_never_wraps() {
    assert_eq!(encode_cch_weight(f64::INFINITY), CCH_WEIGHT_INFINITY);
    assert_eq!(encode_cch_weight(1e20), CCH_WEIGHT_OVERFLOW);
    assert_eq!(
        add_cch_weights(CCH_WEIGHT_OVERFLOW - 10, 20),
        CCH_WEIGHT_OVERFLOW
    );
    assert_eq!(add_cch_weights(CCH_WEIGHT_OVERFLOW, 5), CCH_WEIGHT_OVERFLOW);
    assert_eq!(add_cch_weights(CCH_WEIGHT_INFINITY, 0), CCH_WEIGHT_INFINITY);
    let largest = CCH_WEIGHT_OVERFLOW - 1;
    assert_eq!(encode_cch_weight(decode_cch_weight(largest)), largest);
}

#[test]
fn edge_metrics_are_two_f64_with_absent_sentinels() {
    assert_eq!(size_of::<CompiledEdgeMetric>(), 16);
    assert_eq!(CompiledEdgeMetric::ABSENT.travel_time_s(), None);
    assert_eq!(CompiledEdgeMetric::ABSENT.generalized_cost(), None);
    let metric = CompiledEdgeMetric::new(Some(12.5), None);
    assert_eq!(metric.travel_time_s(), Some(12.5));
    assert_eq!(metric.generalized_cost(), None);
    let metric = CompiledEdgeMetric::new(Some(1_000.0 / (70.0 / 3.6)), Some(f64::INFINITY));
    assert_eq!(metric.travel_time_s(), Some(1_000.0 / (70.0 / 3.6)));
    assert_eq!(metric.generalized_cost(), Some(f64::INFINITY));
}
