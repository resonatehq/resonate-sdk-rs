//! Timeout processing — vendored from
//! `resonate/src/processing/processing_timeouts.rs` (the Resonate server).
//!
//! Only the synchronous `process_all_timeouts` entry point (and its schedule
//! helper) is kept. The background loop that drives it lives in
//! [`super::SqliteNetwork::start`]; the `metrics` counter increment is dropped.

use super::persistence::{Db, StorageResult};
use super::util;

/// Process all expired timeouts at the given time.
pub fn process_all_timeouts(db: &dyn Db, time: i64) -> StorageResult<()> {
    // Run the three tick CTE statements (promise timeouts, task retry, task lease)
    tracing::debug!(time = time, "Processing expired timeouts");
    db.process_timeouts(time)?;

    // Process expired schedules (application-level cron computation)
    process_schedule_timeouts(db, time)?;

    Ok(())
}

/// Process expired schedule timeouts.
fn process_schedule_timeouts(db: &dyn Db, time: i64) -> StorageResult<()> {
    let expired = db.get_expired_schedule_timeouts(time)?;

    for (schedule_id, fired_at) in &expired {
        let schedule = match db.schedule_get(schedule_id)? {
            Some(s) => s,
            None => continue,
        };

        let next_run_at = util::compute_next_cron(&schedule.cron, *fired_at);

        let mut promise_tags = schedule.promise_tags.clone();
        promise_tags.insert("resonate:schedule".to_string(), schedule_id.clone());

        let promise_id = schedule
            .promise_id
            .replace("{{.id}}", schedule_id)
            .replace("{{.timestamp}}", &fired_at.to_string());
        promise_tags.insert("resonate:origin".to_string(), promise_id.clone());
        promise_tags.insert("resonate:branch".to_string(), promise_id.clone());
        promise_tags.insert("resonate:parent".to_string(), promise_id.clone());
        promise_tags.insert("resonate:prefix".to_string(), promise_id.clone());

        match db.process_schedule_timeout(
            schedule_id,
            *fired_at,
            next_run_at,
            time,
            &promise_tags,
        )? {
            Some(_) => {
                tracing::info!(
                    schedule_id = %schedule_id,
                    fired_at = fired_at,
                    next_run_at = next_run_at,
                    "Schedule fired"
                );
            }
            None => {
                // Idempotency guard fired or schedule was deleted — skip.
            }
        }
    }

    Ok(())
}
