-- Daily orders pay Engagement Valor too (Joe's decision, docs/PROGRESSION.md Question 3): the amount
-- and the channel it went to.
ALTER TABLE daily_orders ADD COLUMN ev integer;
ALTER TABLE daily_orders ADD COLUMN ev_channel text REFERENCES users(id) ON DELETE SET NULL;
