-- Initial schema for the Grammatic capture agent.

CREATE TABLE profiles (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    sex         TEXT NOT NULL CHECK (sex IN ('male', 'female')),
    height_cm   DOUBLE PRECISION NOT NULL CHECK (height_cm BETWEEN 30 AND 220),
    dob         DATE NOT NULL,
    weight_min  DOUBLE PRECISION,
    weight_max  DOUBLE PRECISION,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE measurements (
    id                 BIGSERIAL PRIMARY KEY,
    measured_at        TIMESTAMPTZ NOT NULL,
    clock_source       TEXT NOT NULL CHECK (clock_source IN ('scale', 'receiver')),
    received_at        TIMESTAMPTZ NOT NULL,
    weight_kg          DOUBLE PRECISION NOT NULL,
    impedance_ohm      INTEGER CHECK (impedance_ohm IS NULL OR impedance_ohm BETWEEN 1 AND 3000),
    profile_id         BIGINT REFERENCES profiles(id) ON DELETE SET NULL,
    unit               TEXT NOT NULL CHECK (unit IN ('kg', 'lbs', 'jin')),
    raw_frame          TEXT NOT NULL DEFAULT '',
    rssi               SMALLINT,
    bmi                DOUBLE PRECISION,
    bmr_kcal           DOUBLE PRECISION,
    visceral_fat       DOUBLE PRECISION,
    ideal_weight_kg    DOUBLE PRECISION,
    body_fat_pct       DOUBLE PRECISION,
    water_pct          DOUBLE PRECISION,
    bone_mass_kg       DOUBLE PRECISION,
    muscle_mass_kg     DOUBLE PRECISION,
    protein_pct        DOUBLE PRECISION,
    lean_body_mass_kg  DOUBLE PRECISION,
    metabolic_age      INTEGER,
    body_type          TEXT
);

-- Dedup: the scale re-broadcasts the identical final frame (same embedded
-- timestamp). measured_at is minute-exact for scale-clocked frames and
-- minute-truncated for receiver-clocked frames (see capture), so the key is
-- stable across re-broadcasts. impedance is nullable, so a plain UNIQUE
-- constraint would treat NULLs as distinct; use a COALESCE expression index.
CREATE UNIQUE INDEX measurements_dedup
    ON measurements (measured_at, weight_kg, COALESCE(impedance_ohm, -1));

CREATE INDEX measurements_profile_idx ON measurements (profile_id);
CREATE INDEX measurements_measured_at_idx ON measurements (measured_at);
