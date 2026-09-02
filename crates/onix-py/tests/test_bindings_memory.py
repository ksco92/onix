"""Peak-RSS regression guard for the direct compact-value build.

The bindings convert both inputs into `onix_core::Value` directly, with no
intermediate `serde_json::Value` tree, so the two input trees only ever exist
in the compact model. This test measures the incremental process RSS of
converting an ``api_payloads``-shaped fixture (the dict-heavy shape from
``perf/generate_fixtures.py``) and asserts it stays under a bound that the old
`serde_json::Value` intermediate — with its fixed-size empty ``BTreeMap`` node
slots and one key ``String`` per occurrence — would blow past.

Run in its own subprocess so the RSS reading is isolated from the rest of the
pytest process, and measured via ``ru_maxrss`` (the process peak), normalized
across macOS (bytes) and Linux (KiB).
"""

import json
import subprocess
import sys
import textwrap

# Measured on this fixture: the direct compact build's two-tree conversion
# overhead is ~35 MB for 10,000 records (stable to <1 MB across runs). The
# pre-migration `serde_json::Value` representation of this dict-heavy shape
# cost several times that (onix-core's own footprint test pins the
# compact-vs-serde ratio at >=3x, and higher on small-map-heavy data like the
# per-record `tags`/`flags`/`history`/`address` sub-dicts here), which would
# clear this bound with room to spare. The bound sits well above the compact
# measurement so ordinary allocator/platform noise cannot make it flaky.
_OVERHEAD_BOUND_MB = 90.0
_RECORD_COUNT = 10_000

# A plain (non-f) string: `__RECORD_COUNT__` is substituted below, and every
# brace here is a normal Python literal (the inner f-strings use single braces).
_SUBPROCESS_BODY = """
import json
import random
import resource
import sys

from deepdiff_rs import DeepDiff


def rss_mb():
    # macOS reports ru_maxrss in bytes; Linux in KiB.
    factor = 1 if sys.platform == "darwin" else 1024
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * factor / 1e6


def make_record(index, rng):
    return {
        "id": index,
        "uuid": f"{index:08x}-{rng.getrandbits(32):08x}-{rng.getrandbits(32):08x}",
        "name": f"user_{index:07d}",
        "email": f"user_{index:07d}@example.test",
        "active": rng.random() < 0.8,
        "score": round(rng.uniform(0, 100), 4),
        "tags": [{"tag": f"tag_{rng.randint(0, 999)}"} for _ in range(rng.randint(0, 5))],
        "address": {
            "street": f"{rng.randint(1, 9999)} Main St",
            "city": rng.choice(["Springfield", "Shelbyville", "Ogdenville", "Capital City"]),
            "state": rng.choice(["CA", "NY", "TX", "WA", "CO"]),
            "zip": f"{rng.randint(10000, 99999)}",
        },
        "created_at": f"2024-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}T00:00:00Z",
        "metadata": {
            "source": rng.choice(["web", "mobile", "api", "batch"]),
            "priority": rng.randint(0, 5),
            "flags": [{"value": rng.random() < 0.5} for _ in range(3)],
        },
        "history": [
            {"event": rng.choice(["created", "updated", "viewed"]),
             "at": "2024-01-01T00:00:00Z",
             "value": round(rng.uniform(0, 1), 4)}
            for _ in range(rng.randint(0, 3))
        ],
    }


rng = random.Random(1234)
payload = [make_record(i, rng) for i in range(__RECORD_COUNT__)]

# Diff the payload against itself: the report is empty, so the measured RSS
# delta is exactly the two compact input trees the conversion builds and holds
# during the diff -- the pure conversion overhead, with no report contribution.
before = rss_mb()
diff = DeepDiff(payload, payload)
after = rss_mb()
assert not bool(diff), "equal inputs must produce an empty report"

print(json.dumps({"overhead_mb": after - before, "records": __RECORD_COUNT__}))
"""


def test_api_payloads_conversion_overhead_is_bounded() -> None:
    body = textwrap.dedent(_SUBPROCESS_BODY).replace("__RECORD_COUNT__", str(_RECORD_COUNT))
    result = subprocess.run(
        [sys.executable, "-c", body],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr

    data = json.loads(result.stdout.strip().splitlines()[-1])
    overhead = data["overhead_mb"]
    assert overhead < _OVERHEAD_BOUND_MB, (
        f"compact conversion overhead {overhead:.1f} MB for {data['records']} "
        f"api_payloads-shaped records exceeds the {_OVERHEAD_BOUND_MB} MB bound "
        f"(a regression to the serde_json::Value intermediate would land here)"
    )
