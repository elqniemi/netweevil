#!/usr/bin/env python3
"""Run full-data Hong Kong API examples and retain their responses as evidence."""
import argparse
import json
from pathlib import Path
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:8080')
    parser.add_argument('--roads-url', help='Optional hk_roads server, e.g. http://127.0.0.1:8081')
    parser.add_argument('--date', default='2026-09-10', help='A day inside the imported service window')
    parser.add_argument('--output-dir', type=Path, default=Path('.netweevil/hong-kong/verification'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    args.output_dir.mkdir(parents=True, exist_ok=True)

    def request(path, body=None, url=None):
        req = urllib.request.Request((url or args.url).rstrip('/') + path,
              data=json.dumps(body).encode() if body is not None else None,
              headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=300) as response:
            return json.load(response)

    health = request('/healthz')
    assert health['status'] == 'ok', health
    service = request('/readyz')
    assert service['dataset']['dataset_id'] == 'hk_pedestrian_3d', 'Load hk_pedestrian_3d first'
    args.output_dir.joinpath('service.json').write_text(json.dumps(service, indent=2) + '\n')
    summaries = []
    for name in ['walk', 'walk-corridor', 'mtr', 'mtr-interchange', 'mtr-sai-ying-pun', 'transit-directions', 'bus', 'tram', 'ferry'] + (['drive'] if args.roads_url else []):
        body = json.loads(root.joinpath('examples/hong-kong', name + '.json').read_text())
        transit = 'feed_id' in body
        if transit:
            body['request']['time']['datetime'] = args.date + 'T09:00:00+08:00'
        start = time.perf_counter()
        endpoint = '/v1/transit-directions' if name == 'transit-directions' else ('/v1/transit-route' if transit else '/v1/route')
        response = request(endpoint, body,
                           args.roads_url if name == 'drive' else None)
        elapsed = time.perf_counter() - start
        args.output_dir.joinpath(name + '-response.json').write_text(json.dumps(response) + '\n')
        result = response['result']
        assert result['outcome'] == ('scheduled' if transit else 'legal'), (name, result['outcome'])
        if not transit:
            assert not result.get('fallback_used'), name
        if name.startswith('mtr') or name == 'transit-directions':
            vehicle = [leg for leg in result['legs'] if leg['leg_type'] == 'transit']
            assert vehicle and all(leg.get('mode') == 'subway' for leg in vehicle), name
            assert all('rail:MTR-PLATFORM-' in leg['from_id'] for leg in vehicle), name
            walking = [leg for leg in result['legs'] if leg['leg_type'] in ('access', 'egress')]
            assert all(leg.get('network_path', {}).get('edge_path') for leg in walking), name
            assert any(p[2] < 0 for leg in result['legs'] for p in leg.get('geometry', [])), name
        if name in ('mtr-interchange', 'transit-directions'):
            assert any(leg['leg_type'] == 'transfer' and leg.get('network_path', {}).get('edge_path')
                       for leg in result['legs']), 'No routed station transfer'
        if name == 'transit-directions':
            directions = response['directions']
            assert [step['sequence'] for step in directions] == list(range(1, len(directions) + 1))
            kinds = {step['kind'] for step in directions}
            assert {'access', 'board', 'ride', 'alight', 'transfer', 'egress'} <= kinds, kinds
            assert sum(step['kind'] == 'board' for step in directions) == result['summary']['boarding_count']
            assert all(step['arrival_s'] >= step['departure_s'] for step in directions)
            assert all(step['departure_datetime'].endswith('+08:00') for step in directions)
            assert all(step.get('platform') for step in directions if step['kind'] == 'board')
        if name in ('bus', 'tram', 'ferry'):
            assert any(leg.get('mode') == name for leg in result['legs']), name
        summaries.append(dict(example=name, outcome=result['outcome'], elapsed_s=round(elapsed, 3), summary=result['summary']))
        print(f'{name}: {result["outcome"]}, {elapsed:.3f}s')
    args.output_dir.joinpath('smoke-summary.json').write_text(json.dumps(summaries, indent=2) + '\n')


if __name__ == '__main__':
    main()
