# GATT history pull recovers missed stabilized frames — never synthesizes from live frames

When live frames were seen but the stabilized frame never arrived (agent
restarted mid-weigh-in, advertisement missed), the agent asks the scale for
its stored history over GATT (Body Composition History characteristic
`0x2A2F`: `0x01 + device-id` size probe, `0x02` fetch, `0x03` stop,
`0x04 + device-id` ack) and feeds each 13-byte entry (byte-identical frame)
through the normal capture path (clock decision, profile assignment,
metrics, dedup, spool). History entries are stabilized records by
construction — the scale only stores finished weigh-ins — so the invariant
"only stabilized frames become measurements" is preserved.

## Considered options

- **Synthesize from live frames** (rejected): live frames are unstable
  mid-weigh-in weights; recording one would store a wrong weight with a
  wrong time under a plausible-looking row. No flag enables this — a pull
  that yields nothing records nothing.
- **History pull, manual + automatic** (chosen): an explicit
  `grammatic fetch-history` for the missed-weigh-in case, plus an opt-in
  fallback in `listen` (armed by live frames, fired after quiet on *raw*
  advertisement sightings + idle, disarmed by any stabilized outcome).
  Re-delivery is harmless (dedup collapses it); failures log + back off and
  never touch the spool's live bytes.

## Device-id choice

The scale tracks the history position per client device-id, so the agent
persists one random `u32` on disk (`[history] device_id_file`, default
`/var/lib/grammatic/device_id`) and reuses it across pulls: repeated pulls
only re-send new entries. A missing/unwritable file degrades to noise
(re-delivery), never to loss — dedup makes re-delivery collapse.

## Protocol status

Validated live on the XMTZC05HM (personal capture traces omitted):
`0x2A2F` present (WRITE + NOTIFY, bluer `notify()` works), `0x01 + u32 LE`
probe answered with count + id echo, `0x02` fetched 13-byte byte-identical
frames terminated by `0x03`, per-device-id position confirmed (re-pull with
the same id: count 0; fresh id would re-deliver; dedup collapses either
way). (Reverse-engineering docs describe a 12-byte entry variant without
the first control byte; this firmware sends 13 bytes.)
`auto_fetch` defaults on since the 2026-09-03/04 soak validated the
fallback end to end (plan step 9.5); `fetch-history` works regardless.

## Enrichment rule

The live stabilized frame carries no impedance of its own. When an
impedance-bearing record arrives for a minute+weight that already has a
weight-only row, the row is upgraded in place (impedance, profile, metrics,
unit, clock source, raw frame become the new record's; the original
`received_at`/`rssi` win as first observation). The mirror holds for the
reverse order: a weight-only frame after its impedance sibling collapses.
A conflicting non-NULL impedance stays distinct — odd evidence that must
not merge silently. The dedup triple is unchanged; enrichment is an
UPDATE-before-INSERT on the same key shape, not a key change.
