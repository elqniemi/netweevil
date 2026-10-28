use super::*;
use netweevil_core::NodeId;

fn node(id: u32, lon: f64, lat: f64) -> TopologyNode {
    TopologyNode {
        node_id: NodeId(id),
        lon,
        lat,
        z: 0.0,
    }
}

#[test]
fn nodes_receive_interned_iana_zones_across_a_border() {
    let nodes = [
        node(0, 6.5665, 53.2194), // Groningen
        node(1, 7.45, 53.23),     // Leer, Germany
        node(2, 6.60, 53.21),
        node(3, 114.17, 22.30), // Hong Kong
    ];
    let (zones, indexes) = assign_timezones(&nodes);
    assert_eq!(
        zones,
        ["Europe/Amsterdam", "Europe/Berlin", "Asia/Hong_Kong"]
    );
    assert_eq!(indexes, vec![0, 1, 0, 2]);
}

#[test]
fn an_empty_topology_has_no_zones() {
    assert_eq!(assign_timezones(&[]), (Vec::new(), Vec::new()));
}
