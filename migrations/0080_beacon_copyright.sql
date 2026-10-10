-- Copyright notices against an uploaded Beacon (docs/BEACONS.md). A case names a video or a
-- Beacon; an upheld Beacon case hides it and keeps its earlier state for a restoration.
ALTER TABLE copyright_cases ALTER COLUMN video_id DROP NOT NULL;
ALTER TABLE copyright_cases ADD COLUMN beacon_id text REFERENCES beacons(id);
ALTER TABLE copyright_cases ADD COLUMN beacon_restore jsonb;
ALTER TABLE copyright_cases ADD CONSTRAINT copyright_cases_target CHECK (num_nonnulls(video_id, beacon_id) = 1);
