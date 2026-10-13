//! Dynamic file segmentation.
//!
//! A download starts as one segment covering the whole file. Whenever a connection
//! is free (at start-up, or after finishing its own segment) it halves the segment
//! with the most bytes left and takes the second half. The connection that owned
//! the split segment stops when it reaches the new boundary. That way no connection
//! idles while another still has a large tail, and slow connections lose work to
//! fast ones automatically.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::types::SegmentState;

/// Split points are aligned to this many bytes when possible.
const ALIGN: u64 = 64 * 1024;

pub struct Segment {
    pub start: u64,
    range: Mutex<Cursor>,
    /// Whether a connection currently owns this segment.
    assigned: AtomicBool,
}

#[derive(Clone, Copy)]
struct Cursor {
    pos: u64,
    end: u64,
}

impl Segment {
    fn new(state: SegmentState) -> Self {
        Self {
            start: state.start,
            range: Mutex::new(Cursor {
                pos: state.pos,
                end: state.end,
            }),
            assigned: AtomicBool::new(false),
        }
    }

    pub fn state(&self) -> SegmentState {
        let c = *self.range.lock();
        SegmentState {
            start: self.start,
            pos: c.pos,
            end: c.end,
        }
    }

    pub fn pos(&self) -> u64 {
        self.range.lock().pos
    }

    pub fn is_complete(&self) -> bool {
        let c = self.range.lock();
        c.pos >= c.end
    }

    /// Writes as much of `data` as fits before the segment's end at the current
    /// position, and returns how many bytes were written. The cursor lock is held
    /// across the write so a concurrent split can never move `end` below bytes
    /// that are being written.
    pub fn write(&self, file: &File, data: &[u8]) -> io::Result<usize> {
        let mut c = self.range.lock();
        let room = c.end.saturating_sub(c.pos);
        let n = (data.len() as u64).min(room) as usize;
        if n > 0 {
            file.write_all_at(&data[..n], c.pos)?;
            c.pos += n as u64;
        }
        Ok(n)
    }

    /// Marks a segment of unknown length as finished at its current position.
    pub fn close_at_current(&self) -> u64 {
        let mut c = self.range.lock();
        c.end = c.pos;
        c.pos
    }

    /// Rewinds the segment to its start, discarding received bytes.
    pub fn reset(&self) {
        self.range.lock().pos = self.start;
    }

    pub fn release(&self) {
        self.assigned.store(false, Ordering::Release);
    }

    fn try_assign(&self) -> bool {
        self.assigned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub struct SegmentMap {
    segments: Mutex<Vec<Arc<Segment>>>,
    /// Splitting needs byte-range support and a known size.
    splittable: bool,
    min_split: u64,
}

impl SegmentMap {
    pub fn new(states: Vec<SegmentState>, splittable: bool, min_split: u64) -> Self {
        let mut segments: Vec<Arc<Segment>> =
            states.into_iter().map(|s| Arc::new(Segment::new(s))).collect();
        segments.sort_by_key(|s| s.start);
        Self {
            segments: Mutex::new(segments),
            splittable,
            min_split: min_split.max(1),
        }
    }

    pub fn snapshot(&self) -> Vec<SegmentState> {
        self.segments.lock().iter().map(|s| s.state()).collect()
    }

    pub fn downloaded(&self) -> u64 {
        self.segments.lock().iter().map(|s| s.state().downloaded()).sum()
    }

    pub fn is_complete(&self) -> bool {
        self.segments.lock().iter().all(|s| s.is_complete())
    }

    /// Claims the unowned, unfinished segment that starts at `pos`, if any.
    pub fn take_at(&self, pos: u64) -> Option<Arc<Segment>> {
        self.segments
            .lock()
            .iter()
            .find(|s| s.pos() == pos && !s.is_complete() && s.try_assign())
            .cloned()
    }

    /// Claims the first unowned, unfinished segment (left over from a previous run
    /// or from a connection that gave up).
    pub fn take_unassigned(&self) -> Option<Arc<Segment>> {
        self.segments
            .lock()
            .iter()
            .find(|s| !s.is_complete() && s.try_assign())
            .cloned()
    }

    /// Halves the segment with the most bytes left and returns the new second half,
    /// already claimed by the caller.
    pub fn split_largest(&self) -> Option<Arc<Segment>> {
        if !self.splittable {
            return None;
        }
        let mut segments = self.segments.lock();
        let (index, _) = segments
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.state().remaining()))
            .filter(|&(_, remaining)| remaining >= 2 * self.min_split)
            // On ties prefer the earliest segment, so the file fills front to back.
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;

        let victim = segments[index].clone();
        let new_segment = {
            let mut c = victim.range.lock();
            let remaining = c.end.saturating_sub(c.pos);
            if remaining < 2 * self.min_split {
                return None;
            }
            let mut mid = c.pos + remaining / 2;
            let aligned = mid - mid % ALIGN;
            if aligned >= c.pos + self.min_split {
                mid = aligned;
            }
            let new_segment = Arc::new(Segment::new(SegmentState {
                start: mid,
                pos: mid,
                end: c.end,
            }));
            c.end = mid;
            new_segment
        };
        new_segment.assigned.store(true, Ordering::Release);
        let insert_at = segments.partition_point(|s| s.start < new_segment.start);
        segments.insert(insert_at, new_segment.clone());
        Some(new_segment)
    }

    /// Next work for a free connection: leftovers first, then a fresh split.
    pub fn next_work(&self) -> Option<Arc<Segment>> {
        self.take_unassigned().or_else(|| self.split_largest())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    fn whole(size: u64) -> SegmentMap {
        SegmentMap::new(vec![SegmentState { start: 0, pos: 0, end: size }], true, MB)
    }

    fn assert_contiguous(states: &[SegmentState], size: u64) {
        assert_eq!(states.first().unwrap().start, 0);
        for pair in states.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "gap or overlap in {states:?}");
        }
        assert_eq!(states.last().unwrap().end, size);
    }

    #[test]
    fn splits_largest_remaining_in_half() {
        let map = whole(100 * MB);
        let first = map.take_unassigned().unwrap();
        let second = map.split_largest().unwrap();
        assert_eq!(first.state().end, second.start);
        assert_eq!(second.start, 50 * MB);
        let third = map.split_largest().unwrap();
        // Both halves have 50 MB left; the first one found is split.
        assert_eq!(third.start, 25 * MB);
        assert_contiguous(&map.snapshot(), 100 * MB);
    }

    #[test]
    fn split_accounts_for_progress() {
        let map = whole(100 * MB);
        let seg = map.take_unassigned().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = File::create(dir.path().join("f")).unwrap();
        file.set_len(100 * MB).unwrap();
        let chunk = vec![7u8; (60 * MB) as usize];
        assert_eq!(seg.write(&file, &chunk).unwrap(), chunk.len());
        let new = map.split_largest().unwrap();
        // 40 MB remain from 60 MB; the split lands halfway, at 80 MB.
        assert_eq!(new.start, 80 * MB);
        // Writes stop at the new boundary.
        let written = seg.write(&file, &chunk).unwrap();
        assert_eq!(written as u64, 20 * MB);
        assert!(seg.is_complete());
        assert_contiguous(&map.snapshot(), 100 * MB);
    }

    #[test]
    fn refuses_tiny_splits() {
        let map = whole(MB + MB / 2);
        map.take_unassigned().unwrap();
        assert!(map.split_largest().is_none());
    }

    #[test]
    fn unsplittable_maps_never_split() {
        let map = SegmentMap::new(vec![SegmentState { start: 0, pos: 0, end: 100 * MB }], false, MB);
        assert!(map.split_largest().is_none());
    }

    #[test]
    fn leftovers_are_claimed_once() {
        let map = SegmentMap::new(
            vec![
                SegmentState { start: 0, pos: 10, end: 10 },
                SegmentState { start: 10, pos: 15, end: 20 },
                SegmentState { start: 20, pos: 22, end: 30 },
            ],
            true,
            MB,
        );
        assert_eq!(map.take_unassigned().unwrap().start, 10);
        assert_eq!(map.take_unassigned().unwrap().start, 20);
        assert!(map.take_unassigned().is_none());
        assert_eq!(map.downloaded(), 10 + 5 + 2);
    }
}
