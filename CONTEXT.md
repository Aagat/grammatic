# Grammatic

Language for the Mi Body Composition Scale 2 capture agent: one scale, passive
Bluetooth advertisements, measurements recorded straight into Postgres with no
phone app and no cloud. The integrated dashboard browses, corrects and deletes measurements and manages
household profiles.

## Language

**Frame**:
The 13-byte body-composition payload the scale broadcasts inside every
advertisement while someone weighs themselves.
_Avoid_: packet, message, broadcast (the broadcast is the radio event; the frame is its payload)

**Measurement**:
One completed weigh-in as recorded: weight, optional impedance, who it belongs
to (or guest), when it was measured. The unit of data this agent produces.
_Avoid_: reading, sample, data point

**Stabilized frame**:
A frame the scale sends once the measurement is finished — the final,
authoritative weight. Only stabilized frames become measurements.
_Avoid_: final frame, settled frame

**Re-broadcast**:
The identical final frame the scale re-sends after a measurement. Re-broadcasts
collapse onto the original measurement; they never create a new one.
_Avoid_: duplicate, repeat (as nouns)

**Dedup key**:
The triple that identifies a measurement: measurement time, weight, impedance.
Two broadcasts sharing a key are the same measurement; a distinct weigh-in
always has a distinct key.
_Avoid_: dedup window (the rejected wall-clock approach)

**Scale clock**:
The timestamp embedded in the frame, kept by the scale itself (minute
resolution, no timezone). Trusted only when plausible and close to receive time.
_Avoid_: device time

**Receiver clock**:
The agent machine's receive time, used in place of the scale clock when the
scale clock is missing or implausible. Truncated to the minute so re-broadcasts
share one dedup key.
_Avoid_: fallback time

**Measurement date**:
The date that ages a profile for metric computation — the validated scale-clock
date, or the receive date when the scale clock isn't trusted. Pinned at capture
so replay and recompute are deterministic.
_Avoid_: today, current age

**Profile assignment**:
Matching a measurement to a person: exclusive per-profile weight windows are
the guardrails; overlapping windows break ties against each candidate's most
recent measurement strictly before the current one (weight first, impedance
second; strict `< measured_at` so replay is deterministic). Missing history,
ties, and thin margins stay guest.
_Avoid_: user matching, identification

**Guest**:
A measurement matching no window — or an overlap that history cannot resolve —
recorded without a profile. Ambiguity resolves to guest, never to a guess.
_Avoid_: unknown user, unassigned

**Capture**:
The pipeline from frame to stored measurement: parse, classify the clock,
assign a profile, compute metrics, record. The automatic measurement path.
_Avoid_: ingestion, processing

**Spool**:
The capped on-disk holding area for frames captured while the database is
unreachable, drained on reconnect.
_Avoid_: queue, buffer, cache

**Replay**:
Feeding recorded frames (spool or file) through capture again, as if just
received.
_Avoid_: reimport

**Recompute**:
Re-deriving the metric columns of already-stored measurements from current
profiles and formulas. Raw frames are retained precisely so recompute never
loses information.
_Avoid_: refresh, recalculate

**Correction**:
An explicit edit to a Measurement or household profile, including recomputation
of the affected metrics. A Correction preserves captured raw frames and receive
metadata; it does not repeat automatic Profile assignment.
_Avoid_: recapture, reassignment (for the entire edit)
