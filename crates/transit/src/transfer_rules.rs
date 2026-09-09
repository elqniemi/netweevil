use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use csv::StringRecord;

use crate::model::{TransitBundle, TransitConnection, TransitTransferRule, TransitTransferType};

pub(crate) const TIMED_TRANSFER_DIAGNOSTIC: &str = "GTFS timed transfers use scheduled departure times; vehicle holding guarantees are not simulated";

fn required(headers: &StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|header| header == name)
        .with_context(|| format!("missing GTFS column '{name}'"))
}

fn optional<'a>(headers: &StringRecord, record: &'a StringRecord, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .position(|header| header == name)
        .and_then(|index| record.get(index))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn stop_groups(raw: &str, stop_by_id: &HashMap<String, u32>) -> Result<HashMap<String, Vec<u32>>> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let id = required(&headers, "stop_id")?;
    let records = reader
        .records()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut groups = stop_by_id
        .iter()
        .map(|(id, index)| (id.clone(), vec![*index]))
        .collect::<HashMap<_, _>>();
    for record in &records {
        if optional(&headers, record, "location_type") == Some("1") {
            groups
                .entry(record.get(id).unwrap_or_default().to_string())
                .or_default();
        }
    }
    for record in &records {
        let Some(&stop_index) = stop_by_id.get(record.get(id).unwrap_or_default()) else {
            continue;
        };
        if let Some(parent) = optional(&headers, record, "parent_station") {
            groups
                .get_mut(parent)
                .with_context(|| format!("unknown parent_station '{parent}'"))?
                .push(stop_index);
        }
    }
    Ok(groups)
}

pub(crate) fn parse_transfer_rules(
    raw: &str,
    stops_raw: &str,
    trips_raw: &str,
    stop_by_id: &HashMap<String, u32>,
    route_by_id: &HashMap<String, u32>,
    trip_by_id: &HashMap<String, u32>,
) -> Result<Vec<TransitTransferRule>> {
    let groups = stop_groups(stops_raw, stop_by_id)?;
    let mut trip_reader = csv::Reader::from_reader(trips_raw.as_bytes());
    let trip_id_column = required(trip_reader.headers()?, "trip_id")?;
    let trip_route_column = required(trip_reader.headers()?, "route_id")?;
    let all_trip_routes = trip_reader
        .records()
        .map(|record| {
            let record = record?;
            Ok((
                record.get(trip_id_column).unwrap_or_default().to_string(),
                record
                    .get(trip_route_column)
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let transfer_type = required(&headers, "transfer_type")?;
    let mut rules = Vec::new();
    let mut scopes = HashSet::new();
    for record in reader.records() {
        let record = record?;
        let rule_type = match record.get(transfer_type).unwrap_or_default().trim() {
            "" | "0" => TransitTransferType::Recommended,
            "1" => TransitTransferType::Timed,
            "2" => TransitTransferType::MinimumTime,
            "3" => TransitTransferType::Forbidden,
            "4" | "5" => bail!(
                "GTFS in-seat transfer types 4 and 5 are not supported; linked vehicle trips require explicit continuation modelling"
            ),
            value => bail!("invalid GTFS transfer_type '{value}'"),
        };
        let from_stops = groups
            .get(
                optional(&headers, &record, "from_stop_id")
                    .context("transfers.txt requires from_stop_id for transfer types 0–3")?,
            )
            .context("transfers.txt references unknown from_stop_id")?;
        let to_stops = groups
            .get(
                optional(&headers, &record, "to_stop_id")
                    .context("transfers.txt requires to_stop_id for transfer types 0–3")?,
            )
            .context("transfers.txt references unknown to_stop_id")?;
        let mut route_indexes = [None; 2];
        let mut trip_indexes = [None; 2];
        let mut inactive = false;
        for (side, route_column, trip_column) in [
            (0, "from_route_id", "from_trip_id"),
            (1, "to_route_id", "to_trip_id"),
        ] {
            if let Some(route_id) = optional(&headers, &record, route_column) {
                route_indexes[side] =
                    Some(*route_by_id.get(route_id).with_context(|| {
                        format!("unknown transfer {route_column} '{route_id}'")
                    })?);
            }
            if let Some(trip_id) = optional(&headers, &record, trip_column) {
                let actual_route = all_trip_routes
                    .get(trip_id)
                    .with_context(|| format!("unknown transfer {trip_column} '{trip_id}'"))?;
                if optional(&headers, &record, route_column)
                    .is_some_and(|route_id| route_id != actual_route)
                {
                    bail!(
                        "transfer {trip_column} '{trip_id}' does not belong to its specified {route_column}"
                    );
                }
                let Some(&trip_index) = trip_by_id.get(trip_id) else {
                    inactive = true;
                    continue;
                };
                trip_indexes[side] = Some(trip_index);
                // A trip selector takes precedence over its route selector.
                route_indexes[side] = None;
            }
        }
        let minimum = optional(&headers, &record, "min_transfer_time");
        if rule_type == TransitTransferType::MinimumTime && minimum.is_none() {
            bail!("GTFS transfer_type 2 requires min_transfer_time");
        }
        let minimum = minimum
            .map(str::parse::<u32>)
            .transpose()
            .context("invalid min_transfer_time")?
            .unwrap_or_default();
        if inactive {
            continue;
        }
        for &from_stop_index in from_stops {
            for &to_stop_index in to_stops {
                let scope = (from_stop_index, to_stop_index, route_indexes, trip_indexes);
                if !scopes.insert(scope) {
                    bail!("duplicate or ambiguous GTFS transfer rule scope");
                }
                rules.push(TransitTransferRule {
                    from_stop_index,
                    to_stop_index,
                    from_route_index: route_indexes[0],
                    to_route_index: route_indexes[1],
                    from_trip_index: trip_indexes[0],
                    to_trip_index: trip_indexes[1],
                    rule_type,
                    min_transfer_time_s: minimum,
                });
            }
        }
    }
    Ok(rules)
}

impl TransitTransferRule {
    fn specificity(&self) -> u8 {
        let side = |trip: Option<u32>, route: Option<u32>| {
            if trip.is_some() {
                2
            } else if route.is_some() {
                1
            } else {
                0
            }
        };
        match (
            side(self.from_trip_index, self.from_route_index),
            side(self.to_trip_index, self.to_route_index),
        ) {
            (2, 2) => 6,
            (2, 1) | (1, 2) => 5,
            (2, 0) | (0, 2) => 4,
            (1, 1) => 3,
            (1, 0) | (0, 1) => 2,
            _ => 1,
        }
    }

    fn matches(&self, from: &TransitConnection, to: &TransitConnection) -> bool {
        let side = |trip: Option<u32>, route: Option<u32>, connection: &TransitConnection| {
            trip.map_or_else(
                || route.is_none_or(|route| route == connection.route_index),
                |trip| trip == connection.trip_index,
            )
        };
        side(self.from_trip_index, self.from_route_index, from)
            && side(self.to_trip_index, self.to_route_index, to)
    }
}

pub(crate) struct TransferRuleIndex {
    by_stop_pair: HashMap<(u32, u32), Vec<usize>>,
    from_stops: HashSet<u32>,
    to_stops: HashSet<u32>,
}

impl TransferRuleIndex {
    pub(crate) fn new(bundle: &TransitBundle) -> Self {
        let mut by_stop_pair = HashMap::<_, Vec<_>>::new();
        let mut from_stops = HashSet::new();
        let mut to_stops = HashSet::new();
        for (index, rule) in bundle.transfer_rules.iter().enumerate() {
            by_stop_pair
                .entry((rule.from_stop_index, rule.to_stop_index))
                .or_default()
                .push(index);
            from_stops.insert(rule.from_stop_index);
            to_stops.insert(rule.to_stop_index);
        }
        for indexes in by_stop_pair.values_mut() {
            indexes.sort_by_key(|index| {
                std::cmp::Reverse(bundle.transfer_rules[*index].specificity())
            });
        }
        Self {
            by_stop_pair,
            from_stops,
            to_stops,
        }
    }

    pub(crate) fn has_origin(&self, stop_index: u32) -> bool {
        self.from_stops.contains(&stop_index)
    }

    pub(crate) fn has_destination(&self, stop_index: u32) -> bool {
        self.to_stops.contains(&stop_index)
    }

    pub(crate) fn allows(
        &self,
        bundle: &TransitBundle,
        from: &TransitConnection,
        to: &TransitConnection,
    ) -> Result<bool> {
        let Some(indexes) = self
            .by_stop_pair
            .get(&(from.to_stop_index, to.from_stop_index))
        else {
            return Ok(true);
        };
        let mut selected = None::<&TransitTransferRule>;
        for &index in indexes {
            let rule = &bundle.transfer_rules[index];
            if selected.is_some_and(|selected| selected.specificity() > rule.specificity()) {
                break;
            }
            if rule.matches(from, to) {
                if selected.is_some() {
                    bail!(
                        "equally specific GTFS transfer rules apply to trip pair {} -> {}",
                        bundle.trips[from.trip_index as usize].trip_id,
                        bundle.trips[to.trip_index as usize].trip_id
                    );
                }
                selected = Some(rule);
            }
        }
        Ok(selected.is_none_or(|rule| {
            rule.rule_type != TransitTransferType::Forbidden
                && to
                    .departure_s
                    .checked_sub(from.arrival_s)
                    .is_some_and(|gap| gap >= rule.min_transfer_time_s)
        }))
    }
}
