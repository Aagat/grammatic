"""API smoke test. Creates and cleans only its own records in a disposable DB.
Run: python3 tests/dashboard_api.py http://127.0.0.1:8090
"""
import json
import sys
import uuid
from datetime import datetime, timezone, timedelta
from urllib.request import Request, urlopen
from urllib.error import HTTPError

base = sys.argv[1].rstrip('/')
def call(path, method='GET', body=None, expected=200):
    request = Request(base + '/api' + path, data=json.dumps(body).encode() if body is not None else None,
                      method=method, headers={'Content-Type': 'application/json'})
    try:
        response = urlopen(request)
    except HTTPError as error:
        response = error
    assert response.status == expected, (path, response.status, response.read().decode())
    data = response.read()
    return json.loads(data) if data else None

assert call('/health')['database'] == 'connected'
for route in ['/', '/measurements', '/profiles', '/settings', '/measurements/123']:
    with urlopen(base + route) as response:
        assert response.status == 200 and b'<div id="root"></div>' in response.read(), route
name = 'API test ' + uuid.uuid4().hex[:8]
p = dict(name=name, sex='male', height_cm=178, dob='1990-06-14', weight_min=None, weight_max=None)
pid = call('/profiles', 'POST', p)['id']
mid = None
try:
    call('/profiles', 'POST', p, 409)
    call('/profiles', 'POST', {**p, 'name': ' ', 'height_cm': 0}, 400)
    now = datetime.now(timezone.utc).replace(microsecond=0)
    m = dict(measured_at=(now-timedelta(days=1)).isoformat(), weight_kg=72.4, impedance_ohm=600, profile_id=pid)
    mid = call('/measurements', 'POST', m)['id']
    call('/measurements', 'POST', m, 409)
    def saved(): return next(row for row in call('/measurements') if row['id'] == mid)
    assert saved()['bmi'] is not None and saved()['body_fat_pct'] is not None
    old_bmi = saved()['bmi']
    call(f'/profiles/{pid}', 'PUT', {**p, 'height_cm': 165})
    assert saved()['bmi'] != old_bmi
    call('/measurements', 'POST', {**m, 'weight_kg': -1}, 400)
    call('/measurements', 'POST', {**m, 'impedance_ohm': 3001}, 400)
    call('/measurements', 'POST', {**m, 'measured_at': (now+timedelta(days=1)).isoformat()}, 400)
    call(f'/measurements/{mid}', 'PUT', {**m, 'profile_id': 999999999}, 400)
    assert saved()['profile_id'] == pid
    call(f'/measurements/{mid}', 'PUT', {**m, 'impedance_ohm': None})
    assert saved()['body_fat_pct'] is None and saved()['bmi'] is not None
    call(f'/measurements/{mid}', 'PUT', {**m, 'profile_id': None})
    assert saved()['bmi'] is None and saved()['profile_id'] is None
    call(f'/measurements/{mid}', 'PUT', m)
    call(f'/profiles/{pid}', 'DELETE', expected=204)
    assert saved()['profile_id'] is None and saved()['bmi'] is None
    pid = None
    call(f'/measurements/{mid}', 'DELETE', expected=204)
    call(f'/measurements/{mid}', 'DELETE', expected=404)
    mid = None
    call('/unknown', expected=404)
    print('PASS: CRUD, computed metrics, profile recompute, guest assignment, deletion, duplicates, validation, missing records')
finally:
    if mid: call(f'/measurements/{mid}', 'DELETE', expected=204)
    if pid: call(f'/profiles/{pid}', 'DELETE', expected=204)
