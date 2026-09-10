# Hong Kong example

Follow the [Hong Kong tutorial](../../docs/hong-kong-tutorial.md) to prepare the full data and start the frontend.

- `drive.json`: Central waterfront to Wan Chai on `hk_roads`.
- `network.json`: source 3D pedestrian links around Central and Admiralty.
- `walk-corridor.json`: Causeway Bay to Central; compare fastest walking with the shelter-weighted profile.
- `walk.json`: Central Exit J3 to Admiralty Exit B on the 3D pedestrian network.
- `mtr.json`: Tsuen Wan Exit C to Central Exit J3 restricted to MTR.
- `mtr-sai-ying-pun.json`: the reported northern origin to Sai Ying Pun, with automatic endpoint elevation.
- `mtr-interchange.json`: Tsuen Wan to Tai Koo with a routed station interchange.
- `bus.json`, `tram.json`, `ferry.json`: individual mode checks using source GTFS stops.
- `transit-directions.json`: MTR interchange with ordered walking, platform boarding and ride instructions via `/v1/transit-directions`.
- `transit.json`: Tsuen Wan Exit C to Central Exit J3, with network access, egress and transfers.

These are API request envelopes. Walking/MTR coordinates come from the separate CSDI-derived MTR exit catalogue; other transit fixtures use source GTFS stops. Transit departures use September 10, 2026 in Hong Kong time; update the date to match your imported service window.

Walking, transit access and station transfers default to `pedestrian_fastest_multilayer`. The optional shelter-weighted research profiles can choose longer covered passages. See the tutorial for the comparison and step-free option.

`transit-directions-step-free.json` uses the step-free fastest profile and Central Exit A. Submit it to `/v1/transit-directions` to reproduce the lift-based MTR itinerary. The ordinary MTR fixture ends at Exit J3, whose modeled escalator access does not satisfy the step-free profile.
