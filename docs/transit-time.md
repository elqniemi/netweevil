# Transit time and service days

GTFS imports require `agency.txt` with a valid IANA `agency_timezone`. Every agency in a feed must specify the same timezone. Stop-specific timezones do not change the interpretation of `stop_times.txt`.

NetWeevil converts each service day's timetable to real instants using the GTFS rule: find local noon in the agency timezone, subtract twelve elapsed hours, then add the GTFS stop time. Calendar days around a daylight-saving change can therefore have service-day origins 23 or 25 hours apart. Times above `24:00:00` remain attached to their original service day and can run beyond the last imported calendar date.

For example, an Amsterdam trip on October 24, 2026 with stop times `26:15:00` and `27:15:00` departs at `2026-10-25T02:15:00+02:00` and arrives at `2026-10-25T02:15:00+01:00`. Its travel time is one hour, although the two local clock readings match.

## Requests

Use RFC3339 datetimes with an explicit UTC offset when possible. `2026-03-29T07:59:00+02:00` and `2026-03-29T05:59:00Z` select the same instant.

A datetime without an offset is interpreted in the feed's agency timezone. Ambiguous times during an autumn clock change are rejected with a request to supply an offset. Local times skipped by a spring clock change are also rejected. Datetimes use whole-second precision.

Depart-after, arrive-by, and service-area queries all use these rules. Overnight queries may refer to the day after the imported service window. Queries before the timetable's time origin are rejected; import an earlier service day to search earlier instants.

## Returned times

Route and service-area results include a `time_context` object:

```json
{
  "agency_timezone": "Europe/Amsterdam",
  "time_origin_unix_s": 1792792800
}
```

Every `departure_s` and `arrival_s` is an elapsed UTC-second offset from `time_origin_unix_s`:

```text
unix_timestamp = time_origin_unix_s + departure_s
```

Durations such as `travel_time_s` remain elapsed seconds. Do not divide returned times by 86,400 to infer a service date, or treat the remainder as a local wall-clock time. Rust callers can use `TransitTimeContext::datetime(seconds)` to produce an agency-local RFC3339 datetime with the correct UTC offset.

The origin is the earlier of the first service day's local midnight and its GTFS noon-minus-twelve-hours anchor. This also represents midnight queries on an autumn transition day whose GTFS anchor falls after midnight. API service metadata and imported-feed manifests expose the same timezone and origin.

Transit bundle schema is 5. Re-import GTFS feeds to regenerate bundles and feed manifests; earlier bundle layouts are rejected.

The time rules follow the [GTFS schedule reference](https://gtfs.org/documentation/schedule/reference/#field-types) and [agency timezone requirement](https://gtfs.org/documentation/schedule/reference/#agencytxt). Timezone conversion uses the IANA data bundled with [chrono-tz](https://docs.rs/chrono-tz/latest/chrono_tz/).
