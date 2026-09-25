//! Scheduled bookmarks: which may run on a schedule and which one is due. The app
//! runs them while it is quiet (see its monitor), one at a time.

use std::time::Duration;

use tangul_assist::guard;
use tangul_data::local::Bookmark;

/// Whether a bookmark may run on a schedule: it is flagged, belongs to one database
/// and doesn't change data.
pub fn schedulable(bookmark: &Bookmark) -> bool {
    bookmark.scheduled
        && bookmark.database_id.is_some()
        && guard::not_reads(&bookmark.sql).is_empty()
}

/// The scheduled bookmark that waited longest past `interval` since its last run
/// (by hand or scheduled); one that never ran comes first.
pub fn due(
    bookmarks: &[Bookmark],
    last_run: impl Fn(&str) -> Option<i64>,
    now_ms: i64,
    interval: Duration,
) -> Option<&Bookmark> {
    let interval = i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
    bookmarks
        .iter()
        .filter(|b| schedulable(b))
        .map(|b| (last_run(&b.id), b))
        .filter(|(last, _)| last.is_none_or(|t| now_ms.saturating_sub(t) >= interval))
        .min_by_key(|(last, _)| last.unwrap_or(i64::MIN))
        .map(|(_, b)| b)
}

/// `"5 minutes"`, `"2 hours"`, `"1 day"`: the largest whole unit.
pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (n, unit) = if secs.is_multiple_of(86_400) {
        (secs / 86_400, "day")
    } else if secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else {
        (secs.div_ceil(60), "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(id: &str, sql: &str) -> Bookmark {
        Bookmark {
            id: id.into(),
            name: id.into(),
            sql: sql.into(),
            database_id: Some("db".into()),
            scheduled: true,
            ..Default::default()
        }
    }

    #[test]
    fn picks_the_longest_waiting_due_bookmark() {
        let hour = Duration::from_secs(3600);
        let list = vec![
            bookmark("recent", "select 1"),
            bookmark("old", "select 2"),
            bookmark("older", "select 3"),
        ];
        let last = |id: &str| match id {
            "recent" => Some(9_000_000),
            "old" => Some(5_000_000),
            "older" => Some(1_000_000),
            _ => None,
        };
        let now = 9_500_000;
        assert_eq!(due(&list, last, now, hour).unwrap().id, "older");
        // Nothing waited an hour yet.
        assert!(due(&list, |_| Some(now - 1000), now, hour).is_none());
        // Never ran: first.
        let mut with_new = list;
        with_new.push(bookmark("new", "select 4"));
        assert_eq!(due(&with_new, last, now, hour).unwrap().id, "new");
    }

    #[test]
    fn only_read_only_bookmarks_of_one_database() {
        let hour = Duration::from_secs(3600);
        let mut global = bookmark("global", "select 1");
        global.database_id = None;
        let mut off = bookmark("off", "select 1");
        off.scheduled = false;
        let list = vec![
            global,
            off,
            bookmark("writes", "delete from t where id = 1"),
            bookmark("reads", "select * from t"),
        ];
        assert_eq!(due(&list, |_| None, 0, hour).unwrap().id, "reads");
    }

    #[test]
    fn durations_in_words() {
        let d = |s| human_duration(Duration::from_secs(s));
        assert_eq!(d(300), "5 minutes");
        assert_eq!(d(3600), "1 hour");
        assert_eq!(d(2 * 86_400), "2 days");
        assert_eq!(d(90), "2 minutes");
    }
}
