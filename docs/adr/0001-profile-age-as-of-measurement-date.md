# Profile age is evaluated as of the measurement date

The metric formulas need the profile's age, and a profile's age depends on
*when* you ask. We pin it to the **measurement date** (the validated scale
clock's date, or the receive date when the scale clock isn't trusted) instead
of the compute date, so `replay` and `recompute --all` are deterministic: the
same raw frame always produces the same metric columns no matter when it is
(re)processed. Compute-time aging would silently rewrite historical
body-fat/metabolic-age values on every later recompute.

## Considered options

- **Compute-time age** (original behavior): simpler, no date threading through
  the pipeline. Rejected — recompute was non-idempotent, contradicting the
  spec's promise that retained raw frames mean nothing is lost to later
  formula/profile changes.
- **Measurement-date age** (chosen): one date threaded from the frame through
  the pipeline; recompute becomes idempotent. Slight cost: every metrics
  computation must know which measurement it belongs to.
