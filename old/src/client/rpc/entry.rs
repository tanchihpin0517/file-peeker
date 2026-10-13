use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::protocol::v1::{
    EntryKind as ProtocolEntryKind, EntryMetadata as ProtocolEntryMetadata, ListingEntry,
};

use crate::{DirectoryEntry, EntryKind, fs::EntryMetadata, host_path::HostPath};

/// Takes a selected-host name or path off the wire.
///
/// Names cross as an escaped form, so one that does not decode means the server is
/// not speaking this protocol — the same class of fault as an unknown entry kind, and
/// reported the same way.
pub(super) fn wire_path(escaped: String, what: &str) -> io::Result<HostPath> {
    HostPath::from_escaped(escaped).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned an incorrectly escaped {what}: {error}"),
        )
    })
}

pub(super) fn convert_entry(entry: ListingEntry) -> io::Result<DirectoryEntry> {
    let kind = ProtocolEntryKind::try_from(entry.kind).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned unknown entry kind {}", entry.kind),
        )
    })?;
    let kind = match kind {
        ProtocolEntryKind::Unspecified => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server returned an unspecified entry kind",
            ));
        }
        ProtocolEntryKind::File => EntryKind::File,
        ProtocolEntryKind::Directory => EntryKind::Directory,
        ProtocolEntryKind::Symlink => EntryKind::Symlink,
        ProtocolEntryKind::Other => EntryKind::Other,
    };
    Ok(DirectoryEntry {
        name: wire_path(entry.name, "entry name")?,
        kind,
        navigable: entry.navigable,
        metadata: entry.metadata.map(convert_metadata).transpose()?,
        defect: (!entry.defect.is_empty()).then_some(entry.defect),
    })
}

fn convert_metadata(metadata: ProtocolEntryMetadata) -> io::Result<EntryMetadata> {
    Ok(EntryMetadata {
        size: metadata.size,
        modified: join_unix_time(metadata.modified_unix_seconds, metadata.modified_nanos),
        unix_mode: metadata.unix_mode,
        symlink_target: (!metadata.symlink_target.is_empty())
            .then(|| wire_path(metadata.symlink_target, "symlink target"))
            .transpose()?,
    })
}

/// Rebuilds a time from whole seconds and a non-negative sub-second remainder.
///
/// `None` rather than an error for a value that cannot be represented: an odd
/// timestamp is a curiosity of one entry, and refusing the whole listing over one
/// would be the very behaviour this work removed.
fn join_unix_time(seconds: i64, nanos: u32) -> Option<SystemTime> {
    if nanos >= 1_000_000_000 {
        return None;
    }
    let offset = Duration::new(seconds.unsigned_abs(), 0);
    let base = if seconds < 0 {
        UNIX_EPOCH.checked_sub(offset)?
    } else {
        UNIX_EPOCH.checked_add(offset)?
    };
    base.checked_add(Duration::new(0, nanos))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{UNIX_EPOCH, join_unix_time};
    use crate::server::ops::split_unix_time;

    /// The two halves of the wire's time encoding are written in different modules —
    /// the server splits, the client joins — so this is the test that they agree.
    #[test]
    fn times_survive_the_round_trip_in_both_directions() {
        for offset in [0, 1, 999_999_999, 1_786_000_000_000_000_000] {
            let after = UNIX_EPOCH + Duration::from_nanos(offset);
            let (seconds, nanos) = split_unix_time(after);
            assert_eq!(
                join_unix_time(seconds, nanos),
                Some(after),
                "{offset}ns after"
            );

            let before = UNIX_EPOCH - Duration::from_nanos(offset);
            let (seconds, nanos) = split_unix_time(before);
            assert_eq!(
                join_unix_time(seconds, nanos),
                Some(before),
                "{offset}ns before"
            );
        }
    }

    /// A nanosecond field out of range is one entry's oddity, not a reason to refuse
    /// the listing it arrived in.
    #[test]
    fn an_impossible_remainder_reports_no_time_rather_than_failing() {
        assert_eq!(join_unix_time(0, 1_000_000_000), None);
    }
}
