# Hong Kong examples

This folder contains API requests for the Hong Kong datasets. Follow the
[Hong Kong tutorial](../../docs/guides/hong-kong-tutorial.md) to prepare the
data and start the console.

| File | Contents |
|---|---|
| `drive.json` | Car route from the Central waterfront to Wan Chai on `hk_roads` |
| `network.json` | Source 3D pedestrian links around Central and Admiralty |
| `walk.json` | Walk from Central Exit J3 to Admiralty Exit B on the 3D pedestrian network |
| `walk-corridor.json` | Walk from Causeway Bay to Central. Compare the fastest walk with the shelter-weighted profile. |
| `mtr.json` | Tsuen Wan Exit C to Central Exit J3, MTR only |
| `mtr-sai-ying-pun.json` | Sha Tin to a street point in Sai Ying Pun, with automatic endpoint elevation |
| `mtr-interchange.json` | Tsuen Wan to Tai Koo with a routed station interchange |
| `bus.json`, `tram.json`, `ferry.json` | Checks of one mode each, from source GTFS stops |
| `transit.json` | Tsuen Wan Exit C to Central Exit J3, with network access, egress and transfers |
| `transit-directions.json` | MTR interchange with walk, platform boarding and ride instructions, for `/v1/transit-directions` |
| `transit-directions-step-free.json` | Step-free itinerary from Central Exit A, for `/v1/transit-directions` |

Each file is an API request envelope.

- The walk and MTR coordinates come from the CSDI MTR exit catalogue.
- The other transit files use source GTFS stops.
- Transit departures are on 10 September 2026, Hong Kong time. Change the date
  to a day in the service window of your imported feed.

Walks, transit access and station transfers use the
`pedestrian_fastest_multilayer` profile by default. The shelter-weighted
profiles can select longer covered passages. The tutorial compares the
profiles and describes the step-free option.

`transit-directions-step-free.json` uses the step-free fastest profile and
Central Exit A. Send it to `/v1/transit-directions` to get the MTR itinerary
with lifts. The `mtr.json` request ends at Exit J3. The modeled escalator
access of Exit J3 does not agree with the step-free profile.
