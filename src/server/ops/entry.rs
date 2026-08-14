use std::time::{SystemTime, UNIX_EPOCH};

use crate::fs::{DirectoryEntry, EntryKind, EntryMetadata};
use crate::protocol::v1::{
    EntryKind as ProtocolEntryKind, EntryMetadata as ProtocolEntryMetadata, ListingEntry,
};

pub(crate) fn convert_entry(entry: DirectoryEntry) -> ListingEntry {
    let kind = match entry.kind {
        EntryKind::File => ProtocolEntryKind::File,
        EntryKind::Directory => ProtocolEntryKind::Directory,
        EntryKind::Symlink => ProtocolEntryKind::Symlink,
        EntryKind::Other => ProtocolEntryKind::Other,
    };
    ListingEntry {
        name: entry.name.into_escaped(),
        kind: kind.into(),
        navigable: entry.navigable,
        metadata: entry.metadata.map(convert_metadata),
        defect: entry.defect.unwrap_or_default(),
    }
}

fn convert_metadata(metadata: EntryMetadata) -> ProtocolEntryMetadata {
    let (modified_unix_seconds, modified_nanos) = metadata.modified.map_or((0, 0), split_unix_time);
    ProtocolEntryMetadata {
        size: metadata.size,
        modified_unix_seconds,
        modified_nanos,
        unix_mode: metadata.unix_mode,
        symlink_target: metadata
            .symlink_target
            .map(crate::host_path::HostPath::into_escaped)
            .unwrap_or_default(),
    }
}

/// Splits a time into whole seconds and a non-negative sub-second remainder.
///
/// Seconds are signed because a modification time before 1970 is a real thing to
/// find, and the remainder stays non-negative so the pair always reads as
/// `seconds + nanos` rather than needing a sign rule of its own.
pub(crate) fn split_unix_time(time: SystemTime) -> (i64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => (
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            since.subsec_nanos(),
        ),
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            match before.subsec_nanos() {
                0 => (-seconds, 0),
                nanos => (-seconds - 1, 1_000_000_000 - nanos),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::split_unix_time;

    #[test]
    fn splits_times_after_the_epoch() {
        let time = UNIX_EPOCH + Duration::new(1_700_000_000, 250);
        assert_eq!(split_unix_time(time), (1_700_000_000, 250));
    }

    #[test]
    fn splits_the_epoch_itself() {
        assert_eq!(split_unix_time(UNIX_EPOCH), (0, 0));
    }

    /// A time before 1970 borrows a second so the remainder stays non-negative, which
    /// is what lets the pair be read as a sum without a sign rule for the nanos.
    #[test]
    fn splits_times_before_the_epoch_into_a_non_negative_remainder() {
        assert_eq!(
            split_unix_time(UNIX_EPOCH - Duration::new(1, 0)),
            (-1, 0),
            "a whole second before the epoch"
        );

        let (seconds, nanos) = split_unix_time(UNIX_EPOCH - Duration::new(0, 250));
        assert_eq!((seconds, nanos), (-1, 999_999_750));
        // Reading it back as seconds + nanos lands where it started.
        let rebuilt = UNIX_EPOCH - Duration::new(1, 0) + Duration::new(0, nanos);
        assert_eq!(rebuilt, UNIX_EPOCH - Duration::new(0, 250));
        let _ = SystemTime::now();
    }
}
